use std::collections::HashMap;

use crate::type_system::FunctionSummaryId;
use crate::util::{node_text, LineIndex};

use super::enclosing_statement_for_function_expr;

/// Second single-pass over the AST to collect `CallSite` records
/// scoped to their enclosing function. Uses its own walk (rather
/// than threading through the main visitor) because the main
/// visitor is already cluttered with type-inference state and the
/// call-site concern is mostly independent.
///
/// `name_to_id` maps function names (as they appear in the source)
/// to their `FunctionSummaryId`, allowing each `CallSite` to record
/// the precise caller ID instead of relying on name-based lookup at
/// query time.
pub(super) fn collect_call_sites(
    root: tree_sitter::Node,
    source: &[u8],
    line_index: &LineIndex,
    name_to_id: &HashMap<String, FunctionSummaryId>,
) -> Vec<crate::summary::CallSite> {
    let mut out = Vec::new();
    collect_calls_in_scope(root, source, "", None, &mut out, line_index, name_to_id);
    out
}

/// Walk `node` emitting every `function_call` encountered, tagging
/// its enclosing function name via `caller_name`. Entering a nested
/// function updates `caller_name` for the subtree.
fn collect_calls_in_scope(
    node: tree_sitter::Node,
    source: &[u8],
    caller_name: &str,
    caller_id: Option<FunctionSummaryId>,
    out: &mut Vec<crate::summary::CallSite>,
    line_index: &LineIndex,
    name_to_id: &HashMap<String, FunctionSummaryId>,
) {
    match node.kind() {
        "function_declaration" | "local_function_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_default();
            let id = name_to_id.get(&name).copied();
            if let Some(body) = node.child_by_field_name("body") {
                collect_calls_in_scope(body, source, &name, id, out, line_index, name_to_id);
            }
            return;
        }
        "function_definition" => {
            // Anonymous function — caller_name for its body is the
            // binding anchor if we can identify one, else inherit
            // from the outer scope. We only set a meaningful name
            // when the function expression is the direct RHS of a
            // local_declaration / assignment_statement with a
            // simple bare-identifier LHS; this matches what
            // `enclosing_statement_for_function_expr` already
            // produces.
            let inferred = infer_anon_caller_name(node, source);
            let (sub_caller, sub_id) = if let Some(ref name) = inferred {
                (name.as_str(), name_to_id.get(name).copied())
            } else {
                (caller_name, caller_id)
            };
            if let Some(body) = node.child_by_field_name("body") {
                collect_calls_in_scope(body, source, sub_caller, sub_id, out, line_index, name_to_id);
            }
            return;
        }
        "function_call" => {
            if let Some(cs) = crate::call_hierarchy::extract_call_site(node, source, caller_name, caller_id, line_index) {
                out.push(cs);
            }
            // Still recurse — arguments may contain nested calls
            // (e.g. `foo(bar(1))`) whose callee we also want to
            // record, with the same caller context.
        }
        _ => {}
    }
    // Skip bracket-key-only table constructors — they contain only
    // literal key-value pairs, never function calls.
    if node.kind() == "table_constructor"
        && crate::util::is_bracket_key_only_table(node)
    {
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_calls_in_scope(child, source, caller_name, caller_id, out, line_index, name_to_id);
        }
    }
}

/// Best-effort: given a `function_definition` node, return the
/// binding name if it sits at the RHS of a simple `local f = function()` /
/// `Foo.m = function()` / `Foo.m = function() end` assignment. Dotted
/// LHS are preserved verbatim; `obj:m` wrappers produced implicitly
/// (rare for `function_definition`) are kept as-is.
fn infer_anon_caller_name(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let stmt = enclosing_statement_for_function_expr(node)?;
    match stmt.kind() {
        "local_declaration" => {
            let names = stmt.child_by_field_name("names")?;
            let id = names.named_child(0)?;
            if id.kind() == "identifier" {
                Some(node_text(id, source).to_string())
            } else {
                None
            }
        }
        "assignment_statement" => {
            let left = stmt.child_by_field_name("left")?;
            let first = left.named_child(0)?;
            // Return the full LHS text (supports `Foo.m` / `m[1]` / bare id).
            Some(node_text(first, source).to_string())
        }
        _ => None,
    }
}
