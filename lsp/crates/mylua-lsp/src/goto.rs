use tower_lsp_server::ls_types::*;
use crate::config::GotoStrategy;
use crate::document::Document;
use crate::resolver;
use crate::aggregation::WorkspaceAggregation;
use crate::util::{node_text, position_to_byte_offset, find_node_at_position};

pub fn goto_definition(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
    strategy: &GotoStrategy,
) -> Option<GotoDefinitionResponse> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, doc.text.as_bytes());

    // If clicking on the LHS name of `local x = require("mod")`, prefer
    // jumping to the required module's `return` statement over resolving
    // to the (same) local declaration itself.
    if let Some(target) = try_require_goto(doc, ident_node, index) {
        return Some(target);
    }

    if let Some(def) = doc.scope_tree.resolve(byte_offset, name, uri) {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: def.uri,
            range: def.selection_range,
        }));
    }

    // Dotted-access goto: walk ancestors to find a `variable` /
    // `field_expression` node whose `field` is this identifier, then
    // resolve via the AST-driven infer chain (supports `a[1].b`,
    // `a:m().c`, `require("mod").field`, etc.).
    {
        let mut n = ident_node;
        for _ in 0..8 {
            if let Some(p) = n.parent() {
                if matches!(p.kind(), "variable" | "field_expression") {
                    let field_is_ident = p
                        .child_by_field_name("field")
                        .map(|f| f.id() == ident_node.id())
                        .unwrap_or(false);
                    if field_is_ident {
                        if let Some(result) = goto_variable_field(p, doc, uri, index) {
                            return Some(result);
                        }
                        break;
                    }
                }
                n = p;
            } else {
                break;
            }
        }
    }

    // Check if ident is a type name → jump to its definition
    if let Some(candidates) = index.type_shard.get(name) {
        if let Some(candidate) = candidates.first() {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: candidate.source_uri.clone(),
                range: candidate.range,
            }));
        }
    }

    if let Some(candidates) = index.global_shard.get(name) {
        let locations: Vec<Location> = candidates
            .iter()
            .map(|c| Location {
                uri: c.source_uri.clone(),
                range: c.selection_range,
            })
            .collect();

        if !locations.is_empty() {
            return Some(apply_goto_strategy(locations, strategy));
        }
    }

    None
}

/// AST-driven goto for a dotted access: the clicked identifier is the
/// `field` of `var_node`. Recursively infer the type of `object` and
/// resolve the field, jumping to the definition location if available.
fn goto_variable_field(
    var_node: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    let source = doc.text.as_bytes();
    let object = var_node.child_by_field_name("object")?;
    let field = var_node.child_by_field_name("field")?;
    let field_name = node_text(field, source).to_string();

    let base_fact = crate::hover::infer_node_type(object, source, uri, index);
    let resolved = resolver::resolve_field_chain(&base_fact, &[field_name], index);

    if let (Some(def_uri), Some(def_range)) = (resolved.def_uri, resolved.def_range) {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: def_uri,
            range: def_range,
        }));
    }

    None
}

fn try_require_goto(
    doc: &Document,
    ident_node: tree_sitter::Node,
    index: &WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    // Walk up to the enclosing local_declaration if the clicked identifier
    // is one of its LHS names (directly, or nested inside `name_list`).
    // Typical trees for `local m = require("mod")`:
    //   local_declaration
    //     names: <identifier "m">  OR  names: name_list -> identifier "m"
    //     values: expression_list -> function_call
    // Find the enclosing local_declaration and compute the identifier's
    // index among ONLY the `identifier` children of the names list — so
    // that non-identifier children like `<const>` / `<close>` attributes
    // in `local x <const>, y = require(...)` don't push `y`'s index past
    // the end of `values`.
    let mut p = ident_node.parent()?;
    let idx_in_names = if matches!(p.kind(), "name_list" | "attribute_name_list") {
        let list = p;
        p = p.parent()?;
        identifier_index_in_list(list, ident_node)?
    } else {
        0
    };
    if p.kind() != "local_declaration" {
        return None;
    }
    let values = p.child_by_field_name("values")?;
    let first_val = values.named_child(idx_in_names)?;
    if first_val.kind() != "function_call" {
        return None;
    }
    let callee = first_val.child_by_field_name("callee")?;
    let callee_text = node_text(callee, doc.text.as_bytes());
    if callee_text != "require" {
        return None;
    }
    let args = first_val.child_by_field_name("arguments")?;
    let arg = args.named_child(0)?;

    let module_path = extract_string_content(arg, doc.text.as_bytes())?;

    let target_uri = index.resolve_module_to_uri(&module_path)?;

    // Prefer the file-level `return` statement's range (what the require
    // expression actually evaluates to). Fall back to the first global
    // contribution's selection range, then to file start.
    let target_range = index.summaries.get(&target_uri)
        .and_then(|s| {
            s.module_return_range
                .or_else(|| s.global_contributions.first().map(|gc| gc.selection_range))
        })
        .unwrap_or_default();

    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: target_range,
    }))
}

fn extract_string_content(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    fn find_string_content(n: tree_sitter::Node, source: &[u8]) -> Option<String> {
        if n.kind().starts_with("short_string_content") {
            return Some(node_text(n, source).to_string());
        }
        for i in 0..n.named_child_count() {
            if let Some(child) = n.named_child(i as u32) {
                if let Some(s) = find_string_content(child, source) {
                    return Some(s);
                }
            }
        }
        None
    }

    if node.kind() == "expression_list" {
        if let Some(first) = node.named_child(0) {
            return find_string_content(first, source);
        }
    }
    find_string_content(node, source)
}

/// Return `target`'s position among the `identifier` children of `list`
/// (`name_list` or `attribute_name_list`), ignoring non-identifier children
/// like `<const>` / `<close>` attribute nodes so that downstream index
/// lookups into `values` stay aligned.
fn identifier_index_in_list(
    list: tree_sitter::Node,
    target: tree_sitter::Node,
) -> Option<u32> {
    let mut id_idx: u32 = 0;
    for i in 0..list.named_child_count() {
        if let Some(c) = list.named_child(i as u32) {
            if c.kind() == "identifier" {
                if c.id() == target.id() {
                    return Some(id_idx);
                }
                id_idx += 1;
            }
        }
    }
    None
}

fn apply_goto_strategy(
    locations: Vec<Location>,
    strategy: &GotoStrategy,
) -> GotoDefinitionResponse {
    match strategy {
        GotoStrategy::Single => {
            GotoDefinitionResponse::Scalar(locations.into_iter().next().unwrap())
        }
        GotoStrategy::List => {
            GotoDefinitionResponse::Array(locations)
        }
        GotoStrategy::Auto => {
            if locations.len() == 1 {
                GotoDefinitionResponse::Scalar(locations.into_iter().next().unwrap())
            } else {
                GotoDefinitionResponse::Array(locations)
            }
        }
    }
}
