use super::type_compat::{infer_argument_type, is_type_compatible};
use crate::aggregation::WorkspaceAggregation;
use crate::type_system::TypeFact;
use crate::uri_id::UriId;
use crate::util::LineIndex;
use tower_lsp_server::ls_types::*;

/// Walk every `function_call` in the tree and compare actual argument
/// count (and, when types are knowable, types) against the resolved
/// callee's `FunctionSignature`s. `@overload` annotations produce
/// alternative signatures; if any one matches the call, no diagnostic
/// is emitted.
///
/// - `self` is implicit for `obj:method(...)` calls and is therefore
///   filtered out of the parameter list before counting.
/// - A vararg trailing param (`...`) absorbs any number of extra
///   arguments; only the required-arg minimum is enforced.
/// - Unknown-typed args (literal expression whose `infer_literal_type`
///   returns `Unknown`) suppress the type mismatch but do not suppress
///   the count check.
pub(super) fn check_call_argument_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    index: &WorkspaceAggregation,
    scope_tree: &crate::scope::ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    count_severity: Option<DiagnosticSeverity>,
    type_severity: Option<DiagnosticSeverity>,
    line_index: &LineIndex,
) {
    // Depth-first collection of call nodes; we have to collect up front
    // because `resolve_call_signatures` borrows `index` mutably and we
    // can't nest that inside a tree-sitter cursor walk that also owns
    // `root`.
    let mut calls: Vec<tree_sitter::Node> = Vec::new();
    collect_function_calls(root, &mut calls);

    for call in calls {
        let Some((sigs, is_method, display)) =
            crate::signature_help::resolve_call_signatures(call, source, uri_id, scope_tree, index)
        else {
            continue;
        };
        // After `resolve_call_signatures` returns, the `&mut index`
        // borrow ends (the returned values are owned). We can now
        // take an immutable summary reference for `uri` for
        // the type-check path without cloning a full DocumentSummary
        // on every call.
        if sigs.is_empty() {
            continue;
        }
        let Some(args_node) = call.child_by_field_name("arguments") else {
            continue;
        };
        let (actual_count, arg_exprs) = collect_call_arguments(args_node, source);

        // Count match: any overload compatible with the actual count
        // clears the diagnostic.
        if let Some(severity) = count_severity {
            let any_count_ok = sigs
                .iter()
                .any(|sig| signature_accepts_count(sig, actual_count, is_method));
            if !any_count_ok {
                // Use the smallest/largest expected count range across
                // overloads for the human-readable message.
                let (min_expected, max_expected) = expected_count_range(&sigs, is_method);
                let range = line_index.ts_node_to_range(args_node, source);
                let expected_desc = if min_expected == max_expected {
                    format!("{}", min_expected)
                } else if max_expected == u32::MAX {
                    format!("at least {}", min_expected)
                } else {
                    format!("{} to {}", min_expected, max_expected)
                };
                diagnostics.push(Diagnostic {
                    range,
                    severity: Some(severity),
                    source: Some("mylua".to_string()),
                    message: format!(
                        "Call to '{}' passes {} argument(s), expected {}",
                        display, actual_count, expected_desc,
                    ),
                    ..Default::default()
                });
                // Skip per-arg type checks when count is already wrong —
                // the positional pairing is ambiguous.
                continue;
            }
        }

        // Type match: only when a suitable summary is available (local
        // file) to evaluate argument literal types. For each positional
        // arg i, check against the best matching overload's param i.
        // A single "any overload matches" check keeps behavior
        // consistent with the count pass.
        if let Some(severity) = type_severity {
            // Find the first overload whose count is compatible; use
            // its param slots for typing. If multiple overloads match,
            // prefer the one whose param types align most with the
            // provided literal types (best-effort, non-critical).
            let Some(best_sig) =
                pick_best_typing_overload(&sigs, &arg_exprs, is_method, source, scope_tree)
            else {
                continue;
            };
            let visible_params = visible_params_for(&best_sig, is_method);
            for (i, arg_expr) in arg_exprs.iter().enumerate() {
                // Vararg param absorbs everything past its position.
                let param_idx = i;
                let param = match visible_params.get(param_idx) {
                    Some(p) => p,
                    None => break,
                };
                if param.name == "..." {
                    break;
                }
                let actual = infer_argument_type(*arg_expr, source, scope_tree);
                if actual == TypeFact::Unknown {
                    continue;
                }
                if !is_type_compatible(&param.type_fact, &actual) {
                    diagnostics.push(Diagnostic {
                        range: line_index.ts_node_to_range(*arg_expr, source),
                        severity: Some(severity),
                        source: Some("mylua".to_string()),
                        message: format!(
                            "Argument {} of '{}': declared '{}', got '{}'",
                            i + 1,
                            display,
                            param.type_fact,
                            actual,
                        ),
                        ..Default::default()
                    });
                }
            }
        }
    }
}

fn collect_function_calls<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if node.kind() == "function_call" {
        out.push(node);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_function_calls(child, out);
        }
    }
}

/// Count actual arguments at a `function_call`'s `arguments` node and
/// return the individual argument-expression nodes.
///
/// Three grammar forms:
/// - `( expression_list )` — multi-arg; count the expression_list's
///   named children.
/// - `table_constructor` (`foo{...}`) — 1 arg, the table itself.
/// - `string` (`foo "x"`) — 1 arg, the string literal.
fn collect_call_arguments<'tree>(
    args: tree_sitter::Node<'tree>,
    source: &[u8],
) -> (u32, Vec<tree_sitter::Node<'tree>>) {
    let exprs = crate::util::extract_call_arg_nodes(args, source);
    (exprs.len() as u32, exprs)
}

fn visible_params_for(
    sig: &crate::type_system::FunctionSignature,
    is_method: bool,
) -> Vec<crate::type_system::ParamInfo> {
    sig.params
        .iter()
        .filter(|p| !(is_method && p.name == "self"))
        .cloned()
        .collect()
}

fn signature_accepts_count(
    sig: &crate::type_system::FunctionSignature,
    actual: u32,
    is_method: bool,
) -> bool {
    let visible = visible_params_for(sig, is_method);
    let (min, max) = accepted_count_bounds(&visible);
    actual >= min && (max == u32::MAX || actual <= max)
}

/// Return the `(min, max)` acceptable argument counts across all
/// overloads, where `max == u32::MAX` indicates at least one overload
/// has a vararg trailing parameter.
fn expected_count_range(
    sigs: &[crate::type_system::FunctionSignature],
    is_method: bool,
) -> (u32, u32) {
    let mut min_acc = u32::MAX;
    let mut max_acc = 0u32;
    let mut any_vararg = false;
    for sig in sigs {
        let visible = visible_params_for(sig, is_method);
        let (lo, hi) = accepted_count_bounds(&visible);
        if hi == u32::MAX {
            any_vararg = true;
        }
        if lo < min_acc {
            min_acc = lo;
        }
        if hi > max_acc {
            max_acc = hi;
        }
    }
    if any_vararg {
        (min_acc, u32::MAX)
    } else {
        (min_acc, max_acc)
    }
}

fn accepted_count_bounds(visible: &[crate::type_system::ParamInfo]) -> (u32, u32) {
    let has_vararg = visible.last().is_some_and(|p| p.name == "...");
    let fixed_len = if has_vararg {
        visible.len().saturating_sub(1)
    } else {
        visible.len()
    };
    let optional_tail = visible[..fixed_len]
        .iter()
        .rev()
        .take_while(|p| p.optional)
        .count();
    let min = fixed_len.saturating_sub(optional_tail) as u32;
    let max = if has_vararg {
        u32::MAX
    } else {
        fixed_len as u32
    };
    (min, max)
}

/// Heuristic: among overloads that accept the actual count, pick the
/// one whose first N param types are compatible with the supplied
/// argument literal types. Returns `None` when no overload is a count
/// match — the caller already diagnosed that case.
fn pick_best_typing_overload(
    sigs: &[crate::type_system::FunctionSignature],
    arg_exprs: &[tree_sitter::Node],
    is_method: bool,
    source: &[u8],
    scope_tree: &crate::scope::ScopeTree,
) -> Option<crate::type_system::FunctionSignature> {
    let actual_count = arg_exprs.len() as u32;
    let candidates: Vec<&crate::type_system::FunctionSignature> = sigs
        .iter()
        .filter(|s| signature_accepts_count(s, actual_count, is_method))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let mut best: Option<(&crate::type_system::FunctionSignature, usize)> = None;
    for sig in candidates {
        let visible = visible_params_for(sig, is_method);
        let mut score = 0usize;
        for (i, arg) in arg_exprs.iter().enumerate() {
            let Some(param) = visible.get(i) else { break };
            if param.name == "..." {
                break;
            }
            let actual = infer_argument_type(*arg, source, scope_tree);
            if actual == TypeFact::Unknown {
                continue;
            }
            if is_type_compatible(&param.type_fact, &actual) {
                score += 1;
            }
        }
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((sig, score));
        }
    }
    best.map(|(s, _)| s.clone())
}
