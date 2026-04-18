mod test_helpers;

use test_helpers::*;
use mylua_lsp::summary_builder;
use mylua_lsp::symbols;
use tower_lsp_server::ls_types::SymbolKind;

/// Helper: parse + build summary + call collect_document_symbols.
fn collect(src: &str) -> Vec<tower_lsp_server::ls_types::DocumentSymbol> {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let uri = make_uri("test.lua");
    let summary = summary_builder::build_summary(&uri, &doc.tree, src.as_bytes());
    symbols::collect_document_symbols(
        doc.tree.root_node(),
        src.as_bytes(),
        Some(&summary),
    )
}

#[test]
fn symbols_function_declarations() {
    let src = r#"
function hello()
end

function world(a, b)
    return a + b
end
"#;
    let syms = collect(src);
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
    let syms = collect(src);
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"myHelper"), "should contain `myHelper`, got: {:?}", names);
}

#[test]
fn symbols_class_with_field_and_method_children() {
    // P1-4: `@class Foo` + `@field x integer` + `function Foo:m()`
    // should produce a single top-level CLASS node with two
    // children: Field `x` and Method `m`.
    let src = r#"---@class Foo
---@field x integer
Foo = {}

function Foo:m() end
"#;
    let syms = collect(src);
    let foo = syms.iter().find(|s| s.name == "Foo")
        .unwrap_or_else(|| panic!("Foo class missing, got: {:?}", syms.iter().map(|s| &s.name).collect::<Vec<_>>()));
    assert_eq!(foo.kind, SymbolKind::CLASS);
    let children = foo.children.as_ref().expect("children");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert!(names.contains(&"x"), "Foo.x as Field child, got: {:?}", names);
    assert!(names.contains(&"m"), "Foo:m as Method child, got: {:?}", names);

    // Method must be a METHOD kind (since accessed via `:`)
    let m = children.iter().find(|c| c.name == "m").unwrap();
    assert_eq!(m.kind, SymbolKind::METHOD);
    let x = children.iter().find(|c| c.name == "x").unwrap();
    assert_eq!(x.kind, SymbolKind::FIELD);

    // No stray top-level entry for the anchor assignment `Foo = {}`
    // and no stray top-level entry for the class methods.
    let top_names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(!top_names.iter().any(|n| *n == "Foo:m"));
}

#[test]
fn symbols_class_dot_methods_are_functions_not_methods() {
    // `function Foo.bar()` (dot, not colon) → FUNCTION child, not METHOD
    let src = r#"---@class Foo
Foo = {}

function Foo.bar() end
"#;
    let syms = collect(src);
    let foo = syms.iter().find(|s| s.name == "Foo").expect("Foo");
    let children = foo.children.as_ref().expect("children");
    let bar = children.iter().find(|c| c.name == "bar").expect("bar");
    assert_eq!(bar.kind, SymbolKind::FUNCTION);
}

#[test]
fn symbols_dotted_assignment_skipped() {
    // `x.foo = 1` is a field write on an existing variable, not a new
    // symbol — must not appear in the outline.
    let src = r#"local x = {}
x.foo = 1
"#;
    let syms = collect(src);
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"x"), "local x should be in outline");
    assert!(
        !names.iter().any(|n| *n == "x" && syms.iter().filter(|s| s.name == "x").count() > 1),
        "x must appear only once, got: {:?}", names,
    );
    // Critically: no "foo" or "x.foo" entry.
    assert!(
        !names.iter().any(|n| *n == "foo" || *n == "x.foo"),
        "dotted LHS must not generate an outline symbol, got: {:?}", names,
    );
}

#[test]
fn symbols_method_declaration() {
    // Pre-P1-4 behavior retained: @class anchor + methods flatten
    // correctly, class gets its method children.
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
    let syms = collect(src);
    // uiButton is a CLASS at the top level; methods are children.
    let ui = syms.iter().find(|s| s.name == "uiButton").expect("uiButton class");
    assert_eq!(ui.kind, SymbolKind::CLASS);
    let child_names: Vec<&str> = ui
        .children.as_ref().expect("children")
        .iter().map(|c| c.name.as_str()).collect();
    assert!(child_names.contains(&"setX"), "setX as child, got: {:?}", child_names);
    assert!(child_names.contains(&"setY"), "setY as child, got: {:?}", child_names);
}

#[test]
fn symbols_empty_file() {
    let syms = collect("");
    assert!(syms.is_empty(), "empty file should have no symbols");
}

#[test]
fn symbols_fixture_hover1() {
    let src = read_fixture("hover/hover1.lua");
    let syms = collect(&src);
    // hover1.lua defines uiButton (class) + setX/setY/setY1/new as its
    // children.
    let ui = syms.iter().find(|s| s.name == "uiButton");
    assert!(ui.is_some(), "should find uiButton class, got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>());
    if let Some(ui) = ui {
        let child_names: Vec<&str> = ui
            .children.as_ref().map(|v| v.iter().map(|c| c.name.as_str()).collect()).unwrap_or_default();
        assert!(
            child_names.iter().any(|n| *n == "new" || n.contains("new")),
            "uiButton should have `new` child, got: {:?}", child_names,
        );
    }
}
