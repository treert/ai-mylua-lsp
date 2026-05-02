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
use crate::type_system::FunctionSummaryId;
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
    // a call site). Try scope tree first (handles local functions via
    // FunctionRef(id)), then global function_name_index, then global_shard.
    if let Some(summary) = index.summary(uri) {
        // Local function via scope tree → FunctionRef(id)
        if let Some(crate::type_system::TypeFact::Known(
            crate::type_system::KnownType::FunctionRef(fid),
        )) = doc.scope_tree.resolve_type(byte_offset, &name) {
            if let Some(fs) = summary.function_summaries.get(fid) {
                return vec![build_item(
                    fs.name.clone(),
                    SymbolKind::FUNCTION,
                    uri.clone(),
                    fs.range.into(),
                    fs.range.into(),
                )];
            }
        }
        // Global function via function_name_index
        if let Some(fs) = summary.get_function_by_name(&name) {
            return vec![build_item(
                fs.name.clone(),
                SymbolKind::FUNCTION,
                uri.clone(),
                fs.range.into(),
                fs.range.into(),
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
            let r: Range = c.range.into();
            let sr: Range = c.selection_range.into();
            return vec![build_item(
                name,
                kind,
                c.source_uri().clone(),
                r,
                sr,
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
    // Include caller_id when available so shadowed same-name local function
    // expressions do not merge into one incoming caller.
    let mut groups: HashMap<(Uri, String, Option<FunctionSummaryId>), (CallHierarchyItem, Vec<Range>)> = HashMap::new();

    for (uri, summary) in index.summaries_iter() {
        for cs in &summary.call_sites {
            if last_segment(&cs.callee_name) != target {
                continue;
            }
            let caller_item = resolve_caller_item(uri, cs, summary);
            let lsp_range: Range = cs.range.into();
            let key = (uri.clone(), cs.caller_name.clone(), cs.caller_id);
            groups
                .entry(key)
                .or_insert_with(|| (caller_item, Vec::new()))
                .1
                .push(lsp_range);
        }
    }

    groups
        .into_iter()
        .map(|((_uri, _name, _id), (from, ranges))| CallHierarchyIncomingCall {
            from,
            from_ranges: ranges,
        })
        .collect()
}

/// Build a `CallHierarchyItem` for the caller of a call site. When
/// `caller_name` is empty (the call lives at file top level), we
/// return a "module-scope" item using the file URI as the handle.
fn resolve_caller_item(
    uri: &Uri,
    cs: &CallSite,
    summary: &DocumentSummary,
) -> CallHierarchyItem {
    if cs.caller_name.is_empty() {
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
    // Prefer caller_id — avoids name-based ambiguity between local and
    // global functions.
    if let Some(id) = cs.caller_id {
        if let Some(fs) = summary.function_summaries.get(&id) {
            let kind = if cs.caller_name.contains(':') {
                SymbolKind::METHOD
            } else {
                SymbolKind::FUNCTION
            };
            let lsp_range: Range = fs.range.into();
            return build_item(
                cs.caller_name.clone(),
                kind,
                uri.clone(),
                lsp_range,
                lsp_range,
            );
        }
    }
    // Fallback for global functions whose caller_id wasn't set.
    if let Some(fs) = summary.get_function_by_name(&cs.caller_name) {
        let kind = if cs.caller_name.contains(':') {
            SymbolKind::METHOD
        } else {
            SymbolKind::FUNCTION
        };
        let lsp_range: Range = fs.range.into();
        return build_item(
            cs.caller_name.clone(),
            kind,
            uri.clone(),
            lsp_range,
            lsp_range,
        );
    }
    // Last resort: unknown caller (shouldn't happen for a well-formed
    // summary, but stay robust).
    let range = Range {
        start: Position { line: 0, character: 0 },
        end: Position { line: 0, character: 0 },
    };
    build_item(cs.caller_name.clone(), SymbolKind::FUNCTION, uri.clone(), range, range)
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
    let Some(summary) = index.summary(&item.uri) else { return Vec::new() };

    // Find all call sites for this item. Prefer FunctionSummaryId when the
    // item can be tied back to one; otherwise fall back to name matching.
    let item_id = function_id_for_item(summary, item);
    let mut groups: HashMap<String, (CallHierarchyItem, Vec<Range>)> = HashMap::new();

    for cs in &summary.call_sites {
        if let Some(id) = item_id {
            if cs.caller_id != Some(id) {
                continue;
            }
        } else if cs.caller_name != item.name {
            continue;
        }
        let target_name = last_segment(&cs.callee_name).to_string();
        let lsp_range: Range = cs.range.into();
        let to_item = resolve_outgoing_target(&target_name, index, &item.uri, lsp_range);
        groups
            .entry(target_name.clone())
            .or_insert_with(|| (to_item, Vec::new()))
            .1
            .push(lsp_range);
    }

    groups
        .into_iter()
        .map(|(_name, (to, ranges))| CallHierarchyOutgoingCall {
            to,
            from_ranges: ranges,
        })
        .collect()
}

fn function_id_for_item(
    summary: &DocumentSummary,
    item: &CallHierarchyItem,
) -> Option<FunctionSummaryId> {
    summary.function_summaries
        .iter()
        .find_map(|(id, fs)| {
            let range: Range = fs.range.into();
            (fs.name == item.name && range == item.range).then_some(*id)
        })
}

/// Build a `CallHierarchyItem` for an outgoing-call target. Tries
/// the workspace `global_shard` for O(1) lookup. When nothing
/// matches, synthesize a minimal item anchored at the call site
/// itself so the client still has something clickable.
fn resolve_outgoing_target(
    name: &str,
    index: &WorkspaceAggregation,
    fallback_uri: &Uri,
    fallback_range: Range,
) -> CallHierarchyItem {
    // O(1) lookup via global_shard — preferred path.
    if let Some(candidates) = index.global_shard.get(name) {
        if let Some(c) = candidates.first() {
            // Try to refine with the precise FunctionSummary range from
            // the candidate's source file.
            if let Some(summary) = index.summary(c.source_uri()) {
                if let Some(fs) = summary.get_function_by_name(name) {
                    let lsp_range: Range = fs.range.into();
                    return build_item(
                        name.to_string(),
                        SymbolKind::FUNCTION,
                        c.source_uri().clone(),
                        lsp_range,
                        lsp_range,
                    );
                }
            }
            let kind = if matches!(c.kind, GlobalContributionKind::Function) {
                SymbolKind::FUNCTION
            } else {
                SymbolKind::VARIABLE
            };
            let r: Range = c.range.into();
            let sr: Range = c.selection_range.into();
            return build_item(
                name.to_string(),
                kind,
                c.source_uri().clone(),
                r,
                sr,
            );
        }
    }
    // Unknown callee — anchor at the call site itself.
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
    caller_id: Option<FunctionSummaryId>,
    line_index: &LineIndex,
) -> Option<CallSite> {
    let callee = call.child_by_field_name("callee")?;
    let method = call.child_by_field_name("method");

    let (callee_name, range) = if let Some(m) = method {
        // `obj:m(...)` — use the method name
        let name = node_text(m, source).to_string();
        (name, line_index.ts_node_to_byte_range(m, source))
    } else if callee.kind() == "identifier" {
        (node_text(callee, source).to_string(), line_index.ts_node_to_byte_range(callee, source))
    } else if matches!(callee.kind(), "variable" | "field_expression") {
        // Dotted: `a.b.c()` or field expression — take the rightmost
        // field's range and the whole dotted chain as the callee_name
        // (caller can use `last_segment` if they only want the name).
        let text = node_text(callee, source).to_string();
        if let Some(field) = callee.child_by_field_name("field") {
            (text, line_index.ts_node_to_byte_range(field, source))
        } else {
            (text, line_index.ts_node_to_byte_range(callee, source))
        }
    } else {
        return None;
    };

    Some(CallSite {
        callee_name,
        caller_name: caller_name.to_string(),
        caller_id,
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
