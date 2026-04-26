use tower_lsp_server::ls_types::*;
use crate::resolver;
use crate::type_system::{TypeFact, KnownType, SymbolicStub};
use crate::util::{is_ancestor_or_equal, node_text, LineIndex};
use crate::aggregation::WorkspaceAggregation;

/// Shared context for field-access diagnostic collection, avoiding
/// long parameter lists in the recursive walker.
struct FieldDiagCtx<'a> {
    source: &'a [u8],
    line_index: &'a LineIndex,
    uri: &'a Uri,
    index: &'a mut WorkspaceAggregation,
    diagnostics: &'a mut Vec<Diagnostic>,
    emmy_severity: Option<DiagnosticSeverity>,
    lua_error_severity: Option<DiagnosticSeverity>,
    lua_warn_severity: Option<DiagnosticSeverity>,
}

pub(super) fn check_field_access_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    diagnostics: &mut Vec<Diagnostic>,
    emmy_severity: Option<DiagnosticSeverity>,
    lua_error_severity: Option<DiagnosticSeverity>,
    lua_warn_severity: Option<DiagnosticSeverity>,
    line_index: &LineIndex,
) {
    let mut ctx = FieldDiagCtx {
        source, line_index, uri, index, diagnostics,
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
        if let (Some(object), Some(field)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("field"),
        ) {
let base_fact = crate::type_inference::infer_node_type(object, ctx.source, ctx.uri, ctx.index);
            let field_name = node_text(field, ctx.source).to_string();

            let global_prefix = match &base_fact {
                TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.clone()),
                TypeFact::Stub(SymbolicStub::RequireRef { module_path }) => {
                    resolver::resolve_require_global_name(module_path, ctx.index)
                }
                TypeFact::Known(KnownType::Table(_)) => {
                    let text = node_text(object, ctx.source);
                    if !text.is_empty()
                        && text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                        && !text.starts_with('.')
                        && !text.ends_with('.')
                        && !text.contains("..")
                    {
                        Some(text.to_string())
                    } else {
                        None
                    }
                }
                _ => None,
            };

            let resolved_base = resolver::resolve_type(&base_fact, ctx.index);
            match &resolved_base.type_fact {
                TypeFact::Known(KnownType::EmmyType(type_name)) => {
                    if let Some(severity) = ctx.emmy_severity {
                        let field_resolved = resolver::resolve_field_chain(
                            &resolved_base.type_fact,
                            std::slice::from_ref(&field_name),
                            ctx.index,
                        );
                        if field_resolved.type_fact == TypeFact::Unknown && field_resolved.def_uri.is_none() {
                            let qualified = format!("{}.{}", type_name, field_name);
                            if !ctx.index.global_shard.contains_key(&qualified) {
                                ctx.diagnostics.push(Diagnostic {
                                    range: ctx.line_index.ts_node_to_range(field, ctx.source),
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
                            if !shape.fields.contains_key(&field_name) {
                                let field_is_global = global_prefix
                                    .as_ref()
                                    .map(|prefix| {
                                        ctx.index
                                            .global_shard
                                            .contains_key(&format!("{}.{}", prefix, field_name))
                                    })
                                    .unwrap_or(false);
                                if !field_is_global {
                                    let severity = if shape.is_closed {
                                        ctx.lua_error_severity
                                    } else {
                                        ctx.lua_warn_severity
                                    };
                                    if let Some(sev) = severity {
                                        ctx.diagnostics.push(Diagnostic {
                                            range: ctx.line_index.ts_node_to_range(field, ctx.source),
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
