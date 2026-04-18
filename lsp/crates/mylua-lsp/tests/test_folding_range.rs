mod test_helpers;

use mylua_lsp::folding_range::folding_range;
use test_helpers::*;
use tower_lsp_server::ls_types::FoldingRangeKind;

#[test]
fn folding_range_empty_file_returns_empty() {
    let (doc, _uri, _agg) = setup_single_file("", "empty.lua");
    let folds = folding_range(&doc);
    assert!(folds.is_empty(), "empty file produces no folds");
}

#[test]
fn folding_range_single_line_function_skipped() {
    let (doc, _uri, _agg) = setup_single_file("function f() end\n", "single.lua");
    let folds = folding_range(&doc);
    assert!(
        folds.is_empty(),
        "single-line function has no meaningful body to fold, got: {:?}",
        folds,
    );
}

#[test]
fn folding_range_function_with_body() {
    let src = "function f()\n  local x = 1\n  return x\nend\n";
    let (doc, _uri, _agg) = setup_single_file(src, "func.lua");
    let folds = folding_range(&doc);
    assert_eq!(folds.len(), 1, "one fold for the function: {:?}", folds);
    let f = &folds[0];
    assert_eq!(f.start_line, 0);
    assert_eq!(
        f.end_line, 2,
        "end_line should be end_row-1 so the closing `end` stays visible",
    );
    assert_eq!(f.kind, Some(FoldingRangeKind::Region));
}

#[test]
fn folding_range_nested_control_flow() {
    // function f() if x then for i=1,10 do end end end
    // expanded onto separate lines
    let src = "function f()\n  if x then\n    for i = 1, 10 do\n      print(i)\n    end\n  end\nend\n";
    let (doc, _uri, _agg) = setup_single_file(src, "nested.lua");
    let folds = folding_range(&doc);
    assert_eq!(
        folds.len(),
        3,
        "expected 3 folds (function, if, for), got: {:?}",
        folds,
    );
    let mut regions: Vec<(u32, u32)> = folds
        .iter()
        .filter(|f| f.kind == Some(FoldingRangeKind::Region))
        .map(|f| (f.start_line, f.end_line))
        .collect();
    regions.sort();
    // function spans lines 0..=6, body hides 0..=5
    // if spans lines 1..=5, body hides 1..=4
    // for spans lines 2..=4, body hides 2..=3
    assert_eq!(regions, vec![(0, 5), (1, 4), (2, 3)]);
}

#[test]
fn folding_range_repeat_until() {
    let src = "repeat\n  x = x + 1\nuntil x > 10\n";
    let (doc, _uri, _agg) = setup_single_file(src, "repeat.lua");
    let folds = folding_range(&doc);
    assert_eq!(folds.len(), 1);
    assert_eq!(folds[0].start_line, 0);
    assert_eq!(folds[0].end_line, 1);
    assert_eq!(folds[0].kind, Some(FoldingRangeKind::Region));
}

#[test]
fn folding_range_while_loop() {
    let src = "while x < 10 do\n  x = x + 1\nend\n";
    let (doc, _uri, _agg) = setup_single_file(src, "while.lua");
    let folds = folding_range(&doc);
    assert_eq!(folds.len(), 1);
    assert_eq!(folds[0].start_line, 0);
    assert_eq!(folds[0].end_line, 1);
}

#[test]
fn folding_range_do_block() {
    let src = "do\n  local x = 1\n  print(x)\nend\n";
    let (doc, _uri, _agg) = setup_single_file(src, "do.lua");
    let folds = folding_range(&doc);
    assert_eq!(folds.len(), 1);
    assert_eq!(folds[0].start_line, 0);
    assert_eq!(folds[0].end_line, 2);
}

#[test]
fn folding_range_for_numeric_and_generic() {
    let src = "for i = 1, 10 do\n  print(i)\nend\nfor k, v in pairs(t) do\n  print(k)\nend\n";
    let (doc, _uri, _agg) = setup_single_file(src, "for.lua");
    let folds = folding_range(&doc);
    assert_eq!(folds.len(), 2, "both for-loops should fold, got: {:?}", folds);
}

#[test]
fn folding_range_table_constructor_multiline() {
    let src = "local t = {\n  1,\n  2,\n  3,\n}\n";
    let (doc, _uri, _agg) = setup_single_file(src, "table.lua");
    let folds = folding_range(&doc);
    assert_eq!(folds.len(), 1);
    assert_eq!(folds[0].start_line, 0);
    assert_eq!(folds[0].end_line, 3);
    assert_eq!(folds[0].kind, Some(FoldingRangeKind::Region));
}

#[test]
fn folding_range_block_comment_multiline() {
    let src = "--[[\nthis is a\nblock comment\n]]\nlocal x = 1\n";
    let (doc, _uri, _agg) = setup_single_file(src, "block_comment.lua");
    let folds = folding_range(&doc);
    let comment_folds: Vec<_> = folds
        .iter()
        .filter(|f| f.kind == Some(FoldingRangeKind::Comment))
        .collect();
    assert_eq!(
        comment_folds.len(),
        1,
        "one Comment fold for the block comment, got: {:?}",
        folds,
    );
    let f = comment_folds[0];
    assert_eq!(f.start_line, 0);
    assert_eq!(
        f.end_line, 3,
        "Comment fold should include the closing line (end_line=end_row)",
    );
}

#[test]
fn folding_range_line_comment_not_folded() {
    let src = "-- just a single line comment\nlocal x = 1\n";
    let (doc, _uri, _agg) = setup_single_file(src, "line_comment.lua");
    let folds = folding_range(&doc);
    assert!(
        folds.iter().all(|f| f.kind != Some(FoldingRangeKind::Comment)),
        "single-line `--` comment must not fold, got: {:?}",
        folds,
    );
}

#[test]
fn folding_range_emmy_comment_group() {
    let src = "---@class Foo\n---@field x number\n---@field y number\nFoo = {}\n";
    let (doc, _uri, _agg) = setup_single_file(src, "emmy.lua");
    let folds = folding_range(&doc);
    let comment_folds: Vec<_> = folds
        .iter()
        .filter(|f| f.kind == Some(FoldingRangeKind::Comment))
        .collect();
    assert_eq!(
        comment_folds.len(),
        1,
        "one Comment fold for the `---@...` run, got: {:?}",
        folds,
    );
    assert_eq!(comment_folds[0].start_line, 0);
    assert_eq!(comment_folds[0].end_line, 2);
}

#[test]
fn folding_range_leveled_block_comment() {
    let src = "--[==[\nlevel-2 block\nwith ] and ]] inside\n]==]\nlocal z = 2\n";
    let (doc, _uri, _agg) = setup_single_file(src, "leveled.lua");
    let folds = folding_range(&doc);
    let comment_folds: Vec<_> = folds
        .iter()
        .filter(|f| f.kind == Some(FoldingRangeKind::Comment))
        .collect();
    assert_eq!(
        comment_folds.len(),
        1,
        "level-N block comments should still fold, got: {:?}",
        folds,
    );
    assert_eq!(comment_folds[0].end_line, 3);
}
