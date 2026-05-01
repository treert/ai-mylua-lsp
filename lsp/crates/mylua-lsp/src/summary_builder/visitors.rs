use crate::emmy::{collect_preceding_comments, parse_emmy_comments, emmy_type_to_fact, parse_type_from_str, EmmyAnnotation, EmmyType};
use crate::scope::{ScopeKind, ScopeDecl};
use crate::summary::*;
use crate::table_shape::{FieldInfo, TableShape};
use crate::type_system::*;
use crate::types::DefKind;
use crate::util::{node_text, extract_string_literal};

use super::BuildContext;
use super::emmy_visitors::{flush_pending_class, visit_emmy_comment};
use super::type_infer::infer_expression_type;
use super::fingerprint::{merge_types, hash_function_signature};

// ---------------------------------------------------------------------------
// Top-level visitor
// ---------------------------------------------------------------------------

pub(super) fn visit_top_level(ctx: &mut BuildContext, root: tree_sitter::Node) {
    ctx.push_scope(ScopeKind::File, root.start_byte(), root.end_byte());

    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        ctx.pop_scope();
        return;
    }
    loop {
        let node = cursor.node();
        match node.kind() {
            "local_declaration" => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                visit_local_declaration(ctx, node);
            }
            "local_function_declaration" => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                ctx.pending_class_name = None;
                ctx.pending_type_annotation = None;
                visit_local_function(ctx, node);
            }
            "function_declaration" => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                ctx.pending_class_name = None;
                ctx.pending_type_annotation = None;
                visit_function_declaration(ctx, node);
            }
            "assignment_statement" => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                visit_assignment(ctx, node);
            }
            "return_statement" => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                ctx.pending_class_name = None;
                ctx.pending_type_annotation = None;
                visit_module_return(ctx, node);
                visit_anonymous_function_definitions_in_node(ctx, node);
            }
            "emmy_comment" => {
                visit_emmy_comment(ctx, node);
                ctx.last_emmy_end_row = Some(node.end_position().row as u32);
            }
            "if_statement" | "do_statement" | "while_statement" | "repeat_statement"
            | "for_numeric_statement" | "for_generic_statement" => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                ctx.pending_class_name = None;
                ctx.pending_type_annotation = None;
                visit_nested_block(ctx, node);
            }
            _ => {
                clear_pending_on_blank_line_gap(ctx, node);
                flush_pending_class(ctx, node);
                ctx.pending_class_name = None;
                ctx.pending_type_annotation = None;
                visit_anonymous_function_definitions_in_node(ctx, node);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    ctx.pop_scope();
}

/// If there is a blank-line gap between the last `emmy_comment` and `node`,
/// discard pending annotation state so that `---@class` / `---@type` does
/// not leak across the gap.
fn clear_pending_on_blank_line_gap(ctx: &mut BuildContext, node: tree_sitter::Node) {
    if let Some(emmy_end_row) = ctx.last_emmy_end_row.take() {
        let node_start_row = node.start_position().row as u32;
        if node_start_row > emmy_end_row + 1 {
            // Blank line gap — drop all pending bindings.
            ctx.pending_class = None;
            ctx.pending_class_name = None;
            ctx.pending_type_annotation = None;
        }
    }
}

fn visit_module_return(ctx: &mut BuildContext, node: tree_sitter::Node) {
    ctx.module_return_range = Some(ctx.line_index.ts_node_to_byte_range(node, ctx.source));
    // Grammar: `return_statement = word_return, optional(expression_list), optional(';')`
    // The expression_list has no field name, so use `find_named_child_by_kind`.
    if let Some(values) = find_named_child_by_kind(node, "expression_list") {
        if let Some(first_expr) = values.named_child(0) {
            let type_fact = infer_expression_type(ctx, first_expr, 0);
            ctx.module_return_type = Some(type_fact);
        }
    }
}

/// Find the first named child of `node` whose kind matches `kind`.
/// Used when the grammar doesn't assign a field name to a child.
pub(super) fn find_named_child_by_kind<'a>(
    node: tree_sitter::Node<'a>,
    kind: &str,
) -> Option<tree_sitter::Node<'a>> {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            if child.kind() == kind {
                return Some(child);
            }
        }
    }
    None
}

fn visit_nested_block(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let mut return_types = None;
    visit_nested_block_inner(ctx, node, &mut return_types);
}

fn visit_nested_block_inner(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
    return_types: &mut Option<&mut Vec<TypeFact>>,
) {
    let scope_kind = match node.kind() {
        "do_statement" => Some(ScopeKind::DoBlock),
        "while_statement" => Some(ScopeKind::WhileBlock),
        "repeat_statement" => Some(ScopeKind::RepeatBlock),
        "if_statement" | "if_clause" => Some(ScopeKind::IfThenBlock),
        "elseif_clause" => Some(ScopeKind::ElseIfBlock),
        "else_clause" => Some(ScopeKind::ElseBlock),
        "for_numeric_statement" => Some(ScopeKind::ForNumeric),
        "for_generic_statement" => Some(ScopeKind::ForGeneric),
        _ => None,
    };
    if let Some(kind) = scope_kind {
        ctx.push_scope(kind, node.start_byte(), node.end_byte());
    }

    // Register for-loop variables into scope
    match node.kind() {
        "for_numeric_statement" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let db = name_node.start_byte();
                ctx.add_scoped_decl(ScopeDecl {
                    name: node_text(name_node, ctx.source).to_string(),
                    kind: DefKind::ForVariable,
                    decl_byte: db,
                    visible_after_byte: db,
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
                    type_fact: None,
                    bound_class: None,
                    is_emmy_annotated: false,
                });
            }
        }
        "for_generic_statement" => {
            if let Some(names_node) = node.child_by_field_name("names") {
                for i in 0..names_node.named_child_count() {
                    if let Some(id_node) = names_node.named_child(i as u32) {
                        if id_node.kind() == "identifier" {
                            let db = id_node.start_byte();
                            ctx.add_scoped_decl(ScopeDecl {
                                name: node_text(id_node, ctx.source).to_string(),
                                kind: DefKind::ForVariable,
                                decl_byte: db,
                                visible_after_byte: db,
                                range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                                selection_range: ctx.line_index.ts_node_to_byte_range(id_node, ctx.source),
                                type_fact: None,
                                bound_class: None,
                    is_emmy_annotated: false,
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        if scope_kind.is_some() { ctx.pop_scope(); }
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "block" | "if_clause" | "elseif_clause" | "else_clause"
            | "if_statement" | "do_statement" | "while_statement" | "repeat_statement"
            | "for_numeric_statement" | "for_generic_statement" => {
                clear_pending_on_blank_line_gap(ctx, child);
                visit_nested_block_inner(ctx, child, return_types);
            }
            "function_declaration" => {
                clear_pending_on_blank_line_gap(ctx, child);
                visit_function_declaration(ctx, child);
            }
            "assignment_statement" => {
                clear_pending_on_blank_line_gap(ctx, child);
                visit_assignment(ctx, child);
            }
            "local_declaration" => {
                clear_pending_on_blank_line_gap(ctx, child);
                visit_local_declaration(ctx, child);
            }
            "local_function_declaration" => {
                clear_pending_on_blank_line_gap(ctx, child);
                visit_local_function(ctx, child);
            }
            "return_statement" => {
                clear_pending_on_blank_line_gap(ctx, child);
                if let Some(returns) = return_types.as_deref_mut() {
                    collect_return_statement_types(ctx, child, returns);
                }
                visit_anonymous_function_definitions_in_node(ctx, child);
            }
            "emmy_comment" => {
                visit_emmy_comment(ctx, child);
                ctx.last_emmy_end_row = Some(child.end_position().row as u32);
            }
            _ => {
                clear_pending_on_blank_line_gap(ctx, child);
                visit_anonymous_function_definitions_in_node(ctx, child);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    if scope_kind.is_some() { ctx.pop_scope(); }
}

// ---------------------------------------------------------------------------
// local declarations: `local x = require("mod")` / `local x = expr`
// ---------------------------------------------------------------------------

fn visit_local_declaration(ctx: &mut BuildContext, node: tree_sitter::Node) {
    // Check for preceding `---@type` annotation (either from pending or inline comment)
    let pending_type = ctx.take_pending_type().or_else(|| {
        extract_preceding_type_annotation(node, ctx.source)
    });

    // Phase 2: consume pending_class_name — bind the first local to the class
    let mut pending_class = ctx.pending_class_name.take();

    let names_node = match node.child_by_field_name("names") {
        Some(n) => n,
        None => return,
    };
    let values_node = node.child_by_field_name("values");

    let name_count = names_node.named_child_count();

    // Multi-return distribution: `local a, b, c = f()` where the RHS
    // has exactly one value expression that is a `function_call`.
    // When we can statically infer the callee's return signature, map
    // `returns[i]` to `names[i]`; for every name beyond the signature's
    // return count we leave `Unknown`.
    let multi_return_types: Option<Vec<TypeFact>> = if name_count > 1 {
        values_node
            .and_then(single_function_call_rhs)
            .and_then(|call| extract_call_return_types(ctx, call))
    } else {
        None
    };

    for i in 0..name_count {
        let name_node = match names_node.named_child(i as u32) {
            Some(n) if n.kind() == "identifier" => n,
            _ => continue,
        };
        let name = node_text(name_node, ctx.source).to_string();
        let range = ctx.line_index.ts_node_to_byte_range(name_node, ctx.source);

        let value_node = values_node
            .and_then(|v| v.named_child(i as u32));
        // Phase 2: only the first local immediately following the class
        // comment gets the binding, even when it has no initializer.
        let var_bound_class = if i == 0 { pending_class.take() } else { None };

        // If we have an explicit @type annotation, it takes priority
        if i == 0 {
            if let Some(ref type_expr) = pending_type {
                let type_fact = emmy_type_to_fact(type_expr);
                ctx.add_scoped_decl(ScopeDecl {
                    name: name.clone(),
                    kind: DefKind::LocalVariable,
                    decl_byte: name_node.start_byte(),
                    visible_after_byte: node.end_byte(),
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: range,
                    type_fact: Some(type_fact),
                    bound_class: var_bound_class,
                    is_emmy_annotated: true,
                });
                continue;
            }
        }

        // Multi-return path takes priority when in effect: distribute
        // the i-th return type from the single RHS call. Names beyond
        // the signature's return arity stay Unknown.
        if let Some(ref returns) = multi_return_types {
            let type_fact = returns.get(i).cloned().unwrap_or(TypeFact::Unknown);
            ctx.add_scoped_decl(ScopeDecl {
                name: name.clone(),
                kind: DefKind::LocalVariable,
                decl_byte: name_node.start_byte(),
                visible_after_byte: node.end_byte(),
                range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                selection_range: range,
                type_fact: Some(type_fact),
                bound_class: var_bound_class,
                is_emmy_annotated: false,
            });
            continue;
        }

        if let Some(val) = value_node {
            if let Some(rb) = try_extract_require(ctx, &name, val) {
                ctx.require_bindings.push(rb);
                let require_type_fact = TypeFact::Stub(SymbolicStub::RequireRef {
                    module_path: ctx.require_bindings.last().unwrap().module_path.clone(),
                });
                ctx.add_scoped_decl(ScopeDecl {
                    name: name.clone(),
                    kind: DefKind::LocalVariable,
                    decl_byte: name_node.start_byte(),
                    visible_after_byte: node.end_byte(),
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: range,
                    type_fact: Some(require_type_fact),
                    bound_class: var_bound_class,
                    is_emmy_annotated: false,
                });
                continue;
            }


            let type_fact = infer_expression_type(ctx, val, 0);
            
            // If we have a pending class binding (from ---@class) and this is
            // the first local with no explicit @type annotation, and the inferred
            // type is a table, keep the table type so backfill can anchor the shape.
            // The bound_class is sufficient for method resolution. Otherwise use the
            // class type directly to make local classes consistent with global classes.
            let final_type_fact = if i == 0 && var_bound_class.is_some() && pending_type.is_none() {
                match &type_fact {
                    TypeFact::Known(KnownType::Table(_)) => {
                        // For table literals, keep the table type so backfill can anchor
                        // the shape. The bound_class is sufficient for method resolution.
                        type_fact.clone()
                    }
                    _ => {
                        // For non-table initializers, use the class type directly
                        let class_name = var_bound_class.clone().unwrap();
                        TypeFact::Known(KnownType::EmmyType(class_name))
                    }
                }
            } else {
                type_fact.clone()
            };
            // When the RHS is a literal table constructor, stamp
            // this binding name onto the freshly-allocated shape so
            // hover / signature_help can disambiguate same-named
            // methods across two shape tables in the same file.
            if let TypeFact::Known(KnownType::Table(shape_id)) = &type_fact {
                if let Some(shape) = ctx.table_shapes.get_mut(shape_id) {
                    shape.set_owner(name.clone());
                }
            }
            ctx.add_scoped_decl(ScopeDecl {
                name: name.clone(),
                kind: DefKind::LocalVariable,
                decl_byte: name_node.start_byte(),
                visible_after_byte: node.end_byte(),
                range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                selection_range: range,
                type_fact: Some(final_type_fact.clone()),
                bound_class: var_bound_class,
                is_emmy_annotated: false,
            });

            // Traverse anonymous function bodies for scope tree completeness
            if val.kind() == "function_definition" {
                if let Some(body) = val.child_by_field_name("body") {
                    let params = match &type_fact {
                        TypeFact::Known(KnownType::Function(sig)) => sig.params.clone(),
                        _ => Vec::new(),
                    };
                    visit_function_body(ctx, body, &params, false, "", None);
                }
            } else {
                visit_anonymous_function_definitions_in_node(ctx, val);
            }
            continue;
        }

        ctx.add_scoped_decl(ScopeDecl {
            name: name.clone(),
            kind: DefKind::LocalVariable,
            decl_byte: name_node.start_byte(),
            visible_after_byte: node.end_byte(),
            range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
            selection_range: range,
            type_fact: None,
            bound_class: var_bound_class,
            is_emmy_annotated: false,
        });
    }
}

/// True iff `values_node` (the `expression_list` on the RHS of a
/// `local_declaration` / `assignment_statement`) holds exactly one
/// expression and that expression is a `function_call`. Returns the
/// call node when it qualifies.
fn single_function_call_rhs(values: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if values.named_child_count() != 1 {
        return None;
    }
    let only = values.named_child(0)?;
    if only.kind() == "function_call" {
        Some(only)
    } else {
        None
    }
}

/// Try to statically extract the full list of return types for a
/// `function_call` node. Returns `None` when the callee's signature
/// isn't reachable at summary-build time (e.g. cross-file require,
/// method calls, field expressions) — callers should fall back to
/// leaving each name `Unknown` in that case rather than guessing.
///
/// Only the cheap, deterministic paths are handled here; more
/// elaborate multi-return analysis (e.g. following require to another
/// file's `module_return_type` → resolving multiple returns) is
/// intentionally deferred to avoid ballooning the summary pass.
fn extract_call_return_types(
    ctx: &BuildContext,
    call_node: tree_sitter::Node,
) -> Option<Vec<TypeFact>> {
    // Grammar: `function_call` with a `method` field is `obj:m(...)`.
    // The `callee` child is just `obj` in that shape, so relying on
    // the callee kind alone would wrongly treat `obj:m()` like `obj()`
    // when `obj` is also a top-level function registered in
    // `function_summaries`. Mirror `infer_call_return_type`'s method
    // branch and bail out unambiguously.
    if call_node.child_by_field_name("method").is_some() {
        return None;
    }
    let callee = call_node.child_by_field_name("callee")?;
    // Only bare identifier callees are statically resolvable here;
    // dotted calls (`mod.f()`) and subscript calls fall through the
    // resolver as CallReturn stubs without direct access to the full
    // returns list.
    if !matches!(callee.kind(), "variable" | "identifier") {
        return None;
    }
    let callee_text = node_text(callee, ctx.source);

    // Same-file function summary (covers `local function`, `function`,
    // `function Class:m`, etc.).
    if let Some(&func_id) = ctx.function_name_to_id.get(callee_text) {
        if let Some(fs) = ctx.function_summaries.get(&func_id) {
            return Some(fs.signature.returns.clone());
        }
    }
    None
}

/// Extract `---@type X` from a comment node immediately preceding the given node.
fn extract_preceding_type_annotation(node: tree_sitter::Node, source: &[u8]) -> Option<EmmyType> {
    let prev = node.prev_sibling()?;
    match prev.kind() {
        "emmy_comment" => {
            for i in 0..prev.named_child_count() {
                if let Some(line_node) = prev.named_child(i as u32) {
                    if line_node.kind() == "emmy_line" {
                        let text = node_text(line_node, source).trim();
                        if let Some(rest) = text.strip_prefix("---") {
                            let rest = rest.trim();
                            if let Some(rest) = rest.strip_prefix("@type") {
                                let type_text = rest.trim();
                                if !type_text.is_empty() {
                                    return Some(parse_type_from_str(type_text));
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        "comment" => {
            let text = node_text(prev, source).trim();
            if let Some(rest) = text.strip_prefix("---@type") {
                let type_text = rest.trim();
                if !type_text.is_empty() {
                    return Some(parse_type_from_str(type_text));
                }
            }
            None
        }
        _ => None,
    }
}

fn try_extract_require<'a>(
    ctx: &BuildContext<'a>,
    local_name: &str,
    value_node: tree_sitter::Node<'a>,
) -> Option<RequireBinding> {
    if value_node.kind() != "function_call" {
        return None;
    }
    let callee = value_node.child_by_field_name("callee")?;
    if node_text(callee, ctx.source) != "require" {
        return None;
    }
    let args = value_node.child_by_field_name("arguments")?;
    let first_arg = args.named_child(0)?;
    // Unwrap expression_list wrapper if present, then extract string content.
    let string_node = if first_arg.kind() == "expression_list" {
        first_arg.named_child(0)?
    } else {
        first_arg
    };
    let module_path = extract_string_literal(string_node, ctx.source)?;

    Some(RequireBinding {
        local_name: local_name.to_string(),
        module_path,
        range: ctx.line_index.ts_node_to_byte_range(value_node, ctx.source),
    })
}

// ---------------------------------------------------------------------------
// Function declarations
// ---------------------------------------------------------------------------

fn visit_local_function(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, ctx.source).to_string();
    let body = node.child_by_field_name("body");

    let func_id = ctx.alloc_function_id();
    let fs = build_function_summary(ctx, &name, node, body, false);
    let params = fs.signature.params.clone();
    let should_infer_returns = should_infer_body_returns(&fs);
    ctx.function_name_to_id.insert(name.clone(), func_id);
    ctx.function_summaries.insert(func_id, fs);

    ctx.add_scoped_decl(ScopeDecl {
        name: name.clone(),
        kind: DefKind::LocalFunction,
        decl_byte: name_node.start_byte(),
        visible_after_byte: name_node.start_byte(),
        range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
        selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
        type_fact: Some(TypeFact::Known(KnownType::FunctionRef(func_id))),
        bound_class: None,
        is_emmy_annotated: false,
    });

    // Traverse function body to populate scope tree with parameters and locals
    if let Some(b) = body {
        let mut returns = Vec::new();
        let return_target = if should_infer_returns { Some(&mut returns) } else { None };
        visit_function_body(ctx, b, &params, false, "", return_target);
        if should_infer_returns {
            update_function_returns(ctx, func_id, returns);
        }
    }
}

fn visit_function_declaration(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, ctx.source).to_string();
    let body = node.child_by_field_name("body");

    let func_id = ctx.alloc_function_id();
    let fs = build_function_summary(ctx, &name, node, body, false);
    let params = fs.signature.params.clone();
    let should_infer_returns = should_infer_body_returns(&fs);
    let is_method = name.contains(':');
    let class_prefix = crate::type_system::class_prefix_of(&name).to_string();
    ctx.function_name_to_id.insert(name.clone(), func_id);
    ctx.function_summaries.insert(func_id, fs);

    let local_bare_function = if !name.contains('.') && !name.contains(':') {
        if let Some(decl) = ctx.resolve_visible_in_build_scopes_mut(&name, name_node.start_byte()) {
            decl.type_fact = Some(TypeFact::Known(KnownType::FunctionRef(func_id)));
            true
        } else {
            false
        }
    } else {
        false
    };

    // Determine how the function is anchored:
    //   1. Local table shape (`local M = {}; function M:foo() ... end`)
    //      → write field to shape, skip global contribution (it's local)
    //   2. @class TypeDefinition (Phase 2: `---@class Cls; local M = ...; function M:foo()`)
    //      → write field to class TypeDefinition (supplemental — does NOT replace
    //        existing shape write or global contribution)
    //   3. Global contribution (fallback for non-local bases)
    let wrote_to_shape = 'shape: {
        let (base_name, field_name) = if let Some((b, f)) = name.rsplit_once(':') {
            (b, f)
        } else if let Some((b, f)) = name.rsplit_once('.') {
            (b, f)
        } else {
            break 'shape false;
        };

        if base_name.contains('.') || base_name.contains(':') {
            break 'shape false;
        }

        if let Some(decl) = ctx.resolve_visible_in_build_scopes(base_name, name_node.start_byte()) {
            if let Some(TypeFact::Known(KnownType::Table(shape_id))) = &decl.type_fact {
                let sid = *shape_id;
                if let Some(shape) = ctx.table_shapes.get_mut(&sid) {
                    shape.set_field(field_name.to_string(), FieldInfo {
                        name: field_name.to_string(),
                        type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
                        def_range: Some(ctx.line_index.ts_node_to_byte_range(name_node, ctx.source)),
                        assignment_count: 1,
                    });
                    break 'shape true;
                }
            }
        }
        false
    };

    let has_local_base = name
        .rsplit_once(':')
        .or_else(|| name.rsplit_once('.'))
        .map(|(base_name, _)| {
            !base_name.contains('.')
                && !base_name.contains(':')
                && ctx.resolve_visible_in_build_scopes(base_name, name_node.start_byte()).is_some()
        })
        .unwrap_or(false);

    // Phase 2: write method/field to @class TypeDefinition via bound_class.
    // This is supplemental — the global contribution still happens for non-local
    // bases, and the shape write still happens for local table shapes.
    if let Some((base_name, field_name)) = name.rsplit_once(':').or_else(|| name.rsplit_once('.')) {
        if !base_name.contains('.') && !base_name.contains(':') {
            if let Some(class_name) = ctx
                .resolve_bound_class_for_at(base_name, name_node.start_byte())
                .map(|s| s.to_string())
            {
                let def_range = ctx.line_index.ts_node_to_byte_range(name_node, ctx.source);
                let type_fact = TypeFact::Known(KnownType::FunctionRef(func_id));
                add_field_to_class(ctx, &class_name, field_name, type_fact, def_range);
            }
        }
    }

    // Traverse function body to populate scope tree with parameters and locals
    // (must happen before the early-return for local table shapes)
    if let Some(b) = body {
        let mut returns = Vec::new();
        let return_target = if should_infer_returns { Some(&mut returns) } else { None };
        visit_function_body(ctx, b, &params, is_method, &class_prefix, return_target);
        if should_infer_returns {
            update_function_returns(ctx, func_id, returns);
        }
    }

    // Bare `function f()` assigns an existing visible local `f` when one
    // exists; it is not a global definition in that case.
    if local_bare_function {
        return;
    }

    // Base is a local table → field already written to shape, no global.
    if wrote_to_shape {
        return;
    }

    // Base is a visible local but not a table shape. The declaration may
    // still have attached to an @class via bound_class above, but it must
    // not be exported through global_shard.
    if has_local_base {
        return;
    }

    // Global function: colon→dot normalization for both index and contribution.
    let normalized = name.replace(':', ".");
    ctx.function_name_index.insert(normalized.clone(), func_id);

    // Base is not a local (or bare name) → register as global contribution
    // (e.g. `function Player.new()` where Player is a global).
    ctx.global_contributions.push(GlobalContribution {
        name: normalized,
        kind: GlobalContributionKind::Function,
        type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
        range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
        selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
    });
}

/// Add a field to an `@class` TypeDefinition. Skips if:
/// - An `@field` annotation already declares a field with the same name
/// - A runtime field with the same name was already appended
fn add_field_to_class(
    ctx: &mut BuildContext,
    class_name: &str,
    field_name: &str,
    type_fact: TypeFact,
    def_range: crate::util::ByteRange,
) {
    if let Some(td) = ctx.type_definitions.iter_mut().find(|td| {
        td.kind == TypeDefinitionKind::Class && td.name == class_name
    }) {
        if td.fields.iter().any(|f| f.name == field_name) {
            return;
        }
        td.fields.push(TypeFieldDef {
            name: field_name.to_string(),
            type_fact,
            range: def_range,
            name_range: None,
        });
    }
}

fn build_function_summary(
    ctx: &mut BuildContext,
    name: &str,
    decl_node: tree_sitter::Node,
    body: Option<tree_sitter::Node>,
    infer_body_returns: bool,
) -> FunctionSummary {
    let emmy_comments = collect_preceding_comments(decl_node, ctx.source);
    let emmy_text = emmy_comments.join("\n");
    let annotations = parse_emmy_comments(&emmy_text);

    let mut params = Vec::new();
    let mut returns = Vec::new();
    let mut emmy_annotated = false;
    let mut overloads = Vec::new();
    let mut func_generic_params = Vec::new();

    for ann in &annotations {
        match ann {
            EmmyAnnotation::Param { name: pname, type_expr, .. } => {
                emmy_annotated = true;
                params.push(ParamInfo {
                    name: pname.clone(),
                    type_fact: emmy_type_to_fact(type_expr),
                });
            }
            EmmyAnnotation::Return { return_types, .. } => {
                emmy_annotated = true;
                for rt in return_types {
                    returns.push(emmy_type_to_fact(rt));
                }
            }
            EmmyAnnotation::Overload { fun_type } => {
                if let TypeFact::Known(KnownType::Function(sig)) = emmy_type_to_fact(fun_type) {
                    overloads.push(sig);
                }
            }
            EmmyAnnotation::Generic { params: gparams } => {
                for gp in gparams {
                    func_generic_params.push(gp.name.clone());
                }
            }
            _ => {}
        }
    }

    // If no Emmy params, extract from AST
    if params.is_empty() {
        if let Some(b) = body {
            if let Some(param_list) = b.child_by_field_name("parameters") {
                extract_ast_params(&mut params, param_list, ctx.source);
            }
        }
    }

    // If no Emmy return, try to infer from return statements
    if infer_body_returns && returns.is_empty() && !emmy_annotated {
        if let Some(b) = body {
            collect_return_types(ctx, b, &mut returns, 0);
        }
    }

    // `---@return self` / `---@param x self` on a method should
    // resolve to the enclosing class, e.g. for `function Foo:m()`
    // `self` becomes `Foo`. Derive the class prefix from the fully
    // qualified `name` (`Foo:m` → `Foo`, `Foo.m` → `Foo`, free
    // functions keep `self` untouched).
    let class_name = crate::type_system::class_prefix_of(name).to_string();
    if !class_name.is_empty() {
        for p in params.iter_mut() {
            p.type_fact = crate::type_system::substitute_self(&p.type_fact, &class_name);
        }
        for r in returns.iter_mut() {
            *r = crate::type_system::substitute_self(r, &class_name);
        }
        for ol in overloads.iter_mut() {
            for p in ol.params.iter_mut() {
                p.type_fact = crate::type_system::substitute_self(&p.type_fact, &class_name);
            }
            for r in ol.returns.iter_mut() {
                *r = crate::type_system::substitute_self(r, &class_name);
            }
        }
    }

    let sig = FunctionSignature {
        params: params.clone(),
        returns: returns.clone(),
    };
    let fingerprint = hash_function_signature(&sig);

    FunctionSummary {
        name: name.to_string(),
        signature: sig,
        range: ctx.line_index.ts_node_to_byte_range(decl_node, ctx.source),
        signature_fingerprint: fingerprint,
        emmy_annotated,
        overloads,
        generic_params: func_generic_params,
    }
}

fn should_infer_body_returns(fs: &FunctionSummary) -> bool {
    !fs.emmy_annotated && fs.signature.returns.is_empty()
}

fn update_function_returns(
    ctx: &mut BuildContext,
    func_id: FunctionSummaryId,
    returns: Vec<TypeFact>,
) {
    if returns.is_empty() {
        return;
    }
    if let Some(fs) = ctx.function_summaries.get_mut(&func_id) {
        fs.signature.returns = returns;
        fs.signature_fingerprint = hash_function_signature(&fs.signature);
    }
}

/// Return the enclosing `local_declaration` / `assignment_statement`
/// iff the `function_definition` at `node` is a **direct** RHS value
/// (i.e. the function expression is the sole content of a
/// value-list slot on an `expression_list` directly under such a
/// statement). Returns `None` for every nested / indirect case — table
/// entries (`{ m = function() end }`), function arguments
/// (`foo(function() end)`), IIFE wrappers
/// (`(function() end)()` — the RHS is the call, not the function
/// expression itself), arithmetic (`x = 1 + function() end()`),
/// nested function bodies, etc. — so we never pick up unrelated
/// outer Emmy annotations.
pub(crate) fn enclosing_statement_for_function_expr(
    node: tree_sitter::Node,
) -> Option<tree_sitter::Node> {
    // The parent of a direct-bound RHS function is always
    // `expression_list`; any other immediate parent means we're
    // inside a call / wrapper / table / nested context.
    let parent = node.parent()?;
    if parent.kind() != "expression_list" {
        return None;
    }
    let grandparent = parent.parent()?;
    match grandparent.kind() {
        "local_declaration" | "assignment_statement" => Some(grandparent),
        _ => None,
    }
}

pub(super) fn extract_ast_params(params: &mut Vec<ParamInfo>, param_list: tree_sitter::Node, source: &[u8]) {
    // Walk ALL children (named + unnamed) so we can pick up the
    // anonymous `...` token too: the grammar's `_parameter_list_content`
    // is inlined (leading `_`) and does NOT give the ellipsis its own
    // node kind, so `named_child_count` alone would silently drop
    // vararg params. Signal it by pushing a `ParamInfo { name: "...", .. }`.
    for i in 0..param_list.child_count() {
        let Some(child) = param_list.child(i as u32) else { continue };
        match child.kind() {
            "identifier" => {
                params.push(ParamInfo {
                    name: node_text(child, source).to_string(),
                    type_fact: TypeFact::Unknown,
                });
            }
            "name_list" => {
                for j in 0..child.named_child_count() {
                    if let Some(id) = child.named_child(j as u32) {
                        if id.kind() == "identifier" {
                            params.push(ParamInfo {
                                name: node_text(id, source).to_string(),
                                type_fact: TypeFact::Unknown,
                            });
                        }
                    }
                }
            }
            // Legacy explicit node name (if the grammar ever exposes
            // vararg as a named node again) or anonymous `...` token.
            "varargs" => {
                params.push(ParamInfo {
                    name: "...".to_string(),
                    type_fact: TypeFact::Unknown,
                });
            }
            _ => {
                // Anonymous `...` token in `function f(a, ...)`:
                // tree-sitter exposes it as an unnamed child whose
                // literal text is `...`.
                if !child.is_named() && node_text(child, source) == "..." {
                    params.push(ParamInfo {
                        name: "...".to_string(),
                        type_fact: TypeFact::Unknown,
                    });
                }
            }
        }
    }
}

pub(super) fn collect_return_types(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
    returns: &mut Vec<TypeFact>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "return_statement" => {
                collect_return_statement_types(ctx, child, returns);
            }
            // Don't recurse into nested function bodies
            "function_body" | "function_declaration" | "local_function_declaration" => {}
            _ => {
                collect_return_types(ctx, child, returns, depth + 1);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn collect_return_statement_types(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
    returns: &mut Vec<TypeFact>,
) {
    if let Some(values) = find_named_child_by_kind(node, "expression_list") {
        for i in 0..values.named_child_count() {
            if let Some(expr) = values.named_child(i as u32) {
                let tf = infer_expression_type(ctx, expr, 0);
                if returns.len() <= i {
                    returns.push(tf);
                } else {
                    let existing = returns[i].clone();
                    returns[i] = merge_types(existing, tf);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Assignment statements (globals)
// ---------------------------------------------------------------------------

fn visit_assignment(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let pending_type = ctx.take_pending_type().or_else(|| {
        extract_preceding_type_annotation(node, ctx.source)
    });

    // Phase 2: consume pending_class_name for global assignment binding
    let pending_class = ctx.pending_class_name.take();

    let left = match node.child_by_field_name("left") {
        Some(n) => n,
        None => return,
    };
    let right = node.child_by_field_name("right");

    for i in 0..left.named_child_count() {
        let var_node = match left.named_child(i as u32) {
            Some(n) => n,
            None => continue,
        };

        let value_node = right.and_then(|r| r.named_child(i as u32));
        if let Some(value) = value_node {
            visit_anonymous_function_definitions_in_node(ctx, value);
        }

        match var_node.kind() {
            // Simple global: `foo = expr`
            "variable" if var_node.child_count() == 1 => {
                let name = node_text(var_node, ctx.source).to_string();
                if ctx.resolve_visible_in_build_scopes(&name, var_node.start_byte()).is_some() {
                    continue;
                }

                // Phase 2: bind the first simple-identifier LHS to the class
                if i == 0 {
                    if let Some(ref class_name) = pending_class {
                        ctx.global_class_bindings.insert(name.clone(), class_name.clone());
                    }
                }

                let type_fact = if i == 0 {
                    if let Some(ref type_expr) = pending_type {
                        emmy_type_to_fact(type_expr)
                    } else if let Some(ref class_name) = pending_class {
                        TypeFact::Known(KnownType::EmmyType(class_name.clone()))
                    } else {
                        value_node
                            .map(|v| infer_expression_type(ctx, v, 0))
                            .unwrap_or(TypeFact::Unknown)
                    }
                } else {
                    value_node
                        .map(|v| infer_expression_type(ctx, v, 0))
                        .unwrap_or(TypeFact::Unknown)
                };

                // Same owner-stamping as `visit_local_declaration`:
                // anchor the binding name onto the literal table's
                // shape when the RHS is `{ ... }`.
                if let TypeFact::Known(KnownType::Table(shape_id)) = &type_fact {
                    if let Some(shape) = ctx.table_shapes.get_mut(shape_id) {
                        shape.set_owner(name.clone());
                    }
                }

                ctx.global_contributions.push(GlobalContribution {
                    name,
                    kind: GlobalContributionKind::Variable,
                    type_fact,
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: ctx.line_index.ts_node_to_byte_range(var_node, ctx.source),
                });
            }
            // Field assignment: `x.foo = expr` or `a.b.c = expr` etc.
            //
            // Strategy (AST-driven, not string splitn):
            //   1. Walk the nested `variable(object, field)` chain to collect
            //      field names outermost→innermost and reach the innermost
            //      bare identifier.
            //   2. If any intermediate node is not a pure dotted `variable`
            //      (e.g. `function_call`, subscript, parenthesized) → bail:
            //      the LHS targets a transient value that can't be named
            //      again. No shape write, no global contribution.
            //   3. Pure dotted chain:
            //      - Base is a local with Table shape → walk/create nested
            //        shapes for intermediate fields, then `set_field` on the
            //        innermost. Intermediate shape creation is on-demand.
            //      - Otherwise → legacy global TableExtension with the full
            //        dotted path as name.
            "field_expression" | "variable" => {
                let chain = match extract_dotted_chain(var_node, ctx.source) {
                    Some(c) if !c.fields.is_empty() => c,
                    _ => continue,
                };

                let type_fact = if i == 0 {
                    if let Some(ref type_expr) = pending_type {
                        emmy_type_to_fact(type_expr)
                    } else {
                        value_node
                            .map(|v| infer_expression_type(ctx, v, 0))
                            .unwrap_or(TypeFact::Unknown)
                    }
                } else {
                    value_node
                        .map(|v| infer_expression_type(ctx, v, 0))
                        .unwrap_or(TypeFact::Unknown)
                };

                let assign_range = ctx.line_index.ts_node_to_byte_range(node, ctx.source);
                if register_nested_field_write(
                    ctx,
                    &chain.base_name,
                    &chain.fields,
                    type_fact.clone(),
                    assign_range,
                ) {
                    continue;
                }

                // Phase 2: `self.field = expr` or `ABC.field = expr` where
                // the base has a bound_class → write field to @class.
                // This is supplemental — the global contribution still happens
                // for non-local bases (e.g. `Foo.bar = function() end` where
                // Foo is a global). The existing local/global check below
                // decides whether to emit a global contribution.
                if chain.fields.len() == 1 {
                    if let Some(class_name) = ctx
                        .resolve_bound_class_for_at(&chain.base_name, var_node.start_byte())
                        .map(|s| s.to_string())
                    {
                        let field_name = &chain.fields[0];
                        add_field_to_class(ctx, &class_name, field_name, type_fact.clone(), assign_range);
                    }
                }

                // `register_nested_field_write` returned `false` for one
                // of two reasons:
                //   (α) base isn't a local OR isn't a local with a Table
                //       shape → legacy TableExtension is appropriate
                //       (e.g. `GlobalTable.foo = 1`).
                //   (β) base IS a local Table shape but an intermediate
                //       field carries a non-Table type (e.g.
                //       `local a = {}; a.b = 1; a.b.c = 2`) → bail
                //       silently; writing to a non-Table via `.c` is
                //       a likely user bug and we MUST NOT surface the
                //       junk path through `global_shard` (the local `a`
                //       is not a global).
                if ctx
                    .resolve_visible_in_build_scopes(&chain.base_name, var_node.start_byte())
                    .is_some()
                {
                    continue;
                }

                let name = chain.joined();
                ctx.global_contributions.push(GlobalContribution {
                    name,
                    kind: GlobalContributionKind::TableExtension,
                    type_fact,
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: ctx.line_index.ts_node_to_byte_range(var_node, ctx.source),
                });
            }
            // Bracket index: `t[expr] = value` — mark shape open if key is dynamic
            "subscript_expression" => {
                if let Some(base) = var_node.child_by_field_name("object") {
                    let base_text = node_text(base, ctx.source);
                    if let Some(decl) = ctx.resolve_visible_in_build_scopes(base_text, base.start_byte()) {
                        if let Some(TypeFact::Known(KnownType::Table(shape_id))) = &decl.type_fact {
                            let sid = *shape_id;
                            if let Some(shape) = ctx.table_shapes.get_mut(&sid) {
                                shape.mark_open();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Dotted LHS helpers — AST-driven chain extraction & nested shape registration
// ---------------------------------------------------------------------------

/// Decomposition of a pure dotted LHS like `a.b.c` into the bare base name
/// and an ordered list of field names. Returned by `extract_dotted_chain`
/// only when the entire chain is made of `variable` nodes with an
/// `object + field` pair (outer levels) plus a single bare `identifier`
/// at the root. Any intermediate `function_call` / subscript /
/// `parenthesized_expression` → `None` (caller should bail on shape writes).
struct DottedChain {
    base_name: String,
    /// Field names in write order: for `a.b.c` this is `["b", "c"]`.
    fields: Vec<String>,
}

impl DottedChain {
    /// Full dotted path — `"a.b.c"`.
    fn joined(&self) -> String {
        let mut s = self.base_name.clone();
        for f in &self.fields {
            s.push('.');
            s.push_str(f);
        }
        s
    }
}

fn extract_dotted_chain(node: tree_sitter::Node, source: &[u8]) -> Option<DottedChain> {
    // Walk down the `object` chain, collecting field names. Every
    // intermediate node must itself be a `variable` (or legacy
    // `field_expression`) with an `object` + `field` pair. The innermost
    // `object` must be a `variable` whose only named child is a bare
    // `identifier` — i.e. the chain roots at a plain local/global name.
    //
    // Rejected (return None):
    //   - `foo().c`     → intermediate `function_call`
    //   - `a[1].c`      → `variable` whose `object` has an `index` field
    //                     instead of `field`
    //   - `(x).c`       → `parenthesized_expression` intermediate
    //   - `a:m().c`     → grammar wraps the method call as `function_call`
    let mut fields_rev: Vec<String> = Vec::new();
    let mut current = node;
    loop {
        if !matches!(current.kind(), "variable" | "field_expression") {
            return None;
        }
        let field = match current.child_by_field_name("field") {
            Some(f) => f,
            None => {
                // Innermost: a `variable` with a single `identifier` child.
                if current.named_child_count() == 1 {
                    let child = current.named_child(0)?;
                    if child.kind() == "identifier" {
                        let base_name = node_text(child, source).to_string();
                        fields_rev.reverse();
                        return Some(DottedChain {
                            base_name,
                            fields: fields_rev,
                        });
                    }
                }
                return None;
            }
        };
        let object = current.child_by_field_name("object")?;
        fields_rev.push(node_text(field, source).to_string());
        current = object;
    }
}

/// Register a field write `base.f1.f2...fn = value` against the local's
/// table shape, creating intermediate nested shapes on demand. Returns
/// `true` if the write was registered (the caller should skip legacy
/// global_contribution emission), `false` if the base isn't a local with
/// a Table shape (caller should fall through to legacy TableExtension).
fn register_nested_field_write(
    ctx: &mut BuildContext,
    base_name: &str,
    fields: &[String],
    type_fact: TypeFact,
    assign_range: crate::util::ByteRange,
) -> bool {
    let base_shape_id = match ctx.resolve_visible_in_build_scopes(base_name, assign_range.start_byte) {
        Some(decl) => match &decl.type_fact {
            Some(TypeFact::Known(KnownType::Table(sid))) => *sid,
            _ => return false,
        },
        None => return false,
    };

    // Walk the intermediate shapes. Three cases per step:
    //   (a) field exists as `Known(Table(sid))` → reuse existing shape.
    //   (b) field missing entirely → alloc a fresh shape + register it as
    //       the field's type on the parent (on-demand nesting).
    //   (c) field exists but is NOT a Table (e.g. `a.b = 1` then
    //       `a.b.c = 2`) → bail. Silently overwriting `a.b`'s number
    //       with a new Table shape would hide a likely user bug and
    //       mislead downstream hover/sig-help.
    //
    // Invariant (no orphan shapes on bail): shapes allocated inside this
    // loop are freshly created and therefore have empty `fields`, so
    // subsequent iterations can only observe `None` and keep allocating.
    // A `Some(non-Table) → return false` bail can therefore only trigger
    // on a pre-existing shape encountered *before* any allocation in
    // this call — guaranteeing we never leave an unreferenced shape
    // behind when we return `false`.
    let mut current_shape = base_shape_id;
    let last_idx = fields.len().saturating_sub(1);
    for (i, field_name) in fields.iter().enumerate() {
        if i == last_idx {
            break;
        }
        let existing_field = ctx.table_shapes.get(&current_shape)
            .and_then(|s| s.fields.get(field_name))
            .map(|fi| fi.type_fact.clone());
        let next_shape = match existing_field {
            Some(TypeFact::Known(KnownType::Table(sid))) => sid,
            Some(_) => return false,
            None => {
                let new_id = ctx.alloc_shape_id();
                ctx.table_shapes.insert(new_id, TableShape::new(new_id));
                if let Some(parent) = ctx.table_shapes.get_mut(&current_shape) {
                    parent.set_field(field_name.clone(), FieldInfo {
                        name: field_name.clone(),
                        type_fact: TypeFact::Known(KnownType::Table(new_id)),
                        def_range: Some(assign_range),
                        assignment_count: 1,
                    });
                }
                new_id
            }
        };
        current_shape = next_shape;
    }

    let final_field = match fields.last() {
        Some(f) => f.clone(),
        None => return false,
    };
    if let Some(shape) = ctx.table_shapes.get_mut(&current_shape) {
        shape.set_field(final_field.clone(), FieldInfo {
            name: final_field,
            type_fact,
            def_range: Some(assign_range),
            assignment_count: 1,
        });
        true
    } else {
        false
    }
}

fn visit_anonymous_function_definitions_in_node(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
) {
    if node.kind() == "function_definition" {
        visit_anonymous_function_definition(ctx, node);
        return;
    }

    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }

    loop {
        visit_anonymous_function_definitions_in_node(ctx, cursor.node());
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn visit_anonymous_function_definition(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
) {
    let Some(body) = node.child_by_field_name("body") else {
        return;
    };

    let mut params = Vec::new();
    if let Some(param_list) = body.child_by_field_name("parameters") {
        extract_ast_params(&mut params, param_list, ctx.source);
    }

    visit_function_body(ctx, body, &params, false, "", None);
}

// ---------------------------------------------------------------------------
// Function body scope traversal
// ---------------------------------------------------------------------------

/// Push a FunctionBody scope, register parameters + implicit self, then
/// recursively visit the body's statements. This populates the scope tree
/// with function-internal declarations so query-time `resolve_type` works
/// for locals inside functions.
fn visit_function_body(
    ctx: &mut BuildContext,
    func_body: tree_sitter::Node,
    params: &[ParamInfo],
    is_method: bool,
    class_prefix: &str,
    mut return_types: Option<&mut Vec<TypeFact>>,
) {
    ctx.push_scope(ScopeKind::FunctionBody, func_body.start_byte(), func_body.end_byte());

    // Register implicit self for colon methods (before explicit params)
    if is_method {
        let db = func_body.start_byte();

        // Phase 2: resolve bound_class for self. If the class_prefix
        // (e.g. "ABC" from "function ABC:method()") has a bound class,
        // use the class name as self's type and record bound_class.
        let (self_type, self_bound_class) = if !class_prefix.is_empty() {
            let bound = ctx
                .resolve_bound_class_for_at(class_prefix, func_body.start_byte())
                .map(|s| s.to_string());
            if let Some(ref class_name) = bound {
                (
                    Some(TypeFact::Known(KnownType::EmmyType(class_name.clone()))),
                    Some(class_name.clone()),
                )
            } else {
                (
                    Some(TypeFact::Known(KnownType::EmmyType(class_prefix.to_string()))),
                    None,
                )
            }
        } else {
            (None, None)
        };

        ctx.add_scoped_decl(ScopeDecl {
            name: "self".to_string(),
            kind: DefKind::Parameter,
            decl_byte: db,
            visible_after_byte: db,
            range: ctx.line_index.ts_node_to_byte_range(func_body, ctx.source),
            selection_range: ctx.line_index.ts_node_to_byte_range(func_body, ctx.source),
            type_fact: self_type,
            bound_class: self_bound_class,
            is_emmy_annotated: false,
        });
    }

    // Register parameters from the function_body's parameter list
    if let Some(param_list) = func_body.child_by_field_name("parameters") {
        register_params_into_scope(ctx, param_list, params);
    }

    // Recursively visit the body's statements. When requested, collect
    // return types during the walk so earlier local declarations are visible.
    visit_nested_block_inner(ctx, func_body, &mut return_types);

    ctx.pop_scope();
}

/// Register AST parameters into the current scope, enriching them with
/// Emmy type facts from `emmy_params` when available.
fn register_params_into_scope(
    ctx: &mut BuildContext,
    param_list: tree_sitter::Node,
    emmy_params: &[ParamInfo],
) {
    for i in 0..param_list.child_count() {
        let Some(child) = param_list.child(i as u32) else { continue };
        match child.kind() {
            "identifier" => {
                register_single_param(ctx, child, emmy_params);
            }
            "name_list" => {
                for j in 0..child.named_child_count() {
                    if let Some(id) = child.named_child(j as u32) {
                        if id.kind() == "identifier" {
                            register_single_param(ctx, id, emmy_params);
                        }
                    }
                }
            }
            "varargs" => {
                // `...` parameter — register as a vararg decl
                let db = child.start_byte();
                ctx.add_scoped_decl(ScopeDecl {
                    name: "...".to_string(),
                    kind: DefKind::Parameter,
                    decl_byte: db,
                    visible_after_byte: db,
                    range: ctx.line_index.ts_node_to_byte_range(child, ctx.source),
                    selection_range: ctx.line_index.ts_node_to_byte_range(child, ctx.source),
                    type_fact: emmy_params.iter()
                        .find(|p| p.name == "...")
                        .map(|p| p.type_fact.clone())
                        .filter(|tf| *tf != TypeFact::Unknown),
                    bound_class: None,
                    is_emmy_annotated: false,
                });
            }
            _ => {
                // Anonymous `...` token
                if !child.is_named() && node_text(child, ctx.source) == "..." {
                    let db = child.start_byte();
                    ctx.add_scoped_decl(ScopeDecl {
                        name: "...".to_string(),
                        kind: DefKind::Parameter,
                        decl_byte: db,
                        visible_after_byte: db,
                        range: ctx.line_index.ts_node_to_byte_range(child, ctx.source),
                        selection_range: ctx.line_index.ts_node_to_byte_range(child, ctx.source),
                        type_fact: emmy_params.iter()
                            .find(|p| p.name == "...")
                            .map(|p| p.type_fact.clone())
                            .filter(|tf| *tf != TypeFact::Unknown),
                        bound_class: None,
                    is_emmy_annotated: false,
                    });
                }
            }
        }
    }
}

/// Register a single named parameter into the current scope.
fn register_single_param(
    ctx: &mut BuildContext,
    id_node: tree_sitter::Node,
    emmy_params: &[ParamInfo],
) {
    let name = node_text(id_node, ctx.source).to_string();
    let type_fact = emmy_params.iter()
        .find(|p| p.name == name)
        .map(|p| p.type_fact.clone())
        .filter(|tf| *tf != TypeFact::Unknown);
    let db = id_node.start_byte();
    ctx.add_scoped_decl(ScopeDecl {
        name,
        kind: DefKind::Parameter,
        decl_byte: db,
        visible_after_byte: db,
        range: ctx.line_index.ts_node_to_byte_range(id_node, ctx.source),
        selection_range: ctx.line_index.ts_node_to_byte_range(id_node, ctx.source),
        type_fact,
        bound_class: None,
        is_emmy_annotated: false,
    });
}
