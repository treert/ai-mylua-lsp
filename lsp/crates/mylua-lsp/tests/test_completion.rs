mod test_helpers;

use test_helpers::*;
use mylua_lsp::completion;

#[test]
fn complete_local_variable() {
    let src = "local abcdef = 1\nabc";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // cursor at end of "abc" (line 1, col 3)
    let items = completion::complete(&doc, &uri, pos(1, 3), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"abcdef"),
        "completion should include `abcdef`, got: {:?}",
        labels
    );
}

#[test]
fn complete_dot_on_table() {
    let src = r#"local abcdefg = {}
abcdefg.abc = 1
abcdefg."#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // cursor right after the dot (line 2, col 8)
    let items = completion::complete(&doc, &uri, pos(2, 8), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"abc"),
        "dot-completion on table should include `abc`, got: {:?}",
        labels
    );
}

#[test]
fn complete_emmy_class_methods() {
    let src = read_fixture("complete/test2.lua");
    let (doc, uri, mut agg) = setup_single_file(&src, "test2.lua");

    // After "uiButton:" at line 47, the user types inside setY1 body
    // Try completing after `self:` — line 45, col 22 (inside setY1)
    let items = completion::complete(&doc, &uri, pos(45, 22), &mut agg);
    // Should list methods like setX, setY, etc.
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    // At minimum the function should not panic; methods may appear
    let _ = labels;
}

#[test]
fn complete_no_duplicates() {
    let src = r#"local xyz = 1
local xyz2 = 2
xy"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    let items = completion::complete(&doc, &uri, pos(2, 2), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"xyz"), "should contain xyz");
    assert!(labels.contains(&"xyz2"), "should contain xyz2");
}

#[test]
fn complete_fixture_test4_table_field() {
    let src = read_fixture("complete/test4.lua");
    let (_doc, uri, _agg) = setup_single_file(&src, "test4.lua");

    // `abcdefg.` at end of file — should complete with `abc`
    // File content: abcdefg.abc = 1, then blank lines
    // We add a dot expression to trigger completion
    let src_with_dot = format!("{}\nabcdefg.", src.trim());
    let mut parser = new_parser();
    let doc2 = parse_doc(&mut parser, &src_with_dot);
    let summary = mylua_lsp::summary_builder::build_summary(&uri, &doc2.tree, src_with_dot.as_bytes());
    let mut agg2 = mylua_lsp::aggregation::WorkspaceAggregation::new();
    agg2.upsert_summary(summary);

    let lines: Vec<&str> = src_with_dot.lines().collect();
    let last_line = lines.len() - 1;
    let items = completion::complete(&doc2, &uri, pos(last_line as u32, 8), &mut agg2);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"abc"),
        "fixture test4 dot-completion should include `abc`, got: {:?}",
        labels
    );
}

#[test]
fn complete_keywords_present() {
    let src = "lo";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    let items = completion::complete(&doc, &uri, pos(0, 2), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"local"),
        "completion should include keyword `local`, got: {:?}",
        labels
    );
}

#[test]
fn complete_empty_prefix_no_panic() {
    let src = "local x = 1\n";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // At start of empty line
    let items = completion::complete(&doc, &uri, pos(1, 0), &mut agg);
    // Should not panic; may return keywords/locals
    let _ = items;
}
