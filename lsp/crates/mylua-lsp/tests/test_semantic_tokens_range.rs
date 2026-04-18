mod test_helpers;

use mylua_lsp::semantic_tokens;
use test_helpers::*;
use tower_lsp_server::ls_types::Range;

fn absolute_cols(tokens: &[tower_lsp_server::ls_types::SemanticToken]) -> Vec<(u32, u32, u32)> {
    // Reconstruct (line, col, length) from delta encoding.
    let mut out = Vec::with_capacity(tokens.len());
    let mut line = 0u32;
    let mut col = 0u32;
    for t in tokens {
        if t.delta_line == 0 {
            col += t.delta_start;
        } else {
            line += t.delta_line;
            col = t.delta_start;
        }
        out.push((line, col, t.length));
    }
    out
}

fn range(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
    Range {
        start: pos(sl, sc),
        end: pos(el, ec),
    }
}

#[test]
fn semantic_tokens_range_filters_to_range() {
    let src = "local a = 1\nlocal b = 2\nlocal c = 3\nlocal d = 4\n";
    let (doc, _uri, _agg) = setup_single_file(src, "r.lua");

    // Range covering lines 1..=2 (middle two lines)
    let tokens = semantic_tokens::collect_semantic_tokens_range(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &doc.scope_tree,
        range(1, 0, 2, 0),
    );
    let positions = absolute_cols(&tokens);
    let lines: Vec<u32> = positions.iter().map(|(l, _, _)| *l).collect();
    assert!(
        lines.iter().all(|l| *l >= 1 && *l <= 2),
        "all tokens should be within lines 1..=2, got lines: {:?}", lines,
    );
    // Must not include `a` (line 0) or `d` (line 3)
    assert!(!lines.contains(&0));
    assert!(!lines.contains(&3));
}

#[test]
fn semantic_tokens_range_delta_encoding_starts_fresh() {
    // Range provider is a self-contained response: the first token's
    // delta must be computed against (0, 0), NOT against whatever
    // the previous `full` result contained. Verify by checking the
    // first token's delta_line equals its absolute line.
    let src = "local a = 1\nlocal b = 2\nlocal c = 3\n";
    let (doc, _uri, _agg) = setup_single_file(src, "d.lua");

    let tokens = semantic_tokens::collect_semantic_tokens_range(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &doc.scope_tree,
        range(2, 0, 2, 20),
    );
    assert!(!tokens.is_empty(), "line 2 should have at least the `c` token");
    // First (and only) token: `c` at line 2 col 6, length 1.
    assert_eq!(tokens[0].delta_line, 2, "delta_line should be absolute for first token");
    assert_eq!(tokens[0].delta_start, 6);
    assert_eq!(tokens[0].length, 1);
}

#[test]
fn semantic_tokens_range_empty_range_returns_empty() {
    let src = "local a = 1\nlocal b = 2\n";
    let (doc, _uri, _agg) = setup_single_file(src, "e.lua");

    // Range on an empty line past EOF has no identifiers.
    let tokens = semantic_tokens::collect_semantic_tokens_range(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &doc.scope_tree,
        range(100, 0, 100, 0),
    );
    assert!(tokens.is_empty(), "out-of-range request should be empty");
}

#[test]
fn semantic_tokens_range_full_file_equals_full_result() {
    // Requesting the full file via range should match `full` output.
    let src = "local a = 1\nfunction f() return a end\nprint(a)\n";
    let (doc, _uri, _agg) = setup_single_file(src, "full.lua");

    let full = semantic_tokens::collect_semantic_tokens(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &doc.scope_tree,
    );
    let ranged = semantic_tokens::collect_semantic_tokens_range(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &doc.scope_tree,
        range(0, 0, 100, 0),
    );
    assert_eq!(full.len(), ranged.len(), "full equivalent range should yield same count");
    for (f, r) in full.iter().zip(ranged.iter()) {
        assert_eq!(f.delta_line, r.delta_line);
        assert_eq!(f.delta_start, r.delta_start);
        assert_eq!(f.length, r.length);
        assert_eq!(f.token_type, r.token_type);
        assert_eq!(f.token_modifiers_bitset, r.token_modifiers_bitset);
    }
}
