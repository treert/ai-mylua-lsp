mod test_helpers;

use test_helpers::*;

#[test]
fn parse_simple_local() {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, "local abc = 1");
    let root = doc.tree.root_node();
    assert_eq!(root.kind(), "source_file");
    assert!(!root.has_error(), "parse tree should have no errors");
}

#[test]
fn parse_function_declaration() {
    let mut parser = new_parser();
    let src = r#"
function hello(a, b)
    return a + b
end
"#;
    let doc = parse_doc(&mut parser, src);
    assert!(!doc.tree.root_node().has_error());
}

#[test]
fn parse_emmy_class_annotation() {
    let mut parser = new_parser();
    let src = r#"
---@class uiButton
local uiButton = class('uiButton')

---@return uiButton
function uiButton:setX(x)
    self.x_ = x
    return self
end
"#;
    let doc = parse_doc(&mut parser, src);
    assert!(!doc.tree.root_node().has_error());
}

#[test]
fn parse_fixture_test1() {
    let src = read_fixture("parse/test1.lua");
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, &src);
    let root = doc.tree.root_node();
    assert_eq!(root.kind(), "source_file");
    // test1.lua has some intentionally broken lines, so we expect parse errors
}

#[test]
fn parse_fixture_test2() {
    let src = read_fixture("parse/test2.lua");
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, &src);
    let root = doc.tree.root_node();
    assert_eq!(root.kind(), "source_file");
}

#[test]
fn parse_table_constructor() {
    let mut parser = new_parser();
    let src = r#"
local t = {
    a = 1,
    b = "hello",
    c = {
        d = true
    }
}
"#;
    let doc = parse_doc(&mut parser, src);
    assert!(!doc.tree.root_node().has_error());
}

#[test]
fn parse_method_call_chain() {
    let mut parser = new_parser();
    let src = "local x = obj:foo():bar():baz()";
    let doc = parse_doc(&mut parser, src);
    assert!(!doc.tree.root_node().has_error());
}

#[test]
fn parse_for_loop_variants() {
    let mut parser = new_parser();
    let src = r#"
for i = 1, 10 do
    print(i)
end
for k, v in pairs(t) do
    print(k, v)
end
"#;
    let doc = parse_doc(&mut parser, src);
    assert!(!doc.tree.root_node().has_error());
}
