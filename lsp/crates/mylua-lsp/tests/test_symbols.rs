mod test_helpers;

use test_helpers::*;
use mylua_lsp::symbols;

#[test]
fn symbols_function_declarations() {
    let src = r#"
function hello()
end

function world(a, b)
    return a + b
end
"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let syms = symbols::collect_document_symbols(doc.tree.root_node(), src.as_bytes());
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"hello"), "should contain function `hello`, got: {:?}", names);
    assert!(names.contains(&"world"), "should contain function `world`, got: {:?}", names);
}

#[test]
fn symbols_local_function() {
    let src = r#"
local function myHelper()
    return true
end
"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let syms = symbols::collect_document_symbols(doc.tree.root_node(), src.as_bytes());
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"myHelper"), "should contain `myHelper`, got: {:?}", names);
}

#[test]
fn symbols_method_declaration() {
    let src = r#"
---@class uiButton
local uiButton = class('uiButton')

function uiButton:setX(x)
    self.x_ = x
    return self
end

function uiButton:setY(y)
    self.y_ = y
    return self
end
"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let syms = symbols::collect_document_symbols(doc.tree.root_node(), src.as_bytes());
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("setX")),
        "should contain method `setX`, got: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n.contains("setY")),
        "should contain method `setY`, got: {:?}",
        names
    );
}

#[test]
fn symbols_empty_file() {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, "");
    let syms = symbols::collect_document_symbols(doc.tree.root_node(), "".as_bytes());
    assert!(syms.is_empty(), "empty file should have no symbols");
}

#[test]
fn symbols_fixture_hover1() {
    let src = read_fixture("hover/hover1.lua");
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, &src);
    let syms = symbols::collect_document_symbols(doc.tree.root_node(), src.as_bytes());
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    // hover1.lua defines uiButton.new, uiButton:setX, uiButton:setY, setYa, uiButton:setY1
    assert!(
        names.iter().any(|n| n.contains("new")),
        "should find uiButton.new, got: {:?}",
        names
    );
}
