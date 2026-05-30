mod test_helpers;

use mylua_lsp::syntax_kind::{kind, NodeKindExt};
use test_helpers::*;

#[test]
fn parse_simple_local() {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, "local abc = 1");
    let root = doc.root_node().unwrap();
    assert!(root.is_kind(kind::SOURCE_FILE));
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
    assert!(!doc.root_node().unwrap().has_error());
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
    assert!(!doc.root_node().unwrap().has_error());
}

#[test]
fn parse_fixture_test1() {
    let src = r#"local one = 1
math.abs(1)

---@type ...
local one
a.c.b.d
尹飞

尹飞

fsfsd

adfs.b
bbs
sfj
afdsf
ff
fsf.bbbS




local ppd = 1
local pp=1
print(ppd)
local a <closed>,b<closed> = 1
print(ppd)
print(ppd)



"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let root = doc.root_node().unwrap();
    assert!(root.is_kind(kind::SOURCE_FILE));
    // test1 has some intentionally broken lines, so we expect parse errors
}

#[test]
fn parse_fixture_test2() {
    let src = r#"local abcdef = {
    anumber = 1, 
    bstring = "string",
    cany = b,
    dtable = {
        a = 
    }
}
print(abcdef)

local cdef123 = abcdef
print(cdef123)

afjsofjao"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let root = doc.root_node().unwrap();
    assert!(root.is_kind(kind::SOURCE_FILE));
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
    assert!(!doc.root_node().unwrap().has_error());
}

#[test]
fn parse_method_call_chain() {
    let mut parser = new_parser();
    let src = "local x = obj:foo():bar():baz()";
    let doc = parse_doc(&mut parser, src);
    assert!(!doc.root_node().unwrap().has_error());
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
    assert!(!doc.root_node().unwrap().has_error());
}
