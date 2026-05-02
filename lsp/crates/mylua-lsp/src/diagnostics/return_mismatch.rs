use super::type_compat::{infer_return_literal_type, is_type_compatible};
use crate::type_system::{KnownType, TypeFact};
use crate::util::LineIndex;
use tower_lsp_server::ls_types::*;

/// Walk every function declaration / definition; when preceded by
/// `---@return` annotations, compare against every `return_statement`
/// reachable from the body (including nested `if`/`do`/`while`/`for`
/// / `repeat` blocks). Both count and literal types are checked when
/// statically resolvable.
pub(super) fn check_return_mismatch_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let mut functions: Vec<tree_sitter::Node> = Vec::new();
    collect_function_like_nodes(root, &mut functions);
    for fun in functions {
        inspect_function_returns(fun, source, diagnostics, severity, line_index);
    }
}

fn collect_function_like_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if matches!(
        node.kind(),
        "function_declaration" | "local_function_declaration" | "function_definition"
    ) {
        out.push(node);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_function_like_nodes(child, out);
        }
    }
}

fn inspect_function_returns(
    fun: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    // For `function_definition` (anonymous `local f = function() end`
    // or `Class.m = function() end`), the anchor statement (used to
    // locate preceding `---@return` comments) is the enclosing
    // `local_declaration` / `assignment_statement`. For the named
    // forms, the declaration node itself carries the comments.
    let anchor = match fun.kind() {
        "function_definition" => {
            crate::summary_builder::enclosing_statement_for_function_expr(fun).unwrap_or(fun)
        }
        _ => fun,
    };

    let emmy_text = crate::emmy::collect_preceding_comments(anchor, source).join("\n");
    let anns = crate::emmy::parse_emmy_comments(&emmy_text);
    let mut declared_types: Vec<TypeFact> = Vec::new();
    for ann in &anns {
        if let crate::emmy::EmmyAnnotation::Return { return_types, .. } = ann {
            for rt in return_types {
                declared_types.push(crate::emmy::emmy_type_to_fact(rt));
            }
            // All `@return` lines accumulate; each contributes one or
            // more declared types (per EmmyLua convention). A function
            // with `---@return number, string` followed by
            // `---@return Err` declares 3 total return positions.
        }
    }
    if declared_types.is_empty() {
        return;
    }

    let body = fun.child_by_field_name("body");
    let Some(body) = body else { return };

    let mut returns: Vec<tree_sitter::Node> = Vec::new();
    collect_return_statements(body, &mut returns);

    // A function with `@return` but no `return` anywhere in its body is
    // suspicious but often intentional (stub). Skip unless at least
    // one return is present — better to report concrete mismatches
    // than nag about stubs.
    if returns.is_empty() {
        return;
    }

    for ret in returns {
        inspect_single_return(
            ret,
            &declared_types,
            source,
            diagnostics,
            severity,
            line_index,
        );
    }
}

fn collect_return_statements<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if node.kind() == "return_statement" {
        out.push(node);
        return;
    }
    // Do NOT descend into nested functions — their own `return`
    // statements belong to them, not the outer function.
    if matches!(
        node.kind(),
        "function_declaration" | "local_function_declaration" | "function_definition"
    ) {
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_return_statements(child, out);
        }
    }
}

fn inspect_single_return(
    ret: tree_sitter::Node,
    declared_types: &[TypeFact],
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    // `return_statement` in our grammar is `'return' optional(expression_list)
    // optional(';')` — no `values` field. Find the `expression_list`
    // child directly; absence means a bare `return` (0 values).
    let values = (0..ret.named_child_count())
        .filter_map(|i| ret.named_child(i as u32))
        .find(|c| c.kind() == "expression_list");
    let actual_count = values.map(|v| v.named_child_count() as u32).unwrap_or(0);
    let declared_count = declared_types.len() as u32;
    let required_count = required_return_count(declared_types) as u32;

    // Lua multi-value expansion: a trailing `function_call` or
    // `vararg_expression` at the last return-value position expands
    // into N values at call time. Static count comparison isn't
    // meaningful then — skip count *and* type checks for those
    // returns to avoid flooding opt-in users with false positives
    // from idiomatic `return foo()` / `return ...`.
    if let Some(values) = values {
        if let Some(last) = values.named_child(values.named_child_count().saturating_sub(1) as u32)
        {
            if matches!(last.kind(), "function_call" | "vararg_expression") {
                return;
            }
        }
    }

    if actual_count < required_count || actual_count > declared_count {
        diagnostics.push(Diagnostic {
            range: line_index.ts_node_to_range(ret, source),
            severity: Some(severity),
            source: Some("mylua".to_string()),
            message: format!(
                "Return statement yields {} value(s), expected {}",
                actual_count, declared_count,
            ),
            ..Default::default()
        });
        return;
    }
    // Count matches — check literal types when resolvable.
    if let Some(values) = values {
        // Use a lightweight inference that mirrors
        // `infer_literal_type` but without summary access; we don't
        // have the per-file summary plumbed here and the walk is
        // already heuristic. Literal nodes cover the common cases.
        for (i, declared) in declared_types.iter().enumerate() {
            let Some(val) = values.named_child(i as u32) else {
                break;
            };
            let actual = infer_return_literal_type(val);
            if actual == TypeFact::Unknown {
                continue;
            }
            if !is_type_compatible(declared, &actual) {
                diagnostics.push(Diagnostic {
                    range: line_index.ts_node_to_range(val, source),
                    severity: Some(severity),
                    source: Some("mylua".to_string()),
                    message: format!(
                        "Return value {}: declared '{}', got '{}'",
                        i + 1,
                        declared,
                        actual,
                    ),
                    ..Default::default()
                });
            }
        }
    }
}

fn required_return_count(declared_types: &[TypeFact]) -> usize {
    declared_types
        .iter()
        .rposition(|declared| !is_optional_return_type(declared))
        .map(|idx| idx + 1)
        .unwrap_or(0)
}

fn is_optional_return_type(declared: &TypeFact) -> bool {
    match declared {
        TypeFact::Known(KnownType::Nil) => true,
        TypeFact::Union(parts) => parts.iter().any(is_optional_return_type),
        _ => false,
    }
}
