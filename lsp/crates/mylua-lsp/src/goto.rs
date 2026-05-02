use tower_lsp_server::ls_types::*;
use crate::aggregation::WorkspaceAggregation;
use crate::config::GotoStrategy;
use crate::document::Document;
use crate::resolver;
use crate::type_system::{KnownType, SymbolicStub, TypeFact};
use crate::util::{node_text, find_node_at_position, walk_ancestors, extract_string_literal, extract_field_chain};

pub fn goto_definition(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceAggregation,
    strategy: &GotoStrategy,
) -> Option<GotoDefinitionResponse> {
    let byte_offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;
    if let Some(type_name) = crate::emmy::emmy_type_name_at_byte(doc.source(), byte_offset) {
        return type_definition_for_name(&type_name, index, strategy);
    }

    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, doc.source());
    lsp_log!("[goto] ident='{}' kind='{}' parent='{}'", name, ident_node.kind(), ident_node.parent().map_or("none", |p| p.kind()));

    // If clicking on the LHS name of `local x = require("mod")`, prefer
    // jumping to the required module's `return` statement over resolving
    // to the (same) local declaration itself.
    if let Some(target) = try_require_goto(doc, ident_node, index) {
        return Some(target);
    }

    // Dotted-access goto: walk ancestors to find a `variable` /
    // `field_expression` node whose `field` is this identifier, then
    // resolve via the AST-driven infer chain (supports `a[1].b`,
    // `a:m().c`, `require("mod").field`, etc.).
    //
    // `walk_ancestors` caps depth to [`ANCESTOR_WALK_LIMIT`] and logs
    // a warning if ever hit — protects against runaway trees on
    // malformed input.
    if let Some(result) = walk_ancestors(ident_node, |p| {
        if matches!(p.kind(), "variable" | "field_expression") {
            let field_is_ident = p
                .child_by_field_name("field")
                .map(|f| f.id() == ident_node.id())
                .unwrap_or(false);
            if field_is_ident {
                return Some(goto_variable_field(p, doc, uri, index));
            }
        }
        // `obj:method(...)` — the `method` identifier on a `function_call`
        // node. Infer the base type and resolve the method as a field so
        // goto jumps to the method's definition site.
        if p.kind() == "function_call" {
            let method_is_ident = p
                .child_by_field_name("method")
                .map(|m| m.id() == ident_node.id())
                .unwrap_or(false);
            if method_is_ident {
                return Some(goto_method_call(p, doc, uri, index));
            }
        }
        None
    }) {
        // Found a dotted field / method context. If recursive field
        // resolution gives up, do not reinterpret the clicked field as a
        // standalone local/type/global name.
        return result;
    }

    if let Some(def) = doc.scope_tree.resolve(byte_offset, name, uri) {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: def.uri,
            range: def.selection_range.into(),
        }));
    }

    // Check if ident is a type name → jump to its definition
    if let Some(candidates) = index.type_shard.get(name) {
        if let Some(candidate) = candidates.first() {
            let candidate_uri = index.type_candidate_uri(candidate)?;
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: candidate_uri.clone(),
                range: candidate.range.into(),
            }));
        }
    }

    if let Some(candidates) = index.global_shard.get(name) {
        let locations: Vec<Location> = candidates
            .iter()
            .filter_map(|c| {
                let uri = index.candidate_uri(c)?;
                Some(Location {
                    uri: uri.clone(),
                    range: c.selection_range.into(),
                })
            })
            .collect();

        if !locations.is_empty() {
            return Some(apply_goto_strategy(locations, strategy));
        }
    }

    None
}

/// `textDocument/typeDefinition` — jump to the *type* of the symbol
/// at the cursor, rather than to its declaration.
///
/// For `---@type Foo local x = nil`, `goto_definition(x)` jumps to
/// `local x`; `goto_type_definition(x)` jumps to `@class Foo`.
///
/// Resolution order:
///
/// 1. Identifier at the cursor → scope-resolve to a local declaration
///    and read its type via `scope_tree.resolve_type(byte_offset, name)`.
/// 2. If the resolved `TypeFact` names an Emmy type (directly via
///    `Known(EmmyType(n))` / `Known(EmmyGeneric(n, _))` / `Stub(TypeRef{n})`,
///    or indirectly — a `Stub(GlobalRef)` that `resolve_type` chains
///    to an Emmy-named type), locate `type_shard[n]` and return its
///    candidate range.
/// 3. If no Emmy type name can be produced (type is `Number`, `String`,
///    a plain `Table`, etc.) fall back to plain `goto_definition`.
///
/// The fallback means typeDefinition behaves as a strict superset of
/// definition — VS Code's "Go to Type Definition" never lands on
/// nothing when Go-to-Definition would have worked.
pub fn goto_type_definition(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceAggregation,
    strategy: &GotoStrategy,
) -> Option<GotoDefinitionResponse> {
    let byte_offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;

    // Identifier AST path — click on a Lua identifier. Resolve to a
    // local declaration, then walk its stored `TypeFact` to an Emmy
    // type name via the resolver. If no Emmy type can be produced
    // (primitive, bare shape table, function, etc.), fall back to
    // `goto_definition` so the feature never goes silent.
    if let Some(ident_node) = find_node_at_position(doc.tree.root_node(), byte_offset) {
        let name = node_text(ident_node, doc.source());
        if let Some(def) = doc.scope_tree.resolve(byte_offset, name, uri) {
            if let Some(target) = type_definition_for_local(&def.uri, &def.name, byte_offset, &doc.scope_tree, index, strategy) {
                return Some(target);
            }
        }
        // Intentionally do NOT also query `type_shard[name]` here —
        // `name` is a variable name, not a type name; a coincidental
        // collision (`local Foo` in a workspace that also declares
        // `@class Foo`) must not jump us to the class. Fall back to
        // plain `goto_definition` instead.
        return goto_definition(doc, uri, position, index, strategy);
    }

    // Word-extraction fallback — cursor is inside an `emmy_line` text
    // blob (e.g. on `Foo` in `---@type Foo` / `---@class Bar : Foo`).
    // Here the word IS a type name by context; query `type_shard`
    // directly. If not found, returning None is correct (there's no
    // Lua-side definition to fall back to).
    let word = crate::references::extract_word_at(doc.text(), byte_offset)?;
    type_definition_for_name(&word, index, strategy)
}

/// Given a local variable's declaration URI + name, look up its
/// stored `TypeFact` via scope_tree and map it to a `type_shard` candidate range
/// when the fact identifies an Emmy type.
fn type_definition_for_local(
    _def_uri: &Uri,
    local_name: &str,
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
    strategy: &GotoStrategy,
) -> Option<GotoDefinitionResponse> {
    // Resolve the local's type via scope_tree
    let fact = if let Some(tf) = scope_tree.resolve_type(byte_offset, local_name) {
        tf.clone()
    } else {
        return None;
    };

    // Direct name: `---@type Foo local x = ...` stores Foo directly.
    // For `Known(_)` facts the resolver would just return the same
    // fact, so skip the redundant pass.
    if let Some(type_name) = type_name_of(&fact) {
        return type_definition_for_name(&type_name, index, strategy);
    }
    // Indirect: `local x = someGlobalReturningFoo()` / `require("mod")`
    // with Emmy module return type — let the resolver chase stubs
    // (GlobalRef / RequireRef / CallReturn) and try again on the
    // resolved fact.
    let resolved_fact = resolver::resolve_type(&fact, index).type_fact;
    if let Some(type_name) = type_name_of(&resolved_fact) {
        return type_definition_for_name(&type_name, index, strategy);
    }
    None
}

/// Pull an Emmy type name out of a `TypeFact` when the fact carries
/// one directly. Returns `None` for primitives / shape tables /
/// functions, and for stubs other than `TypeRef` (those need an
/// extra resolution step through the resolver).
fn type_name_of(fact: &TypeFact) -> Option<String> {
    match fact {
        TypeFact::Known(KnownType::EmmyType(n) | KnownType::EmmyGeneric(n, _)) => Some(n.clone()),
        TypeFact::Stub(SymbolicStub::TypeRef { name }) => Some(name.clone()),
        _ => None,
    }
}

fn type_definition_for_name(
    name: &str,
    index: &WorkspaceAggregation,
    strategy: &GotoStrategy,
) -> Option<GotoDefinitionResponse> {
    let candidates = index.type_shard.get(name)?;
    if candidates.is_empty() {
        return None;
    }
    let locations: Vec<Location> = candidates
        .iter()
        .filter_map(|c| {
            let uri = index.type_candidate_uri(c)?;
            Some(Location {
                uri: uri.clone(),
                range: c.range.into(),
            })
        })
        .collect();
    if locations.is_empty() {
        return None;
    }
    Some(apply_goto_strategy(locations, strategy))
}

/// AST-driven goto for a dotted access: the clicked identifier is the
/// `field` of `var_node`. Recursively infer the type of `object` and
/// resolve the field, jumping to the definition location if available.
fn goto_variable_field(
    var_node: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    let source = doc.source();
    if let Some((base_node, fields)) = extract_field_chain(var_node, source) {
        let base_fact =
            crate::type_inference::infer_node_type(base_node, source, uri, &doc.scope_tree, index);
        let resolved =
            resolver::resolve_field_chain_in_file(uri, &base_fact, &fields, index);
        return resolved_to_goto(resolved);
    }

    let base_node = var_node.child_by_field_name("object")?;
    let name_node = var_node.child_by_field_name("field")?;
    goto_field_or_method(base_node, name_node, source, uri, &doc.scope_tree, index)
}

/// AST-driven goto for a method call: `obj:method(...)`. The clicked
/// identifier is the `method` field of a `function_call` node. Infer
/// the type of the callee (the base object) and resolve the method
/// name as a field on that type.
fn goto_method_call(
    call_node: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    let source = doc.source();
    let base_node = call_node.child_by_field_name("callee")?;
    let name_node = call_node.child_by_field_name("method")?;
    goto_field_or_method(base_node, name_node, source, uri, &doc.scope_tree, index)
}

/// Shared goto helper for dotted field access (`a.b`) and method calls
/// (`obj:m()`). Infers the base type, resolves the field/method name,
/// and returns the definition location if available.
fn goto_field_or_method(
    base_node: tree_sitter::Node,
    name_node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<GotoDefinitionResponse> {
    let field_name = node_text(name_node, source).to_string();

    let base_fact =
        crate::type_inference::infer_node_type(base_node, source, uri, scope_tree, index);
    let resolved = resolver::resolve_field_chain_in_file(
        uri, &base_fact, &[field_name], index,
    );

    resolved_to_goto(resolved)
}

fn resolved_to_goto(
    resolved: resolver::ResolvedType,
) -> Option<GotoDefinitionResponse> {
    if let (Some(def_uri), Some(def_range)) = (resolved.def_uri, resolved.def_range) {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: def_uri.clone(),
            range: def_range.into(),
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
    let callee_text = node_text(callee, doc.source());
    if callee_text != "require" {
        return None;
    }
    let args = first_val.child_by_field_name("arguments")?;
    let arg = args.named_child(0)?;

    // Unwrap expression_list wrapper if present, then extract string content.
    let string_node = if arg.kind() == "expression_list" {
        arg.named_child(0)?
    } else {
        arg
    };
    let module_path = extract_string_literal(string_node, doc.source())?;

    let target_uri = index.resolve_module_to_uri(&module_path)?;

    // Prefer the file-level `return` statement's range (what the require
    // expression actually evaluates to). Fall back to the first global
    // contribution's selection range, then to file start.
    let target_range = index.summary(&target_uri)
        .and_then(|s| {
            s.module_return_range
                .or_else(|| s.global_contributions.first().map(|gc| gc.selection_range))
        })
        .map(|br| Range::from(br))
        .unwrap_or_default();

    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri,
        range: target_range,
    }))
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
