mod test_helpers;

use std::collections::HashMap;
use test_helpers::*;
use mylua_lsp::hover;

#[test]
fn hover_local_variable() {
    let src = r#"local abc = 123
print(abc)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `abc` in the second line (line 1, col 6)
    let result = hover::hover(doc, &uri, pos(1, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover should return a result for local variable `abc`");
}

#[test]
fn hover_table_literal() {
    let src = r#"local abcd = {
    anumber = 1,
    bstring = "string",
    cany = b,
    dtable = {}
}
print(abcd)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `abcd` in the print line (line 6, col 6)
    let result = hover::hover(doc, &uri, pos(6, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover on table variable should return result");
}

#[test]
fn hover_emmy_class_return_type() {
    let src = read_fixture("hover/hover1.lua");
    let (doc, uri, mut agg) = setup_single_file(&src, "hover1.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `btn1` (line 21, col 6) — should show uiButton
    let result = hover::hover(doc, &uri, pos(21, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover on btn1 should return result");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("uiButton"),
            "btn1 hover should mention uiButton, got: {}",
            content
        );
    }
}

#[test]
fn hover_chain_call() {
    let src = read_fixture("hover/hover1.lua");
    let (doc, uri, mut agg) = setup_single_file(&src, "hover1.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `btn5` (line 30, col 6) — uiButton:setX(1)
    let result = hover::hover(doc, &uri, pos(30, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover on btn5 should return result");
}

#[test]
fn hover_fixture_hover5_table_fields() {
    let src = read_fixture("hover/hover5.lua");
    let (doc, uri, mut agg) = setup_single_file(&src, "hover5.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `abcd` (line 6, col 6) — should show table fields
    let result = hover::hover(doc, &uri, pos(6, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover on abcd should return result");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("anumber") || content.contains("table"),
            "hover should show table info, got: {}",
            content
        );
    }
}

#[test]
fn hover_fixture_hover5_alias() {
    let src = read_fixture("hover/hover5.lua");
    let (doc, uri, mut agg) = setup_single_file(&src, "hover5.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `cdef` (line 8, col 6) — aliased from abcd
    let result = hover::hover(doc, &uri, pos(8, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover on cdef (alias of abcd) should return result");
}

#[test]
fn hover_no_result_on_keyword() {
    let src = "local x = 1";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `local` keyword (line 0, col 0)
    let result = hover::hover(doc, &uri, pos(0, 0), &mut agg, &docs);
    // Keywords may or may not produce hover — this mainly tests no panic
    let _ = result;
}

/// Extract the text content from a Hover result.
fn hover_content_string(h: &tower_lsp_server::ls_types::Hover) -> String {
    use tower_lsp_server::ls_types::HoverContents;
    match &h.contents {
        HoverContents::Scalar(s) => match s {
            tower_lsp_server::ls_types::MarkedString::String(s) => s.clone(),
            tower_lsp_server::ls_types::MarkedString::LanguageString(ls) => ls.value.clone(),
        },
        HoverContents::Array(arr) => arr
            .iter()
            .map(|s| match s {
                tower_lsp_server::ls_types::MarkedString::String(s) => s.clone(),
                tower_lsp_server::ls_types::MarkedString::LanguageString(ls) => ls.value.clone(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        HoverContents::Markup(m) => m.value.clone(),
    }
}
