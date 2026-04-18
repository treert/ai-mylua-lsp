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

#[test]
fn hover_block_comment_on_class() {
    let src = r#"--[[
Misc System Library
]]
---@class UMiscSystemLibrary
UMiscSystemLibrary = {}
local x = UMiscSystemLibrary"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_block.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `UMiscSystemLibrary` at the assignment (line 4, col 0)
    let result = hover::hover(doc, &uri, pos(4, 0), &mut agg, &docs);
    assert!(result.is_some(), "hover should return a result");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("Misc System Library"),
            "hover should include block comment text, got: {}",
            content
        );
    }
}

#[test]
fn hover_at_function_declaration_site() {
    let src = r#"---@class MyObj
MyObj = {}
--[[
Do something useful
]]
---@param x string
---@return number
function MyObj.doSomething(x) end"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_decl.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `doSomething` in the function declaration (line 7, col 15)
    let result = hover::hover(doc, &uri, pos(7, 15), &mut agg, &docs);
    assert!(result.is_some(), "hover should return a result at function declaration site");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("Do something useful"),
            "hover at declaration should include block comment, got: {}",
            content
        );
        assert!(
            content.contains("@param") || content.contains("x"),
            "hover at declaration should include param info, got: {}",
            content
        );
    }
}

#[test]
fn hover_at_simple_function_declaration() {
    let src = r#"--- A helpful utility function
---@param name string
function greet(name) end"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_decl2.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `greet` in the function declaration (line 2, col 9)
    let result = hover::hover(doc, &uri, pos(2, 9), &mut agg, &docs);
    assert!(result.is_some(), "hover should return result at simple function declaration");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("A helpful utility function"),
            "hover at declaration should include doc comment, got: {}",
            content
        );
    }
}

#[test]
fn hover_block_comment_on_function() {
    let src = r#"--[[
Do something useful
]]
---@param x string
---@return number
function doSomething(x) end

local y = doSomething("hello")"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_block_fn.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `doSomething` at the call site (line 7, col 10)
    let result = hover::hover(doc, &uri, pos(7, 10), &mut agg, &docs);
    assert!(result.is_some(), "hover should return a result for function");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("Do something useful"),
            "hover should include block comment text for function, got: {}",
            content
        );
    }
}

#[test]
fn hover_plain_comment_shown_as_doc() {
    let src = r#"-- a local x
---@type string
local x = 1
print(x)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_plain_comment.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `x` at the usage site (line 3, col 6)
    let result = hover::hover(doc, &uri, pos(3, 6), &mut agg, &docs);
    assert!(result.is_some(), "hover should return result for x");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("a local x"),
            "hover should include plain -- comment as doc, got: {}",
            content
        );
        assert!(
            content.contains("local variable"),
            "hover should show local variable kind, got: {}",
            content
        );
    }
}

#[test]
fn hover_dotted_base_shows_local_not_field() {
    let src = r#"---@class Foo
---@field bar integer
local x = {}
x.bar = 1"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_dotted_base.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `x` in `x.bar = 1` (line 3, col 0) — should show local variable, not field
    let result = hover::hover(doc, &uri, pos(3, 0), &mut agg, &docs);
    assert!(result.is_some(), "hover should return result for base x");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("local variable"),
            "hovering on base 'x' in 'x.bar' should show local variable, got: {}",
            content
        );
        assert!(
            !content.contains("(field) x"),
            "should NOT show x as a field, got: {}",
            content
        );
    }
}

#[test]
fn hover_middle_field_in_chain_ast_driven() {
    // Regression for Bug 7: clicking the middle identifier `b` in `a.b.c`
    // used to rely on string splitn('.') which picked the wrong base.
    // With AST-driven hover, clicking `b` should navigate via the
    // enclosing `variable` node whose `field` is `b`.
    let src = r#"---@class Inner
---@field c integer
---@class Outer
---@field b Inner

---@type Outer
local a = {}
local _ = a.b.c"#;
    let (doc, uri, mut agg) = setup_single_file(src, "chain.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on the middle `b` in `a.b.c` (line 7, col 12)
    let result = hover::hover(doc, &uri, pos(7, 12), &mut agg, &docs);
    assert!(result.is_some(), "hover on middle field `b` should return a result");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("Inner") || content.contains("(field)"),
            "middle field hover should mention the `Inner` type or report it as a field, got: {}",
            content,
        );
    }
}

#[test]
fn hover_dotted_field_still_works() {
    let src = r#"---@class Baz
---@field qux integer
local obj = {}
obj.qux = 1"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test_dotted_field.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `qux` in `obj.qux = 1` (line 3, col 4) — should resolve the field
    let result = hover::hover(doc, &uri, pos(3, 4), &mut agg, &docs);
    assert!(result.is_some(), "hover should return result for field qux");
    if let Some(h) = &result {
        let content = hover_content_string(h);
        assert!(
            content.contains("integer") || content.contains("qux"),
            "hovering on field 'qux' should show field info, got: {}",
            content
        );
    }
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

#[test]
fn hover_local_anonymous_function_shows_params() {
    // P1-8: `local f = function(a, b) end` — hover on `f` should show
    // the full `fun(a, b)` signature (previously was empty `fun()`).
    let src = "local f = function(a, b) return a + b end\nprint(f)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "hover_anon.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Hover on `f` in `print(f)` — line 1, col 6
    let result = hover::hover(doc, &uri, pos(1, 6), &mut agg, &docs)
        .expect("hover should return something for local f");
    let text = hover_content_string(&result);
    // Lock on the formatted signature `fun(a, b)` rather than loose
    // `a` / `b` substrings — those also appear in the decl-line code
    // block regardless of whether the anon-function sig was derived.
    assert!(
        text.contains("fun(a, b)"),
        "hover should display formatted signature `fun(a, b)`, got:\n{}", text,
    );
}

#[test]
fn hover_local_anonymous_function_with_emmy_types() {
    // P1-8: Emmy annotations on the enclosing `local` statement should
    // enrich hover output for the anonymous function binding.
    let src = r#"---@param a number
---@param b string
---@return boolean
local f = function(a, b) return true end
print(f)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "hover_anon_emmy.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    let result = hover::hover(doc, &uri, pos(4, 6), &mut agg, &docs)
        .expect("hover on Emmy-annotated anon function should resolve");
    let text = hover_content_string(&result);
    // The full Emmy-merged signature should appear in the "Type:"
    // line — locking on `fun(a: number, b: string): boolean` ensures
    // the new `format_resolved_type` specialization is actually being
    // used (the substrings `number`, `string`, `boolean` would also
    // come through the Emmy-comment markdown block regardless).
    assert!(
        text.contains("fun(a: number, b: string): boolean"),
        "hover Type should be fully formatted signature, got:\n{}", text,
    );
}
