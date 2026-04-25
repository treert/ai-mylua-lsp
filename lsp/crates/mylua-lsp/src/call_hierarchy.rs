//! `textDocument/prepareCallHierarchy` + `callHierarchy/incomingCalls`
//! + `callHierarchy/outgoingCalls` support.
//!
//! Strategy (minimal, correct for common cases, opt-in by client
//! request):
//!
//! - **Prepare**: find the identifier at the cursor. If it resolves
//!   to a known local / global function (`FunctionSummary` in some
//!   summary), return a `CallHierarchyItem` pointing at that
//!   declaration. If the cursor sits *on* the function declaration
//!   name itself, we build the item from the enclosing declaration
//!   node directly (no symbol resolution needed).
//! - **Incoming**: iterate every indexed file's stored `call_sites`
//!   (built at summary time) and return callers whose `callee_name`
//!   matches the item's name. Enclosing function is resolved via the
//!   `caller_name` field recorded alongside each call.
//! - **Outgoing**: walk the target function's AST body (via the
//!   currently-open Document if present) for `function_call` nodes
//!   and emit one `CallHierarchyOutgoingCall` per callee.
//!
//! Name matching uses the **last path segment** of the callee: for
//! `m.sub.foo()` the callee name is `foo`, for `obj:bar()` it's
//! `bar`. This is a known simplification that can produce false
//! positives across same-named functions in different files; see
//! `future-work.md`.

use std::collections::HashMap;

use tower_lsp_server::ls_types::*;

use crate::aggregation::WorkspaceAggregation;
use crate::document::Document;
use crate::summary::{CallSite, DocumentSummary, GlobalContributionKind};
use crate::util::{is_ancestor_or_equal, node_text, LineIndex};

// ---------------------------------------------------------------------------
// Prepare
// ---------------------------------------------------------------------------

pub fn prepare_call_hierarchy(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceAggregation,
) -> Vec<CallHierarchyItem> {
    let Some(byte_offset) = doc.line_index().position_to_byte_offset(doc.source(), position) else {
        return Vec::new();
    };
    let source = doc.source();

    let Some(ident) = identifier_at_offset(doc.tree.root_node(), byte_offset) else {
        return Vec::new();
    };
    let name = node_text(ident, source).to_string();

    // Case 1: the cursor is on the declaration name of a function
    // (function_declaration / local_function_declaration). We can
    // build the item without looking it up elsewhere.
    if let Some(item) = item_from_enclosing_declaration(ident, source, uri, doc.line_index()) {
        return vec![item];
    }

    // Case 2: the cursor is on some other identifier occurrence (e.g.
    // a call site). Resolve the name through the local summary's
    // function_summaries first, then the workspace global_shard.
    if let Some(summary) = index.summaries.get(uri) {
        if let Some(fs) = summary.function_summaries.get(&name) {
            return vec![build_item(
                fs.name.clone(),
                SymbolKind::FUNCTION,
                uri.clone(),
                fs.range,
                // Best-effort: declaration `range` already encloses
                // the header; clients accept the same range as
                // selection_range when no finer info exists.
                fs.range,
            )];
        }
    }
    if let Some(candidates) = index.global_shard.get(&name) {
        if let Some(c) = candidates.first() {
            let kind = if matches!(c.kind, GlobalContributionKind::Function) {
                SymbolKind::FUNCTION
            } else {
                SymbolKind::VARIABLE
            };
            return vec![build_item(
                name,
                kind,
                c.source_uri.clone(),
                c.range,
                c.selection_range,
            )];
        }
    }

    Vec::new()
}

/// Walk up from `ident` looking for an enclosing `function_declaration`
/// / `local_function_declaration` whose `name` child equals `ident`.
/// Returns `None` when the identifier is not a declaration name.
fn item_from_enclosing_declaration(
    ident: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    line_index: &LineIndex,
) -> Option<CallHierarchyItem> {
    let parent = ident.parent()?;
    // `function_declaration` uses `function_name` for the name
    // field; `local_function_declaration` uses a bare `identifier`.
    // For `function_declaration`, walking up from the identifier
    // leaves us at `function_name`, and its parent is
    // `function_declaration`.
    let decl = match parent.kind() {
        "function_declaration" | "local_function_declaration" => parent,
        "function_name" => parent.parent().filter(|p| p.kind() == "function_declaration")?,
        _ => return None,
    };
    let name_node = decl.child_by_field_name("name")?;
    // Ensure the identifier is (part of) the name.
    if !is_ancestor_or_equal(name_node, ident) {
        return None;
    }
    let name = node_text(name_node, source).to_string();
    let kind = if name.contains(':') {
        SymbolKind::METHOD
    } else {
        SymbolKind::FUNCTION
    };
    Some(build_item(
        name,
        kind,
        uri.clone(),
        line_index.ts_node_to_range(decl, source),
        line_index.ts_node_to_range(name_node, source),
    ))
}

fn identifier_at_offset(root: tree_sitter::Node, byte_offset: usize) -> Option<tree_sitter::Node> {
    let node = root.descendant_for_byte_range(byte_offset, byte_offset)?;
    if node.kind() == "identifier" {
        Some(node)
    } else {
        // Descend to the deepest identifier overlapping the offset.
        let mut cursor = node.walk();
        if cursor.goto_first_child() {
            loop {
                let c = cursor.node();
                if c.start_byte() <= byte_offset && byte_offset <= c.end_byte() {
                    if c.kind() == "identifier" {
                        return Some(c);
                    }
                    if let Some(inner) = identifier_at_offset(c, byte_offset) {
                        return Some(inner);
                    }
                }
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
        }
        None
    }
}

fn build_item(
    name: String,
    kind: SymbolKind,
    uri: Uri,
    range: Range,
    selection_range: Range,
) -> CallHierarchyItem {
    CallHierarchyItem {
        name,
        kind,
        tags: None,
        detail: None,
        uri,
        range,
        selection_range,
        data: None,
    }
}

// ---------------------------------------------------------------------------
// Incoming
// ---------------------------------------------------------------------------

pub fn incoming_calls(
    item: &CallHierarchyItem,
    index: &WorkspaceAggregation,
) -> Vec<CallHierarchyIncomingCall> {
    let target = last_segment(&item.name);
    // Map from (caller_uri, caller_name) → accumulated call ranges.
    let mut groups: HashMap<(Uri, String), (CallHierarchyItem, Vec<Range>)> = HashMap::new();

    for (uri, summary) in &index.summaries {
        for cs in &summary.call_sites {
            if last_segment(&cs.callee_name) != target {
                continue;
            }
            let caller_item = resolve_caller_item(uri, &cs.caller_name, summary);
            let key = (uri.clone(), cs.caller_name.clone());
            groups
                .entry(key)
                .or_insert_with(|| (caller_item, Vec::new()))
                .1
                .push(cs.range);
        }
    }

    groups
        .into_iter()
        .map(|((_uri, _name), (from, ranges))| CallHierarchyIncomingCall {
            from,
            from_ranges: ranges,
        })
        .collect()
}

/// Build a `CallHierarchyItem` for the caller of a call site. When
/// `caller_name` is empty (the call lives at file top level), we
/// return a "module-scope" item using the file URI as the handle.
fn resolve_caller_item(uri: &Uri, caller_name: &str, summary: &DocumentSummary) -> CallHierarchyItem {
    if caller_name.is_empty() {
        let range = Range {
            start: Position { line: 0, character: 0 },
            end: Position { line: 0, character: 0 },
        };
        return build_item(
            file_name_hint(uri),
            SymbolKind::MODULE,
            uri.clone(),
            range,
            range,
        );
    }
    if let Some(fs) = summary.function_summaries.get(caller_name) {
        let kind = if caller_name.contains(':') {
            SymbolKind::METHOD
        } else {
            SymbolKind::FUNCTION
        };
        return build_item(
            caller_name.to_string(),
            kind,
            uri.clone(),
            fs.range,
            fs.range,
        );
    }
    // Fallback: unknown caller name (shouldn't happen for a
    // well-formed summary, but stay robust).
    let range = Range {
        start: Position { line: 0, character: 0 },
        end: Position { line: 0, character: 0 },
    };
    build_item(caller_name.to_string(), SymbolKind::FUNCTION, uri.clone(), range, range)
}

fn file_name_hint(uri: &Uri) -> String {
    let s = uri.to_string();
    s.rsplit('/').next().unwrap_or(&s).to_string()
}

// ---------------------------------------------------------------------------
// Outgoing
// ---------------------------------------------------------------------------

pub fn outgoing_calls(
    item: &CallHierarchyItem,
    index: &WorkspaceAggregation,
) -> Vec<CallHierarchyOutgoingCall> {
    let Some(summary) = index.summaries.get(&item.uri) else { return Vec::new() };

    // Find all call sites whose `caller_name == item.name`.
    let mut groups: HashMap<String, (CallHierarchyItem, Vec<Range>)> = HashMap::new();

    for cs in &summary.call_sites {
        if cs.caller_name != item.name {
            continue;
        }
        let target_name = last_segment(&cs.callee_name).to_string();
        let to_item = resolve_outgoing_target(&target_name, index, &item.uri, cs.range);
        groups
            .entry(target_name.clone())
            .or_insert_with(|| (to_item, Vec::new()))
            .1
            .push(cs.range);
    }

    groups
        .into_iter()
        .map(|(_name, (to, ranges))| CallHierarchyOutgoingCall {
            to,
            from_ranges: ranges,
        })
        .collect()
}

/// Build a `CallHierarchyItem` for an outgoing-call target. Tries
/// (in order): same-file function_summaries, workspace global_shard.
/// When nothing matches, synthesize a minimal item anchored at the
/// call site itself so the client still has something clickable.
fn resolve_outgoing_target(
    name: &str,
    index: &WorkspaceAggregation,
    fallback_uri: &Uri,
    fallback_range: Range,
) -> CallHierarchyItem {
    // 1. O(1) lookup via global_shard — preferred path.
    if let Some(candidates) = index.global_shard.get(name) {
        if let Some(c) = candidates.first() {
            // Try to refine with the precise FunctionSummary range from
            // the candidate's source file.
            if let Some(summary) = index.summaries.get(&c.source_uri) {
                if let Some(fs) = summary.function_summaries.get(name) {
                    return build_item(
                        name.to_string(),
                        SymbolKind::FUNCTION,
                        c.source_uri.clone(),
                        fs.range,
                        fs.range,
                    );
                }
            }
            let kind = if matches!(c.kind, GlobalContributionKind::Function) {
                SymbolKind::FUNCTION
            } else {
                SymbolKind::VARIABLE
            };
            return build_item(
                name.to_string(),
                kind,
                c.source_uri.clone(),
                c.range,
                c.selection_range,
            );
        }
    }
    // 2. Fallback: linear scan over all summaries (handles names not
    //    registered in global_shard, e.g. local helpers).
    for (uri, summary) in &index.summaries {
        if let Some(fs) = summary.function_summaries.get(name) {
            return build_item(
                name.to_string(),
                SymbolKind::FUNCTION,
                uri.clone(),
                fs.range,
                fs.range,
            );
        }
    }
    // 3. Unknown callee — anchor at the call site itself.
    build_item(
        name.to_string(),
        SymbolKind::FUNCTION,
        fallback_uri.clone(),
        fallback_range,
        fallback_range,
    )
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the last dot-or-colon-separated segment of a (possibly
/// qualified) callee name. `m.sub.foo` → `foo`; `obj:bar` → `bar`;
/// `baz` → `baz`.
fn last_segment(name: &str) -> &str {
    if let Some(idx) = name.rfind(['.', ':']) {
        &name[idx + 1..]
    } else {
        name
    }
}

/// Record a single call site into `calls`, given a `function_call`
/// AST node and the enclosing function's (possibly qualified) name.
/// Shared with `summary_builder` so single-file inference and this
/// module don't drift.
pub fn extract_call_site(
    call: tree_sitter::Node,
    source: &[u8],
    caller_name: &str,
    line_index: &LineIndex,
) -> Option<CallSite> {
    let callee = call.child_by_field_name("callee")?;
    let method = call.child_by_field_name("method");

    let (callee_name, range) = if let Some(m) = method {
        // `obj:m(...)` — use the method name
        let name = node_text(m, source).to_string();
        (name, line_index.ts_node_to_range(m, source))
    } else if callee.kind() == "identifier" {
        (node_text(callee, source).to_string(), line_index.ts_node_to_range(callee, source))
    } else if matches!(callee.kind(), "variable" | "field_expression") {
        // Dotted: `a.b.c()` or field expression — take the rightmost
        // field's range and the whole dotted chain as the callee_name
        // (caller can use `last_segment` if they only want the name).
        let text = node_text(callee, source).to_string();
        if let Some(field) = callee.child_by_field_name("field") {
            (text, line_index.ts_node_to_range(field, source))
        } else {
            (text, line_index.ts_node_to_range(callee, source))
        }
    } else {
        return None;
    };

    Some(CallSite {
        callee_name,
        caller_name: caller_name.to_string(),
        range,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_segment_strips_dots_and_colons() {
        assert_eq!(last_segment("foo"), "foo");
        assert_eq!(last_segment("m.foo"), "foo");
        assert_eq!(last_segment("m.sub.foo"), "foo");
        assert_eq!(last_segment("obj:bar"), "bar");
        assert_eq!(last_segment("m.obj:bar"), "bar");
    }
}
