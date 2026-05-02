use tower_lsp_server::ls_types::*;
use crate::resolver;
use crate::type_system::{TypeFact, KnownType, SymbolicStub};
use crate::util::{extract_field_chain, is_ancestor_or_equal, node_text, LineIndex};
use crate::aggregation::WorkspaceAggregation;

/// Shared context for field-access diagnostic collection, avoiding
/// long parameter lists in the recursive walker.
struct FieldDiagCtx<'a> {
    source: &'a [u8],
    line_index: &'a LineIndex,
    uri: &'a Uri,
    scope_tree: &'a crate::scope::ScopeTree,
    index: &'a WorkspaceAggregation,
    diagnostics: &'a mut Vec<Diagnostic>,
    emmy_severity: Option<DiagnosticSeverity>,
    lua_error_severity: Option<DiagnosticSeverity>,
    lua_warn_severity: Option<DiagnosticSeverity>,
}

pub(super) fn check_field_access_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    scope_tree: &crate::scope::ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    emmy_severity: Option<DiagnosticSeverity>,
    lua_error_severity: Option<DiagnosticSeverity>,
    lua_warn_severity: Option<DiagnosticSeverity>,
    line_index: &LineIndex,
) {
    let mut ctx = FieldDiagCtx {
        source, line_index, uri, scope_tree, index, diagnostics,
        emmy_severity, lua_error_severity, lua_warn_severity,
    };
    let mut cursor = root.walk();
    collect_field_diagnostics(&mut cursor, &mut ctx);
}

/// Returns true if `node` is (or is any descendant of) the left-hand side
/// of an assignment statement. Walks ancestors so that chained LHS like
/// `a.b.c = 1` — where the outer node is `variable` and the inner
/// `field_expression` node (`a.b`) is not a direct child of `variable_list` —
/// is still recognized as an assignment target and skipped by field
/// diagnostics.
fn is_assignment_target(node: tree_sitter::Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "assignment_statement" {
            // `current` is always an ancestor of (or equal to) `node`,
            // so `is_ancestor_or_equal(left, node)` already covers the
            // `left == current` case.
            return parent
                .child_by_field_name("left")
                .is_some_and(|left| is_ancestor_or_equal(left, node));
        }
        current = parent;
    }
    false
}



fn collect_field_diagnostics(
    cursor: &mut tree_sitter::TreeCursor,
    ctx: &mut FieldDiagCtx,
) {
    let node = cursor.node();

    let is_dotted = matches!(node.kind(), "field_expression" | "variable")
        && node.child_by_field_name("object").is_some()
        && node.child_by_field_name("field").is_some();

    if is_dotted && !is_assignment_target(node) {
        if let Some((base_node, fields)) = extract_field_chain(node, ctx.source) {
            check_dotted_field(ctx, node, base_node, &fields);
        }
    }

    if node.kind() == "function_call" {
        if let (Some(callee), Some(method)) = (
            node.child_by_field_name("callee"),
            node.child_by_field_name("method"),
        ) {
            let base_fact = crate::type_inference::infer_node_type(
                callee,
                ctx.source,
                ctx.uri,
                ctx.scope_tree,
                ctx.index,
            );
            let field_name = node_text(method, ctx.source).to_string();
            let resolved_base = resolver::resolve_type(&base_fact, ctx.index);

            if let TypeFact::Known(KnownType::EmmyType(type_name)) = &resolved_base.type_fact {
                if let Some(severity) = ctx.emmy_severity {
                    let field_resolved = resolver::resolve_field_chain(
                        &resolved_base.type_fact,
                        std::slice::from_ref(&field_name),
                        ctx.index,
                    );
                    if field_resolved.type_fact == TypeFact::Unknown
                        && field_resolved.def_uri.is_none()
                    {
                        let qualified = format!("{}.{}", type_name, field_name);
                        if !ctx.index.global_shard.contains_key(&qualified) {
                            ctx.diagnostics.push(Diagnostic {
                                range: ctx.line_index.ts_node_to_range(method, ctx.source),
                                severity: Some(severity),
                                source: Some("mylua".to_string()),
                                message: format!(
                                    "Unknown field '{}' on type '{}'",
                                    field_name, type_name
                                ),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_field_diagnostics(cursor, ctx);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn check_dotted_field(
    ctx: &mut FieldDiagCtx,
    node: tree_sitter::Node,
    base_node: tree_sitter::Node,
    fields: &[String],
) {
    let Some(field_name) = fields.last() else {
        return;
    };
    let Some(field_node) = node.child_by_field_name("field") else {
        return;
    };

    let base_fact = crate::type_inference::infer_node_type(
        base_node,
        ctx.source,
        ctx.uri,
        ctx.scope_tree,
        ctx.index,
    );
    let resolved_base = if fields.len() == 1 {
        resolver::resolve_type(&base_fact, ctx.index)
    } else {
        resolver::resolve_field_chain_in_file(
            ctx.uri,
            &base_fact,
            &fields[..fields.len() - 1],
            ctx.index,
        )
    };

    match &resolved_base.type_fact {
        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            if let Some(severity) = ctx.emmy_severity {
                let field_resolved = resolver::resolve_field_chain(
                    &resolved_base.type_fact,
                    std::slice::from_ref(field_name),
                    ctx.index,
                );
                if field_resolved.type_fact == TypeFact::Unknown && field_resolved.def_uri.is_none() {
                    let qualified = format!("{}.{}", type_name, field_name);
                    if !ctx.index.global_shard.contains_key(&qualified) {
                        ctx.diagnostics.push(Diagnostic {
                            range: ctx.line_index.ts_node_to_range(field_node, ctx.source),
                            severity: Some(severity),
                            source: Some("mylua".to_string()),
                            message: format!(
                                "Unknown field '{}' on type '{}'",
                                field_name, type_name
                            ),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        TypeFact::Known(KnownType::Table(shape_id)) => {
            let table_uri = resolved_base.def_uri.as_ref().unwrap_or(ctx.uri);
            if let Some(summary) = ctx.index.summaries.get(table_uri) {
                if let Some(shape) = summary.table_shapes.get(shape_id) {
                    if !shape.fields.contains_key(field_name) {
                        let field_is_global = field_global_prefixes(
                            &base_fact,
                            base_node,
                            fields,
                            ctx.source,
                            ctx.index,
                        )
                            .iter()
                            .any(|prefix| {
                                ctx.index
                                    .global_shard
                                    .contains_key(&format!("{}.{}", prefix, field_name))
                            });
                        if !field_is_global {
                            let severity = if shape.is_closed {
                                ctx.lua_error_severity
                            } else {
                                ctx.lua_warn_severity
                            };
                            if let Some(sev) = severity {
                                ctx.diagnostics.push(Diagnostic {
                                    range: ctx.line_index.ts_node_to_range(field_node, ctx.source),
                                    severity: Some(sev),
                                    source: Some("mylua".to_string()),
                                    message: format!(
                                        "Unknown field '{}' on table",
                                        field_name
                                    ),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn field_global_prefixes(
    base_fact: &TypeFact,
    base_node: tree_sitter::Node,
    fields: &[String],
    source: &[u8],
    index: &WorkspaceAggregation,
) -> Vec<String> {
    let mut prefixes = Vec::new();

    if let Some(prefix) = semantic_global_prefix(base_fact, fields, index) {
        prefixes.push(prefix);
    }

    let mut segments = vec![node_text(base_node, source).to_string()];
    if fields.len() > 1 {
        segments.extend(fields[..fields.len() - 1].iter().cloned());
    }
    push_simple_prefix(&mut prefixes, segments.join("."));

    prefixes
}

fn semantic_global_prefix(
    base_fact: &TypeFact,
    fields: &[String],
    index: &WorkspaceAggregation,
) -> Option<String> {
    let base_name = match base_fact {
        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.clone()),
        TypeFact::Stub(SymbolicStub::RequireRef { module_path }) => {
            resolver::resolve_require_global_name(module_path, index)
        }
        _ => None,
    }?;

    let mut segments = vec![base_name];
    if fields.len() > 1 {
        segments.extend(fields[..fields.len() - 1].iter().cloned());
    }
    let prefix = segments.join(".");
    if is_simple_dotted_prefix(&prefix) {
        Some(prefix)
    } else {
        None
    }
}

fn push_simple_prefix(prefixes: &mut Vec<String>, prefix: String) {
    if is_simple_dotted_prefix(&prefix) && !prefixes.contains(&prefix) {
        prefixes.push(prefix);
    }
}

fn is_simple_dotted_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        && !prefix.starts_with('.')
        && !prefix.ends_with('.')
        && !prefix.contains("..")
}
