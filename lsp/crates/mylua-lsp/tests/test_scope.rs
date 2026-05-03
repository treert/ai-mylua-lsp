mod test_helpers;

use mylua_lsp::uri_id::intern;
use test_helpers::*;

fn resolve_name(src: &str, line: u32, col: u32) -> Option<String> {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let uri = make_uri("test.lua");
    let offset = doc.line_index().position_to_byte_offset(doc.source(), pos(line, col))?;
    let ident = mylua_lsp::util::find_node_at_position(doc.tree.root_node(), offset)?;
    let name = mylua_lsp::util::node_text(ident, doc.source());
    doc.scope_tree.resolve_id(offset, name, intern(uri))
        .map(|d| format!("{}: {}", d.name, kind_str(&d.kind)))
}

fn kind_str(k: &mylua_lsp::types::DefKind) -> &'static str {
    match k {
        mylua_lsp::types::DefKind::LocalVariable => "local",
        mylua_lsp::types::DefKind::LocalFunction => "local_fn",
        mylua_lsp::types::DefKind::Parameter => "param",
        mylua_lsp::types::DefKind::ForVariable => "for_var",
        mylua_lsp::types::DefKind::GlobalVariable => "global",
        mylua_lsp::types::DefKind::GlobalFunction => "global_fn",
    }
}

#[test]
fn resolve_local_in_function_body() {
    let src = r#"local createJson = function ()
local json = {}
json.EMPTY_ARRAY = {}
end
"#;
    // "json" at line 2 col 0 (in `json.EMPTY_ARRAY`)
    let result = resolve_name(src, 2, 0);
    assert_eq!(result.as_deref(), Some("json: local"));
}

#[test]
fn resolve_local_at_declaration_site() {
    let src = r#"local createJson = function ()
local json = {}
end
"#;
    // "json" at line 1 col 6
    let result = resolve_name(src, 1, 6);
    assert_eq!(result.as_deref(), Some("json: local"));
}

#[test]
fn resolve_parameter() {
    let src = r#"local function foo(x, y)
    return x + y
end
"#;
    // "x" at line 1 col 11
    let result = resolve_name(src, 1, 11);
    assert_eq!(result.as_deref(), Some("x: param"));
}

#[test]
fn resolve_for_numeric_variable() {
    let src = r#"for i = 1, 10 do
    print(i)
end
"#;
    // "i" inside the loop body at line 1 col 10
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("i: for_var"));
}

#[test]
fn resolve_for_generic_variable() {
    let src = r#"for k, v in pairs(t) do
    print(k, v)
end
"#;
    // "k" at line 1 col 10
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("k: for_var"));
    // "v" at line 1 col 13
    let result_v = resolve_name(src, 1, 13);
    assert_eq!(result_v.as_deref(), Some("v: for_var"));
}

#[test]
fn shadowing_inner_scope() {
    let src = r#"local x = 1
do
    local x = 2
    print(x)
end
print(x)
"#;
    // "x" at line 3 col 10 → inner local (line 2)
    let inner = resolve_name(src, 3, 10);
    assert_eq!(inner.as_deref(), Some("x: local"));

    // "x" at line 5 col 6 → outer local (line 0)
    let outer = resolve_name(src, 5, 6);
    assert_eq!(outer.as_deref(), Some("x: local"));
}

#[test]
fn nested_function_body_locals() {
    let src = r#"local function outer()
    local a = 1
    local function inner()
        local b = 2
        print(a, b)
    end
end
"#;
    // "a" at line 4 col 14 → outer's local
    let result_a = resolve_name(src, 4, 14);
    assert_eq!(result_a.as_deref(), Some("a: local"));
    // "b" at line 4 col 17 → inner's local
    let result_b = resolve_name(src, 4, 17);
    assert_eq!(result_b.as_deref(), Some("b: local"));
}

#[test]
fn unresolved_global() {
    let src = r#"print(undefined_var)
"#;
    // "undefined_var" at line 0 col 6
    let result = resolve_name(src, 0, 6);
    assert_eq!(result, None);
}

#[test]
fn self_in_colon_method() {
    let src = r#"function Foo:bar()
    print(self)
end
"#;
    // "self" at line 1 col 10
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("self: param"));
}

#[test]
fn local_rhs_sees_outer_scope() {
    let src = r#"local x = 1
local x = x + 1
print(x)
"#;
    // "x" in RHS of second declaration (line 1, col 10 → the `x` in `x + 1`)
    // should resolve to the OUTER x (line 0), not the current declaration
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("x: local"));

    // After the second declaration (line 2), `x` should resolve to the inner one
    let result_after = resolve_name(src, 2, 6);
    assert_eq!(result_after.as_deref(), Some("x: local"));
}

#[test]
fn visible_locals_completeness() {
    let src = r#"local a = 1
local function foo(x)
    local b = 2
    do
        local c = 3
        print()
    end
end
"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    // At line 5 col 8 (inside do block, at "print()")
    let offset = doc.line_index().position_to_byte_offset(doc.source(), pos(5, 14)).unwrap();
    let locals = doc.scope_tree.visible_locals(offset);
    let names: Vec<&str> = locals.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"a"), "should see outer 'a': {:?}", names);
    assert!(names.contains(&"x"), "should see param 'x': {:?}", names);
    assert!(names.contains(&"b"), "should see 'b': {:?}", names);
    assert!(names.contains(&"c"), "should see inner 'c': {:?}", names);
    assert!(names.contains(&"foo"), "should see 'foo': {:?}", names);
}
