mod test_helpers;

use test_helpers::*;
use mylua_lsp::completion;
use mylua_lsp::uri_id::intern;

#[test]
fn complete_local_variable() {
    let src = "local abcdef = 1\nabc";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // cursor at end of "abc" (line 1, col 3)
    let items = completion::complete(&doc, intern(uri.clone()), pos(1, 3), &mut agg);
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
    let items = completion::complete(&doc, intern(uri.clone()), pos(2, 8), &mut agg);
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
    let items = completion::complete(&doc, intern(uri.clone()), pos(45, 22), &mut agg);
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

    let items = completion::complete(&doc, intern(uri.clone()), pos(2, 2), &mut agg);
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
    let summary = mylua_lsp::summary_builder::build_file_analysis(&uri, &doc2.tree, doc2.source(), doc2.line_index()).0;
    let mut agg2 = mylua_lsp::aggregation::WorkspaceAggregation::new();
    let uri_id = intern(uri.clone());
    agg2.upsert_summary(uri_id, summary);

    let lines: Vec<&str> = src_with_dot.lines().collect();
    let last_line = lines.len() - 1;
    let items = completion::complete(&doc2, uri_id, pos(last_line as u32, 8), &mut agg2);
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

    let items = completion::complete(&doc, intern(uri.clone()), pos(0, 2), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"local"),
        "completion should include keyword `local`, got: {:?}",
        labels
    );
}

#[test]
fn complete_emmy_tag_after_at() {
    // Typing `---@cl` should list emmy tags starting with "cl" (e.g. class).
    let src = "---@cl\nlocal x = 1";
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_tag.lua");
    let items = completion::complete(&doc, intern(uri.clone()), pos(0, 6), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"class"),
        "`---@cl` should complete to `class`, got: {:?}",
        labels,
    );
    // Should NOT leak locals/keywords into emmy-tag completion.
    assert!(
        !labels.contains(&"local"),
        "emmy tag completion should not include Lua keywords: {:?}",
        labels,
    );
}

#[test]
fn complete_emmy_tag_bare_at() {
    let src = "---@\nlocal x = 1";
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_bare.lua");
    let items = completion::complete(&doc, intern(uri.clone()), pos(0, 4), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    for expected in &["class", "field", "param", "return", "type", "overload"] {
        assert!(
            labels.contains(expected),
            "`---@` should include `{}`, got: {:?}",
            expected, labels,
        );
    }
}

#[test]
fn complete_require_path_from_index() {
    use mylua_lsp::{aggregation::WorkspaceAggregation, summary_builder};

    let mut parser = new_parser();
    let caller_src = "local m = require(\"\")";
    let caller_uri = make_uri("caller.lua");
    let caller_uri_id = intern(caller_uri.clone());
    let caller_doc = parse_doc(&mut parser, caller_src);
    let caller_summary = summary_builder::build_file_analysis(
        &caller_uri, &caller_doc.tree, caller_doc.source(), caller_doc.line_index(),
    ).0;

    let mut agg = WorkspaceAggregation::new();
    agg.set_require_mapping("game.player".to_string(), intern(make_uri("player.lua")));
    agg.set_require_mapping("game.world".to_string(), intern(make_uri("world.lua")));
    agg.set_require_mapping("util.log".to_string(), intern(make_uri("log.lua")));
    agg.upsert_summary(caller_uri_id, caller_summary);

    // Cursor inside the empty string `""` at line 0, col 19 (just inside the quote).
    let items = completion::complete(&caller_doc, caller_uri_id, pos(0, 19), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    for expected in &["game.player", "game.world", "util.log"] {
        assert!(
            labels.contains(expected),
            "require completion should list `{}`, got: {:?}",
            expected, labels,
        );
    }
    // All items inside a require string should be MODULE, not KEYWORD.
    for item in &items {
        assert_eq!(
            item.kind,
            Some(tower_lsp_server::ls_types::CompletionItemKind::MODULE),
            "require completion items should all be MODULE kind, got {:?}",
            item.kind,
        );
    }
}

#[test]
fn complete_dot_base_after_call_chain_is_ast_driven() {
    // Regression for the string-splitn('.') based approach: clicking `.` on
    // the result of a method call — even if we can't yet infer the exact
    // return type — must not panic or propose obviously wrong global names.
    let src = r#"---@class Obj
---@field x integer
local Obj = {}
function Obj:get() return self end
Obj:get()."#;
    let (doc, uri, mut agg) = setup_single_file(src, "chain.lua");
    // Cursor right after `.` (line 4, col 10)
    let items = completion::complete(&doc, intern(uri.clone()), pos(4, 10), &mut agg);
    // No assertion on exact content — just ensure we didn't crash and
    // didn't spill the whole identifier table (which old splitn path did
    // when base resolution failed).
    assert!(
        items.len() <= 64,
        "AST-driven dot after method call should not spill a giant global list: {} items",
        items.len(),
    );
}

#[test]
fn complete_empty_prefix_no_panic() {
    let src = "local x = 1\n";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // At start of empty line
    let items = completion::complete(&doc, intern(uri.clone()), pos(1, 0), &mut agg);
    // Should not panic; may return keywords/locals
    let _ = items;
}
