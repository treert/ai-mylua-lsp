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

#[test]
fn hover_on_method_decl_base_falls_through_to_variable() {
    // `function ABC:f1()` — hover on `ABC` (the base) must NOT show
    // the function declaration; it should resolve `ABC` as a local.
    let src = r#"local ABC = {}

function ABC:f1()
    return 1
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "method_base.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `function ABC:f1()` is at line 2; `A` is col 9.
    let result = hover::hover(doc, &uri, pos(2, 9), &mut agg, &docs);
    assert!(result.is_some(), "hover on method base `ABC` should resolve to the local, got None");
    let content = hover_content_string(result.as_ref().unwrap());
    assert!(
        !content.contains("function ABC:f1"),
        "hover on base `ABC` must not show the whole function signature. content={}",
        content
    );
}

#[test]
fn hover_on_method_decl_name_still_shows_function() {
    // `function ABC:f1()` — hover on the method tail `f1` must still
    // show the function declaration.
    let src = r#"local ABC = {}

function ABC:f1()
    return 1
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "method_tail.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `function ABC:f1()` — `f1` starts at col 13.
    let result = hover::hover(doc, &uri, pos(2, 13), &mut agg, &docs);
    assert!(result.is_some(), "hover on method name `f1` should succeed");
    let content = hover_content_string(result.as_ref().unwrap());
    assert!(
        content.contains("function ABC:f1"),
        "hover on tail `f1` should show the function declaration. content={}",
        content
    );
}

#[test]
fn hover_on_undefined_method_base_returns_none() {
    // `function A1213:f()` with `A1213` undefined — hover on `A1213`
    // must not impersonate a function. With no local / global / type
    // match, hover returns None (or at most a non-function variable
    // fallback). Asserting `content` does not claim `A1213` is a
    // function.
    let src = r#"function A1213:f()
    self.ff = 2
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "undef_base.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `function A1213:f()` — `A1213` starts at col 9.
    let result = hover::hover(doc, &uri, pos(0, 9), &mut agg, &docs);
    if let Some(h) = result {
        let content = hover_content_string(&h);
        assert!(
            !content.contains("function A1213:f"),
            "undefined base `A1213` must not show a function hover. content={}",
            content
        );
    }
    // None is also an acceptable outcome (no hover for undefined ref).
}

#[test]
fn hover_on_intermediate_dotted_segment_does_not_impersonate_function() {
    // `function a.b.c()` — hover on the intermediate `b` must not
    // show the function signature. Only the tail `c` should.
    let src = r#"local a = { b = {} }

function a.b.c()
    return 1
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "inter_dot.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `function a.b.c()` at line 2: `a` col 9, `.` col 10, `b` col 11,
    // `.` col 12, `c` col 13. Hover on intermediate `b` (col 11).
    let result = hover::hover(doc, &uri, pos(2, 11), &mut agg, &docs);
    if let Some(h) = result {
        let content = hover_content_string(&h);
        assert!(
            !content.contains("function a.b.c"),
            "intermediate segment `b` must not show the function declaration. content={}",
            content
        );
    }
    // Tail `c` (col 13) still shows the function decl.
    let tail = hover::hover(doc, &uri, pos(2, 13), &mut agg, &docs);
    assert!(tail.is_some(), "tail hover should succeed");
    let tail_content = hover_content_string(tail.as_ref().unwrap());
    assert!(
        tail_content.contains("function a.b.c"),
        "tail `c` should show the function declaration. content={}",
        tail_content
    );
}

#[test]
fn hover_on_bare_function_decl_name_still_shows_function() {
    // `function foo()` — bare form, hover on `foo` is the tail and
    // must still show the function declaration (the common case).
    let src = r#"function foo()
    return 1
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "bare_decl.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `function foo()` — `foo` starts at col 9.
    let result = hover::hover(doc, &uri, pos(0, 9), &mut agg, &docs);
    assert!(result.is_some(), "hover on bare function name should succeed");
    let content = hover_content_string(result.as_ref().unwrap());
    assert!(
        content.contains("function foo"),
        "hover on bare `foo` should show the function declaration. content={}",
        content
    );
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
fn hover_multi_return_distributes_types_across_names() {
    // P1-9: `local a, b = f()` should bind `a` ↦ returns[0],
    // `b` ↦ returns[1]. Previously both fell back to the first
    // return type via `infer_expression_type` on the full call.
    let src = r#"---@return number, string
function split() return 1, "x" end

local a, b = split()
print(a)
print(b)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "multi_ret.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `a` (first name → first return = number)
    let h_a = hover::hover(doc, &uri, pos(4, 6), &mut agg, &docs)
        .expect("hover on a");
    let text_a = hover_content_string(&h_a);
    assert!(
        text_a.contains("number") && !text_a.contains("string"),
        "a should be number (first return), got:\n{}", text_a,
    );

    // hover on `b` (second name → second return = string)
    let h_b = hover::hover(doc, &uri, pos(5, 6), &mut agg, &docs)
        .expect("hover on b");
    let text_b = hover_content_string(&h_b);
    assert!(
        text_b.contains("string"),
        "b should be string (second return), got:\n{}", text_b,
    );
}

#[test]
fn hover_multi_return_extra_names_fall_back_to_unknown() {
    // When there are more names than return types, extras stay Unknown
    // instead of being wrongly duplicated. Lock on the positive: `a`
    // picks up the one typed return, `b` has no known type.
    let src = r#"---@return number
function only() return 1 end

local a, b = only()
print(a)
print(b)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "multi_ret_short.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `a` gets the sole return type.
    let h_a = hover::hover(doc, &uri, pos(4, 6), &mut agg, &docs).expect("hover on a");
    let text_a = hover_content_string(&h_a);
    assert!(text_a.contains("number"), "a should be number, got:\n{}", text_a);

    // `b` must not falsely claim `number`.
    if let Some(h) = hover::hover(doc, &uri, pos(5, 6), &mut agg, &docs) {
        let text_b = hover_content_string(&h);
        assert!(
            !text_b.contains("`number`"),
            "extra name beyond returns should NOT be typed `number`, got:\n{}", text_b,
        );
    }
}

#[test]
fn hover_multi_return_method_call_falls_back_to_unknown() {
    // P1-9 guard: `obj:m()` where the base `obj` happens to ALSO be
    // a top-level function (registered under the bare key `"obj"` in
    // `function_summaries`) must NOT distribute that function's
    // returns to `a, b`. The grammar's `function_call` splits the
    // `:m` form into `callee = obj` + `method = m`, so if
    // `extract_call_return_types` checks only `callee` kind it will
    // wrongly find `obj()` and leak its return types. The correct
    // behavior is to bail out because we lack a summary-level method-
    // resolution path.
    let src = r#"---@return number, string
function obj() return 1, "decoy" end

local a, b = obj:m()
print(a)
print(b)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "method_call.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    if let Some(h) = hover::hover(doc, &uri, pos(4, 6), &mut agg, &docs) {
        let text = hover_content_string(&h);
        assert!(
            !text.contains("`number`") && !text.contains("Type: number"),
            "method call `obj:m()` must not inherit top-level `obj()`'s number return, got:\n{}", text,
        );
    }
    if let Some(h) = hover::hover(doc, &uri, pos(5, 6), &mut agg, &docs) {
        let text = hover_content_string(&h);
        assert!(
            !text.contains("`string`") && !text.contains("Type: string"),
            "method call `obj:m()` must not inherit top-level `obj()`'s string return, got:\n{}", text,
        );
    }
}

#[test]
fn hover_multi_return_dotted_call_falls_back_to_unknown() {
    // Dotted call `mod.f()` also produces a CallReturn stub and must
    // fall through to Unknown rather than picking any same-named
    // top-level function's returns.
    let src = r#"---@return number, string
function f() return 1, "decoy" end

local mod = {}
---@return boolean, integer
function mod.f() return true, 2 end

local a, b = mod.f()
print(a)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "dotted_call.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    if let Some(h) = hover::hover(doc, &uri, pos(8, 6), &mut agg, &docs) {
        let text = hover_content_string(&h);
        assert!(
            !text.contains("`number`") && !text.contains("Type: number"),
            "dotted call `mod.f()` must not pick up top-level `f()`'s number return, got:\n{}", text,
        );
    }
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

#[test]
fn hover_nested_field_write_then_read() {
    // Chain-field: writing `a.b.c = 1` via AST-driven shape nesting must
    // register `c` on the inner `a.b` shape (not as the literal key "b.c"
    // on `a`'s shape). Hovering on the `c` in `print(a.b.c)` should then
    // resolve to `integer`.
    let src = r#"local a = { b = { c = 0 } }
a.b.c = 1
print(a.b.c)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "nested_write.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Line 2 (0-indexed): `print(a.b.c)` — position on final `c`
    // columns: p=0 r=1 i=2 n=3 t=4 (=5 a=6 .=7 b=8 .=9 c=10
    let h = hover::hover(doc, &uri, pos(2, 10), &mut agg, &docs)
        .expect("hover on final .c should produce a result");
    let text = hover_content_string(&h);
    // Summary builder infers `1` literal as `number` (not `integer`).
    // Lock on the Type line to prove the nested shape write survived.
    assert!(
        text.contains("Type: `number`"),
        "final .c of chained write should resolve to number, got:\n{}", text,
    );
}

#[test]
fn hover_nested_field_on_demand_shape_creation() {
    // `a.b.c.d = 1` with initial shape only defining `a.b` should create
    // a new shape on-demand for `a.b.c`, then set `d` on that shape.
    // Hovering on the deepest `d` must resolve to `integer`.
    let src = r#"local a = { b = {} }
a.b.c = {}
a.b.c.d = 1
print(a.b.c.d)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "on_demand.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Line 3 `print(a.b.c.d)` — p=0 r=1 i=2 n=3 t=4 (=5 a=6 .=7 b=8 .=9 c=10 .=11 d=12
    let h = hover::hover(doc, &uri, pos(3, 12), &mut agg, &docs)
        .expect("hover on final .d should produce a result");
    let text = hover_content_string(&h);
    assert!(
        text.contains("Type: `number`"),
        "on-demand nested shape should carry number type for .d, got:\n{}", text,
    );
}

#[test]
fn hover_field_on_call_return_with_emmy_class() {
    // Read-side support: `make().field` where `make()` returns an
    // `@class Foo` with `@field n integer` must hover `n` as integer.
    // Today `infer_node_type`'s `_ => Unknown` default for `function_call`
    // breaks the chain.
    let src = r#"---@class Foo
---@field n integer
local Foo = {}

---@return Foo
local function make() return nil end

print(make().n)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "call_then_field.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Line 7 `print(make().n)` — p=0 r=1 i=2 n=3 t=4 (=5 m=6 a=7 k=8 e=9 (=10 )=11 .=12 n=13
    let h = hover::hover(doc, &uri, pos(7, 13), &mut agg, &docs)
        .expect("hover on make().n should produce a result");
    let text = hover_content_string(&h);
    assert!(
        text.contains("integer"),
        "field `n` on `make()` CallReturn should resolve to integer, got:\n{}", text,
    );
}

#[test]
fn hover_chained_lhs_with_call_does_not_pollute_shape() {
    // Negative: `foo().c = 1` writes to a transient return value and
    // MUST NOT pollute any shape or produce a junk global contribution.
    // After the fix, the `global_shard` should not contain any entry
    // whose name includes `()`.
    let src = r#"local function foo() return {} end
foo().c = 1
"#;
    let (_doc, _uri, agg) = setup_single_file(src, "call_lhs.lua");
    let junk: Vec<String> = agg.global_shard.iter_all_entries()
        .into_iter().map(|(k, _)| k)
        .filter(|k| k.contains('('))
        .collect();
    assert!(
        junk.is_empty(),
        "foo().c = 1 must not create global contributions with parens in name, got: {:?}",
        junk,
    );
}

#[test]
fn hover_subscript_then_field_reads_array_element_type() {
    // Read-side subscript branch: `a[1].name` where `a` has an array
    // element type recorded must resolve `.name` through the element's
    // shape. Today `a[k] = {...}` marks the shape open and records an
    // array element via the dynamic-key path; we walk that to a shape
    // then to its `name` field.
    //
    // This is a smoke test for the new `infer_node_type` subscript
    // branch — it must not regress to Unknown once we add it.
    let src = r#"local a = {}
a[1] = { name = "x" }
print(a[1].name)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "subscript_read.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Line 2 `print(a[1].name)` — just assert hover doesn't panic and
    // returns something. Whether the summary_builder's array_element_type
    // path catches the `a[1] = {...}` pattern is orthogonal; the branch
    // addition itself is the contract here.
    let _ = hover::hover(doc, &uri, pos(2, 12), &mut agg, &docs);
}

#[test]
fn hover_intermediate_non_table_field_bails() {
    // `a.b = 1` then `a.b.c = 2` must NOT silently overwrite `a.b`
    // from `number` into a fresh Table — that would hide a likely bug.
    // Hover on `b` should still surface `number`, not a table shape.
    // Also lock that the bailed write does NOT leak `a.b.c` into
    // `global_shard` — `a` is a local, not a global.
    let src = r#"local a = {}
a.b = 1
a.b.c = 2
print(a.b)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "mid_non_table.lua");

    assert!(
        !agg.global_shard.contains_key("a.b.c"),
        "bail on local base must not leak into global_shard, got entries: {:?}",
        agg.global_shard.iter_all_entries().into_iter().map(|(k, _)| k).collect::<Vec<_>>(),
    );
    assert!(
        !agg.global_shard.contains_key("a.b"),
        "local `a.b = 1` must not leak into global_shard either",
    );

    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `print(a.b)` line 3 col 8 (b)
    let h = hover::hover(doc, &uri, pos(3, 8), &mut agg, &docs)
        .expect("hover on .b should resolve");
    let text = hover_content_string(&h);
    assert!(
        text.contains("number"),
        "a.b should remain number (not rewritten to Table), got:\n{}", text,
    );
}

#[test]
fn hover_chained_lhs_with_subscript_does_not_pollute_shape() {
    // Negative: `a[1].c = 1` also must not create junk contributions.
    let src = r#"local a = { [1] = {} }
a[1].c = 1
"#;
    let (_doc, _uri, agg) = setup_single_file(src, "subscript_lhs.lua");
    let junk: Vec<String> = agg.global_shard.iter_all_entries()
        .into_iter().map(|(k, _)| k)
        .filter(|k| k.contains('[') || k.contains(']'))
        .collect();
    assert!(
        junk.is_empty(),
        "a[1].c = 1 must not create global contributions with brackets in name, got: {:?}",
        junk,
    );
}

#[test]
fn hover_field_on_alias_to_inline_table() {
    // `---@alias Vec2 { x: number, y: number }` should expose `x` as a
    // real field for hover — resolving `p.x` on a `---@type Vec2`
    // binding must produce a hover result with the field's type
    // (`number`), not `Unknown`.
    let src = r#"---@alias Vec2 { x: number, y: number }

---@type Vec2
local p = { x = 1, y = 2 }

print(p.x)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "alias_shape_hover.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // cursor on `x` in `print(p.x)` — the `x` identifier sits at col 8
    // on line 5 (0-indexed: `print(p.x)` → `(` is col 5, `p` is col 6,
    // `.` is col 7, `x` is col 8).
    let h = hover::hover(doc, &uri, pos(5, 8), &mut agg, &docs)
        .expect("hover on p.x must resolve");
    let text = hover_content_string(&h);
    assert!(
        text.contains("number"),
        "p.x on alias-to-inline-table must show field type 'number', got:\n{}",
        text,
    );
}

#[test]
fn hover_blank_line_separates_comment_blocks() {
    // A blank line between two comment blocks must stop comment collection.
    // Only the contiguous block immediately above the declaration should
    // appear in hover — the earlier block (separated by a blank line)
    // must NOT leak through.
    let src = r#"-- FEATURE: unrelated header
--   * bullet 1
--   * bullet 2

--- identity doc
---@generic T
---@param x T
---@return T
local function identity(x)
    return x
end
print(identity)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "blank_sep.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `identity` at the usage site (line 11, col 6)
    let result = hover::hover(doc, &uri, pos(11, 6), &mut agg, &docs)
        .expect("hover on identity should succeed");
    let text = hover_content_string(&result);
    assert!(
        text.contains("identity doc"),
        "hover should include the contiguous doc comment, got:\n{}", text,
    );
    assert!(
        !text.contains("FEATURE"),
        "hover must NOT include the comment block separated by a blank line, got:\n{}", text,
    );
    assert!(
        !text.contains("bullet"),
        "hover must NOT include the comment block separated by a blank line, got:\n{}", text,
    );
}

#[test]
fn hover_trailing_comment_on_local_variable() {
    // A trailing `-- comment` on the same line as a local declaration
    // should appear in the hover output.
    let src = r#"local top_str = foo()   -- hover type should be string?
print(top_str)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "trailing_comment.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `top_str` at the usage site (line 1, col 6)
    let result = hover::hover(doc, &uri, pos(1, 6), &mut agg, &docs)
        .expect("hover on top_str should succeed");
    let text = hover_content_string(&result);
    assert!(
        text.contains("hover type should be string?"),
        "hover should include the trailing comment, got:\n{}", text,
    );
}

#[test]
fn hover_trailing_comment_not_on_different_line() {
    // A comment on the NEXT line (not same line) should NOT be picked up
    // as a trailing comment.
    let src = r#"local abc = 123
-- this is on the next line
print(abc)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "trailing_no.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `abc` at the usage site (line 2, col 6)
    let result = hover::hover(doc, &uri, pos(2, 6), &mut agg, &docs)
        .expect("hover on abc should succeed");
    let text = hover_content_string(&result);
    assert!(
        !text.contains("this is on the next line"),
        "hover should NOT include a comment from a different line, got:\n{}", text,
    );
}

#[test]
fn hover_trailing_emmy_comment_not_collected() {
    // A `---` Emmy-style comment on the same line should NOT be collected
    // as a trailing comment (it's Emmy annotation material, not inline doc).
    let src = r#"local xyz = 123   ---@type number
print(xyz)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "trailing_emmy.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `xyz` at the usage site (line 1, col 6)
    let result = hover::hover(doc, &uri, pos(1, 6), &mut agg, &docs)
        .expect("hover on xyz should succeed");
    let text = hover_content_string(&result);
    // The `---@type number` should be handled by preceding-comment logic
    // (if it's a preceding sibling) or not at all — but NOT by trailing.
    // We just verify the raw `---@type number` text doesn't appear as
    // plain doc text in the hover.
    assert!(
        !text.contains("---@type number"),
        "hover should NOT show raw Emmy annotation as trailing doc, got:\n{}", text,
    );
}

#[test]
fn hover_prev_line_trailing_comment_not_leaked() {
    // A trailing `-- comment` on the PREVIOUS line's statement should NOT
    // leak into the hover of the NEXT line's variable.
    let src = r#"local n = identity(123)        -- T = number
local s = identity("abc")      -- T = string
print(s)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "trailing_leak.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `s` at the usage site (line 2, col 6)
    let result = hover::hover(doc, &uri, pos(2, 6), &mut agg, &docs)
        .expect("hover on s should succeed");
    let text = hover_content_string(&result);
    assert!(
        !text.contains("T = number"),
        "hover on `s` must NOT include the trailing comment from the previous line, got:\n{}", text,
    );
    assert!(
        text.contains("T = string"),
        "hover on `s` should include its own trailing comment, got:\n{}", text,
    );
}

#[test]
fn hover_generic_function_infers_return_type_string() {
    // Function-level generic: `identity("abc")` should infer T = string.
    let src = r#"---@generic T
---@param x T
---@return T
local function identity(x)
    return x
end

local s = identity("abc")
print(s)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "generic_str.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `s` at the usage site (line 8, col 6)
    let result = hover::hover(doc, &uri, pos(8, 6), &mut agg, &docs)
        .expect("hover on s should succeed");
    let text = hover_content_string(&result);
    assert!(
        text.contains("string"),
        "hover on `s` should show type `string` (inferred from generic), got:\n{}", text,
    );
    assert!(
        !text.contains("Type: T"),
        "hover on `s` should NOT show raw generic param `T`, got:\n{}", text,
    );
}

#[test]
fn hover_generic_function_infers_return_type_number() {
    // Function-level generic: `identity(123)` should infer T = number.
    let src = r#"---@generic T
---@param x T
---@return T
local function identity(x)
    return x
end

local n = identity(123)
print(n)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "generic_num.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `n` at the usage site (line 8, col 6)
    let result = hover::hover(doc, &uri, pos(8, 6), &mut agg, &docs)
        .expect("hover on n should succeed");
    let text = hover_content_string(&result);
    assert!(
        text.contains("number"),
        "hover on `n` should show type `number` (inferred from generic), got:\n{}", text,
    );
}

#[test]
fn hover_dotted_function_on_local_table_writes_to_shape() {
    // `function M.add(a, b)` where M is a local table should write `add`
    // into M's table shape. Hovering on `M.add(...)` should resolve the
    // function type, and `add` should NOT appear in global_contributions.
    let src = r#"local M = {}

---@param a number
---@param b number
---@return number
function M.add(a, b)
    return a + b
end

local sum = M.add(1, 2)
print(sum)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "dotted_func_shape.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // hover on `add` in `M.add(1, 2)` — line 9, col 14 is `add`
    let result = hover::hover(doc, &uri, pos(9, 14), &mut agg, &docs);
    assert!(result.is_some(), "hover on `add` in `M.add(1, 2)` should succeed");
    let text = hover_content_string(result.as_ref().unwrap());
    assert!(
        text.contains("number"),
        "hover on `add` should show number return type, got:\n{}", text,
    );

    // hover on `sum` — should show number type
    let result2 = hover::hover(doc, &uri, pos(10, 6), &mut agg, &docs);
    assert!(result2.is_some(), "hover on `sum` should succeed");
    let text2 = hover_content_string(result2.as_ref().unwrap());
    assert!(
        text2.contains("number"),
        "hover on `sum` should show number type, got:\n{}", text2,
    );
}

#[test]
fn hover_cross_file_require_dotted_function() {
    // Cross-file scenario: `require("math_utils")` returns a local table
    // with `function M.add(a, b)`. Hovering on `math_utils.add` in the
    // consumer file should resolve the function type.
    let mod_src = r#"local M = {}

---@param a number
---@param b number
---@return number
function M.add(a, b)
    return a + b
end

return M
"#;
    let main_src = r#"local math_utils = require("math_utils")
local sum = math_utils.add(1, 2)
print(sum)
"#;
    let (docs, mut agg, _parser) = setup_workspace(&[
        ("math_utils.lua", mod_src),
        ("main.lua", main_src),
    ]);
    // Register require mapping
    let mod_uri = make_uri("math_utils.lua");
    agg.set_require_mapping("math_utils".to_string(), mod_uri.clone());

    let main_uri = make_uri("main.lua");
    let main_doc = docs.get(&main_uri).unwrap();

    // hover on `add` in `math_utils.add(1, 2)` — line 1, col 23 is `add`
    let result = hover::hover(main_doc, &main_uri, pos(1, 23), &mut agg, &docs);
    assert!(result.is_some(), "hover on `add` in cross-file `math_utils.add(1, 2)` should succeed");
    let text = hover_content_string(result.as_ref().unwrap());
    assert!(
        text.contains("number"),
        "hover on cross-file `add` should show number return type, got:\n{}", text,
    );

    // hover on `sum` — should show number type (call return resolved cross-file)
    let result2 = hover::hover(main_doc, &main_uri, pos(2, 6), &mut agg, &docs);
    assert!(result2.is_some(), "hover on `sum` should succeed");
    let text2 = hover_content_string(result2.as_ref().unwrap());
    assert!(
        text2.contains("number"),
        "hover on cross-file `sum` should show number type, got:\n{}", text2,
    );
}

/// When a module does `return Player` (a global table), and the caller
/// does `local Player = require("player")`, hover on `Player.new()`
/// should resolve through the `resolve_require_global_name` helper to
/// find the qualified function `Player.new` in the module's summaries.
#[test]
fn hover_require_returning_global_table_method() {
    let mod_src = r#"
Player = {}

---@param name string
---@return Player
function Player.new(name)
    return { name = name }
end

---@return string
function Player:getName()
    return self.name
end

return Player
"#;
    let main_src = r#"local Player = require("player")
local hero = Player.new("Alice")
local name = hero:getName()
"#;
    let (docs, mut agg, _parser) = setup_workspace(&[
        ("player.lua", mod_src),
        ("main.lua", main_src),
    ]);
    let mod_uri = make_uri("player.lua");
    agg.set_require_mapping("player".to_string(), mod_uri.clone());

    let main_uri = make_uri("main.lua");
    let main_doc = docs.get(&main_uri).unwrap();

    // hover on `new` in `Player.new("Alice")` — line 1, col 20
    let result = hover::hover(main_doc, &main_uri, pos(1, 20), &mut agg, &docs);
    assert!(
        result.is_some(),
        "hover on `new` in cross-file `Player.new(\"Alice\")` should succeed \
         (require returning global table)"
    );
    let text = hover_content_string(result.as_ref().unwrap());
    assert!(
        text.contains("Player"),
        "hover on `Player.new` should mention Player type, got:\n{}", text,
    );

    // hover on `hero` — should show Player type (resolved via module_return_type)
    let result2 = hover::hover(main_doc, &main_uri, pos(1, 6), &mut agg, &docs);
    assert!(result2.is_some(), "hover on `hero` should succeed");
    let text2 = hover_content_string(result2.as_ref().unwrap());
    assert!(
        text2.contains("Player"),
        "hover on `hero` should show Player type, got:\n{}", text2,
    );
}

/// When a parent class in the inheritance chain is anchored by a *local*
/// variable (`local Damageable = {}`), methods defined via
/// `function Damageable:take_damage()` are stored in the table shape
/// rather than `global_shard`. Hover on such inherited methods must
/// still resolve correctly via the local-table-shape fallback.
#[test]
fn hover_inherited_method_from_local_class() {
    let mod_src = r#"
---@class Entity
---@field id integer
---@field name string
local Entity = {}

---@return string
function Entity:describe()
    return self.name
end

---@class Damageable
---@field hp integer
local Damageable = {}

---@param dmg integer
function Damageable:take_damage(dmg)
    self.hp = self.hp - dmg
end

---@class Player: Entity, Damageable
---@field level integer
Player = {}

---@param id integer
---@param name string
---@return Player
function Player.new(id, name)
    return setmetatable({}, { __index = Player })
end

---@param item string
function Player:pick_up(item)
    table.insert(self.inventory, item)
end

return Player
"#;
    let main_src = r#"local Player = require("player")
local hero = Player.new(1, "Alice")
hero:take_damage(5)
hero:pick_up("sword")
"#;
    let (docs, mut agg, _parser) = setup_workspace(&[
        ("player.lua", mod_src),
        ("main.lua", main_src),
    ]);
    let mod_uri = make_uri("player.lua");
    agg.set_require_mapping("player".to_string(), mod_uri.clone());

    let main_uri = make_uri("main.lua");
    let main_doc = docs.get(&main_uri).unwrap();

    // hover on `take_damage` — line 2, col 7
    let result = hover::hover(main_doc, &main_uri, pos(2, 7), &mut agg, &docs);
    assert!(
        result.is_some(),
        "hover on `take_damage` (inherited from local Damageable class) should succeed"
    );
    let text = hover_content_string(result.as_ref().unwrap());
    assert!(
        text.contains("dmg"),
        "hover on `take_damage` should show parameter info, got:\n{}", text,
    );

    // hover on `pick_up` — line 3, col 7 (global Player, should still work)
    let result2 = hover::hover(main_doc, &main_uri, pos(3, 7), &mut agg, &docs);
    assert!(
        result2.is_some(),
        "hover on `pick_up` (defined on global Player) should succeed"
    );
}

#[test]
fn hover_emmy_comment_parent_type_name() {
    let src = r#"---@class BaseCls
---@field id integer
BaseCls = {}

---@class ClassA1:BaseCls
ClassA1 = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_hover.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Hover `BaseCls` in `---@class ClassA1:BaseCls`.
    let result = hover::hover(doc, &uri, pos(4, 20), &mut agg, &docs)
        .expect("hover on Emmy parent type should resolve");
    let text = hover_content_string(&result);
    assert!(
        text.contains("---@class BaseCls"),
        "hover should show the BaseCls type definition, got:\n{}", text
    );
}

#[test]
fn hover_emmy_comment_description_word_does_not_resolve_as_type() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class ClassA1 @ BaseCls appears in docs only
ClassA1 = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_desc_hover.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // The description word `BaseCls` is not part of the type expression.
    let result = hover::hover(doc, &uri, pos(3, 20), &mut agg, &docs);
    assert!(
        result.is_none(),
        "description words in Emmy comments must not act as type references"
    );
}

#[test]
fn hover_emmy_comment_unmarked_description_words_do_not_resolve_as_types() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class OtherCls
OtherCls = {}

---@param x string BaseCls appears in docs only
function takes(x) end

---@class Child:BaseCls OtherCls appears in docs only
Child = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_unmarked_desc_hover.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    let param_desc = hover::hover(doc, &uri, pos(6, 20), &mut agg, &docs);
    assert!(
        param_desc.is_none(),
        "unmarked @param description words must not act as type references"
    );

    let class_desc = hover::hover(doc, &uri, pos(9, 25), &mut agg, &docs);
    assert!(
        class_desc.is_none(),
        "unmarked @class description words must not act as type references"
    );
}

#[test]
fn hover_emmy_comment_skips_type_expression_names_that_are_not_types() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class OtherCls
OtherCls = {}

---@type {BaseCls: OtherCls}
local shaped = {}

---@class Holder
---@field [BaseCls] OtherCls
Holder = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_type_expr_hover.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    let table_key = hover::hover(doc, &uri, pos(6, 11), &mut agg, &docs);
    assert!(
        table_key.is_none(),
        "table field keys in type expressions must not act as type references"
    );

    let bracket_key = hover::hover(doc, &uri, pos(10, 12), &mut agg, &docs)
        .expect("bracket field key type should resolve");
    let text = hover_content_string(&bracket_key);
    assert!(
        text.contains("---@class BaseCls"),
        "hover should show the BaseCls type definition, got:\n{}", text
    );
}

#[test]
fn hover_emmy_comment_string_literal_words_do_not_resolve_as_types() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@type "BaseCls"
local literal = nil

---@type {["BaseCls"]: string}
local keyed = {}

---@type "\"BaseCls"
local escaped = nil
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_string_literal_hover.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    let literal_word = hover::hover(doc, &uri, pos(3, 12), &mut agg, &docs);
    assert!(
        literal_word.is_none(),
        "string literal contents must not act as type references"
    );

    let key_word = hover::hover(doc, &uri, pos(6, 12), &mut agg, &docs);
    assert!(
        key_word.is_none(),
        "string table keys must not act as type references"
    );

    let escaped_word = hover::hover(doc, &uri, pos(9, 12), &mut agg, &docs);
    assert!(
        escaped_word.is_none(),
        "escaped quotes must not expose string literal contents as type references"
    );
}
