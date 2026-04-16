use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::resolver;
use crate::type_system::{TypeFact, SymbolicStub};
use crate::aggregation::WorkspaceAggregation;
use crate::util::{node_text, position_to_byte_offset, find_node_at_position};

pub fn goto_definition(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, doc.text.as_bytes());

    if let Some(def) = doc.scope_tree.resolve(byte_offset, name, uri) {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: def.uri,
            range: def.selection_range,
        }));
    }

    // Field expression goto: `obj.field` → resolve and jump to field definition
    // Handles both `field_expression` (RHS) and `variable` (LHS assignment) nodes.
    if let Some(parent) = ident_node.parent() {
        if parent.kind() == "field_expression" {
            if let Some(result) = goto_field_expression(parent, doc, uri, index) {
                return Some(result);
            }
        }
        if parent.kind() == "variable" {
            let var_text = node_text(parent, doc.text.as_bytes());
            if var_text.contains('.') {
                if let Some(result) = goto_dotted_variable(var_text, uri, index) {
                    return Some(result);
                }
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

    if let Some(target) = try_require_goto(doc, ident_node, index) {
        return Some(target);
    }

    if let Some(entries) = index.globals.get(name) {
        let locations: Vec<Location> = entries
            .iter()
            .map(|e| Location {
                uri: e.uri.clone(),
                range: e.selection_range.clone(),
            })
            .collect();

        if locations.len() == 1 {
            return Some(GotoDefinitionResponse::Scalar(locations.into_iter().next().unwrap()));
        } else if !locations.is_empty() {
            return Some(GotoDefinitionResponse::Array(locations));
        }
    }

    None
}

fn goto_dotted_variable(
    var_text: &str,
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    // Try each dot split point from right to left (prefer longest base).
    // For `A.B.C`, tries: base="A.B" fields=["C"], then base="A" fields=["B","C"].
    let dot_positions: Vec<usize> = var_text.match_indices('.').map(|(i, _)| i).collect();
    if dot_positions.is_empty() {
        return None;
    }

    for &pos in dot_positions.iter().rev() {
        let base_name = &var_text[..pos];
        let field_chain: Vec<String> = var_text[pos + 1..].split('.').map(|s| s.to_string()).collect();

        let base_fact = if let Some(summary) = index.summaries.get(uri) {
            if let Some(ltf) = summary.local_type_facts.get(base_name) {
                ltf.type_fact.clone()
            } else {
                TypeFact::Stub(SymbolicStub::GlobalRef { name: base_name.to_string() })
            }
        } else {
            TypeFact::Stub(SymbolicStub::GlobalRef { name: base_name.to_string() })
        };

        let resolved = resolver::resolve_field_chain(&base_fact, &field_chain, index);
        if let (Some(def_uri), Some(def_range)) = (resolved.def_uri, resolved.def_range) {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: def_uri,
                range: def_range,
            }));
        }
    }
    None
}

fn goto_field_expression(
    field_expr: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    let source = doc.text.as_bytes();
    let object = field_expr.child_by_field_name("object")?;
    let field = field_expr.child_by_field_name("field")?;
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
    let parent = ident_node.parent()?;
    if parent.kind() != "variable" {
        return None;
    }
    let decl = parent.parent()?;
    if decl.kind() != "local_declaration" {
        return None;
    }
    let values = decl.child_by_field_name("values")?;
    let first_val = values.named_child(0)?;
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

    let target_range = index.summaries.get(&target_uri)
        .and_then(|s| {
            s.global_contributions.first().map(|gc| gc.selection_range)
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
