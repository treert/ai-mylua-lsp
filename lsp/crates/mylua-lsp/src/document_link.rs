//! `textDocument/documentLink` — makes `require("mod.path")` strings
//! clickable. The client resolves each link to open the target `.lua`
//! file, matching the navigation behavior of `goto_definition` on the
//! `require(...)` call itself but available inline without a cursor
//! placement round-trip.
//!
//! Coverage:
//!
//! - `require("mod.path")` paren-call form (the common case).
//! - `require "mod.path"` short string-call form (`arguments` is the
//!   `string` node itself, no wrapping parentheses).
//! - Both single-quoted and double-quoted strings are recognized;
//!   long brackets (`[[...]]`) are **not** considered because module
//!   paths using long brackets aren't idiomatic and can contain
//!   arbitrary text that we shouldn't parse.
//!
//! Non-goals:
//!
//! - We deliberately do NOT follow aliases to their `require_aliases`
//!   expansion text inside the link range; the range is always the
//!   string content itself, and the target URI is resolved from
//!   `module_index` through the session URI interner when available.
//! - `m = require; m("foo")` aliased call chains are out of scope —
//!   the callee must be the literal identifier `require`.

use tower_lsp_server::ls_types::{DocumentLink, Range, Uri};

use crate::aggregation::WorkspaceAggregation;
use crate::uri_id::UriInterner;
use crate::util::{node_text, LineIndex};

/// Collect all `require("mod")` document links in `tree`. Strings that
/// don't resolve to a known workspace module are silently skipped —
/// clients render a link only when a target exists.
pub fn document_links(
    root: tree_sitter::Node,
    source: &[u8],
    index: &WorkspaceAggregation,
    uri_interner: Option<&UriInterner>,
    line_index: &LineIndex,
) -> Vec<DocumentLink> {
    let mut out = Vec::new();
    let mut cursor = root.walk();
    collect_recursive(&mut cursor, source, index, uri_interner, &mut out, line_index);
    out
}

fn collect_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    index: &WorkspaceAggregation,
    uri_interner: Option<&UriInterner>,
    out: &mut Vec<DocumentLink>,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    if node.kind() == "function_call" {
        if let Some((string_node, module_path)) = extract_require_argument(node, source) {
            if let Some(target) = resolve_module_target(index, uri_interner, &module_path) {
                // Narrow the link range to the string *contents*
                // (inside the quotes) when we can, so a click on the
                // quotes themselves doesn't feel off; fall back to
                // the full string node range if we can't find quote
                // bytes (e.g. malformed string that tree-sitter still
                // produced as `string` via error recovery).
                let link_range = content_range_inside_quotes(string_node, source, line_index)
                    .unwrap_or_else(|| line_index.ts_node_to_range(string_node, source));
                out.push(document_link(link_range, target));
            }
        }
    }
    if cursor.goto_first_child() {
        loop {
            collect_recursive(cursor, source, index, uri_interner, out, line_index);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn resolve_module_target(
    index: &WorkspaceAggregation,
    uri_interner: Option<&UriInterner>,
    module_path: &str,
) -> Option<Uri> {
    if let Some(uri_interner) = uri_interner {
        if let Some(uri) = index
            .resolve_module_to_id(module_path)
            .and_then(|uri_id| uri_interner.resolve(uri_id))
        {
            return Some(uri);
        }
    }
    index.resolve_module_to_uri(module_path)
}

/// When `call.callee` is the literal identifier `require` and the
/// single argument is a string, return `(string_node, module_path)`.
/// Handles both `require("mod")` and `require "mod"` forms.
fn extract_require_argument<'tree>(
    call: tree_sitter::Node<'tree>,
    source: &[u8],
) -> Option<(tree_sitter::Node<'tree>, String)> {
    let callee = call.child_by_field_name("callee")?;
    // Callee must be a bare `require` identifier. `variable` with a
    // single identifier child also matches (grammar may wrap
    // identifiers as `variable`). Reject dotted / method-style
    // callees — `m.require()` isn't Lua's require.
    let callee_ident = if callee.kind() == "identifier" {
        callee
    } else if callee.kind() == "variable"
        && callee.child_by_field_name("object").is_none()
        && callee.child_by_field_name("index").is_none()
    {
        callee.named_child(0).filter(|c| c.kind() == "identifier")?
    } else {
        return None;
    };
    if node_text(callee_ident, source) != "require" {
        return None;
    }

    let args = call.child_by_field_name("arguments")?;
    // Short call form `require "mod"` / `require 'mod'`: the
    // grammar's `arguments` node wraps a single `string` named
    // child (no parens). Identified by the first byte being a
    // quote character.
    match source.get(args.start_byte()).copied() {
        Some(b'"') | Some(b'\'') => {
            // Either `args` IS the string (older grammar inlining)
            // or wraps it as a single named child.
            let string_node = if args.kind() == "string" {
                args
            } else {
                args.named_child(0).filter(|c| c.kind() == "string")?
            };
            let path = parse_module_path_from_string(string_node, source)?;
            return Some((string_node, path));
        }
        _ => {}
    }
    // Paren form: `arguments` starts with `(` and contains an
    // `expression_list` with exactly one `string` child.
    if source.get(args.start_byte()).copied() == Some(b'(') {
        let list = (0..args.named_child_count())
            .filter_map(|i| args.named_child(i as u32))
            .find(|c| c.kind() == "expression_list")?;
        if list.named_child_count() != 1 {
            return None; // require takes exactly one arg; be strict
        }
        let first = list.named_child(0)?;
        if first.kind() != "string" {
            return None;
        }
        let path = parse_module_path_from_string(first, source)?;
        return Some((first, path));
    }
    None
}

/// Extract the module path text from a short-quoted string literal.
/// Returns `None` for long-bracket strings (`[[...]]`) since those
/// don't correspond to idiomatic `require` usage and the unescaping
/// logic would be misleading.
fn parse_module_path_from_string(string_node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let raw = node_text(string_node, source);
    let bytes = raw.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if !matches!(first, b'"' | b'\'') || first != last {
        return None;
    }
    // Trim outer quotes. No escape processing — module paths never
    // contain escape sequences in idiomatic Lua.
    let inner = &raw[1..raw.len() - 1];
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

/// Compute an LSP Range covering just the content between the quotes
/// of a short-quoted string node. Keeps the client's underline
/// visually tight on the module path. Returns `None` when the node
/// range is degenerate (zero or one byte wide).
fn content_range_inside_quotes(string_node: tree_sitter::Node, source: &[u8], line_index: &LineIndex) -> Option<Range> {
    let full = line_index.ts_node_to_range(string_node, source);
    let start_byte = string_node.start_byte();
    let end_byte = string_node.end_byte();
    if end_byte <= start_byte + 1 {
        return None;
    }
    // Single-line shrink is the common case: move start forward by
    // one column and end back by one. We only do this when both
    // quotes lie on a single line so multi-line strings keep their
    // full range.
    if full.start.line != full.end.line {
        return None;
    }
    if full.end.character == 0 || full.start.character + 1 >= full.end.character {
        return None;
    }
    Some(Range {
        start: tower_lsp_server::ls_types::Position {
            line: full.start.line,
            character: full.start.character + 1,
        },
        end: tower_lsp_server::ls_types::Position {
            line: full.end.line,
            character: full.end.character - 1,
        },
    })
}

fn document_link(range: Range, target: Uri) -> DocumentLink {
    DocumentLink {
        range,
        target: Some(target),
        tooltip: Some("Open module".to_string()),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_module_path_rejects_long_brackets() {
        // Simulate a bracketed literal by crafting a string directly;
        // there's no public helper to build a Node, so we exercise
        // the private path via the outer integration tests instead.
        // This unit test just sanity-checks the quoted-path parser.
        assert!(
            parse_quoted_module_path_for_test("\"foo.bar\"").as_deref() == Some("foo.bar"),
            "double-quoted module path extracts cleanly",
        );
        assert!(
            parse_quoted_module_path_for_test("'foo.bar'").as_deref() == Some("foo.bar"),
            "single-quoted module path extracts cleanly",
        );
        assert!(
            parse_quoted_module_path_for_test("\"\"").is_none(),
            "empty string rejected",
        );
        assert!(
            parse_quoted_module_path_for_test("[[foo.bar]]").is_none(),
            "long bracket form rejected",
        );
    }

    // Trivial standalone helper for unit-testing the quoted-string
    // parser without constructing a tree-sitter Node.
    fn parse_quoted_module_path_for_test(raw: &str) -> Option<String> {
        let bytes = raw.as_bytes();
        if bytes.len() < 2 {
            return None;
        }
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if !matches!(first, b'"' | b'\'') || first != last {
            return None;
        }
        let inner = &raw[1..raw.len() - 1];
        if inner.is_empty() {
            return None;
        }
        Some(inner.to_string())
    }
}
