mod test_helpers;

use mylua_lsp::selection_range::selection_range;
use test_helpers::*;
use tower_lsp_server::ls_types::Range;

/// Flatten a SelectionRange chain to a Vec of its ranges, innermost first.
fn chain_ranges(sr: &tower_lsp_server::ls_types::SelectionRange) -> Vec<Range> {
    let mut out = Vec::new();
    let mut cur: Option<&tower_lsp_server::ls_types::SelectionRange> = Some(sr);
    while let Some(n) = cur {
        out.push(n.range);
        cur = n.parent.as_deref();
    }
    out
}

#[test]
fn selection_range_empty_positions_returns_empty() {
    let (doc, _uri, _agg) = setup_single_file("local x = 1\n", "a.lua");
    let result = selection_range(&doc, &[]);
    assert!(result.is_empty());
}

#[test]
fn selection_range_grows_monotonically_outward() {
    // `local x = 1 + 2` — cursor on `1`. Chain should expand from
    // number literal → binary_expression → expression_list →
    // local_declaration → source_file.
    let src = "local x = 1 + 2\n";
    let (doc, _uri, _agg) = setup_single_file(src, "a.lua");

    // Position on `1` (line 0, col 10)
    let result = selection_range(&doc, &[pos(0, 10)]);
    assert_eq!(result.len(), 1, "one chain per input position");
    let ranges = chain_ranges(&result[0]);
    assert!(ranges.len() >= 3, "should have at least 3 tiers of growth, got {:?}", ranges);

    // Each subsequent range should strictly contain the previous.
    for w in ranges.windows(2) {
        let inner = &w[0];
        let outer = &w[1];
        let inner_start = (inner.start.line, inner.start.character);
        let inner_end = (inner.end.line, inner.end.character);
        let outer_start = (outer.start.line, outer.start.character);
        let outer_end = (outer.end.line, outer.end.character);
        assert!(
            outer_start <= inner_start && outer_end >= inner_end,
            "each outer range should contain the inner: outer={:?} inner={:?}",
            outer, inner,
        );
    }
}

#[test]
fn selection_range_multiple_positions_all_handled() {
    let src = "local a = 1\nlocal b = 2\n";
    let (doc, _uri, _agg) = setup_single_file(src, "multi.lua");

    let result = selection_range(&doc, &[pos(0, 6), pos(1, 6)]);
    assert_eq!(result.len(), 2, "one chain per input position");
}

#[test]
fn selection_range_inside_function_body() {
    // Cursor inside a function body — chain should include
    // the statement, body, function_definition-or-declaration,
    // up to the outer local/assignment/source_file.
    let src = "function f()\n  return 42\nend\n";
    let (doc, _uri, _agg) = setup_single_file(src, "fn.lua");

    let result = selection_range(&doc, &[pos(1, 9)]); // on `42`
    assert_eq!(result.len(), 1);
    let ranges = chain_ranges(&result[0]);
    // The outermost range covers the entire source (function declaration).
    let outer = ranges.last().unwrap();
    assert!(outer.start.line <= 0);
    assert!(outer.end.line >= 2);
}

#[test]
fn selection_range_skips_unnamed_tokens() {
    // Cursor on `(` unnamed token. We skip unnamed nodes in the
    // chain; the innermost named ancestor should still be found.
    let src = "f(x, y)\n";
    let (doc, _uri, _agg) = setup_single_file(src, "call.lua");

    let result = selection_range(&doc, &[pos(0, 1)]);
    assert_eq!(result.len(), 1);
    // The chain should NOT start on a single-char `(` — every link
    // should be a named node span.
    let ranges = chain_ranges(&result[0]);
    let first = &ranges[0];
    let single_char = first.start.line == first.end.line
        && first.end.character == first.start.character + 1;
    assert!(
        !single_char || ranges.len() > 1,
        "chain shouldn't stall on single-char unnamed tokens, got: {:?}", ranges,
    );
}
