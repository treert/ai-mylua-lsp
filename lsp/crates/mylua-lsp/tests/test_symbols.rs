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
    let summary = summary_builder::build_summary(&uri, &doc.tree, doc.source(), doc.line_index());
    symbols::collect_document_symbols(
        doc.tree.root_node(),
        doc.source(),
        Some(&summary),
        doc.line_index(),
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

/// `selection_range` for `---@class Foo` must point to just the `Foo`
/// identifier — not the whole line or the anchor statement. This is
/// the outline's click target; pointing at a wider range makes the
/// client highlight unhelpful context.
#[test]
fn symbols_class_name_range_is_precise() {
    let src = r#"---@class Foo
Foo = {}
"#;
    let syms = collect(src);
    let class = syms.iter().find(|s| s.name == "Foo").expect("Foo class present");
    let sel = class.selection_range;
    let range = class.range;
    // The selection_range should be narrower than the full anchor
    // range, pointing just at the identifier.
    assert!(
        sel.start.line == 0 && sel.end.line == 0,
        "Foo name_range should sit on line 0 (the ---@class line), got: {:?}",
        sel,
    );
    // And it should end at the end of `Foo` — 3 UTF-16 units past its start.
    assert_eq!(
        sel.end.character - sel.start.character,
        3,
        "selection_range should span exactly `Foo`, got {:?}", sel,
    );
    // Meanwhile, `range` covers the anchor statement on line 1.
    assert!(
        range.end.line >= 1,
        "full range spans anchor statement, got: {:?}", range,
    );
}

#[test]
fn symbols_field_name_range_is_precise() {
    // `---@field bar integer` — the FIELD child's selection_range
    // should point at just `bar`, not the whole `---@field` line.
    let src = r#"---@class Foo
---@field bar integer
Foo = {}
"#;
    let syms = collect(src);
    let class = syms.iter().find(|s| s.name == "Foo").expect("Foo class present");
    let children = class.children.as_ref().expect("has children");
    let bar = children.iter().find(|c| c.name == "bar").expect("bar field present");
    let sel = bar.selection_range;
    // `bar` is 3 UTF-16 units wide.
    assert_eq!(
        sel.end.character - sel.start.character,
        3,
        "field selection_range spans just `bar`, got: {:?}", sel,
    );
    // And its start line is the `---@field bar integer` line (line 1).
    assert_eq!(sel.start.line, 1, "field on line 1, got: {:?}", sel);
}

#[test]
fn symbols_alias_name_range_is_precise() {
    let src = r#"---@alias MyString string
"#;
    let syms = collect(src);
    let alias = syms.iter().find(|s| s.name == "MyString").expect("alias present");
    let sel = alias.selection_range;
    assert_eq!(
        sel.end.character - sel.start.character,
        8, // "MyString" is 8 chars
        "alias selection_range spans just `MyString`, got: {:?}", sel,
    );
}

#[test]
fn symbols_enum_name_range_is_precise() {
    let src = r#"---@enum Color
local Color = { R = 1, G = 2 }
"#;
    let syms = collect(src);
    let enum_sym = syms.iter().find(|s| s.name == "Color").expect("enum present");
    let sel = enum_sym.selection_range;
    assert_eq!(
        sel.end.character - sel.start.character,
        5, // "Color" is 5 chars
        "enum selection_range spans just `Color`, got: {:?}", sel,
    );
}

/// Regression: VS Code rejected the whole outline with
/// `"selectionRange must be contained in fullRange"` when the
/// `---@class` annotation sits *above* the anchor statement
/// (`Foo = { ... }`), because `range` used to be the anchor-only
/// range while `selection_range` lived on the comment line above.
/// Assert the LSP invariant holds across common declaration shapes.
#[test]
fn symbols_selection_range_contained_in_range_across_shapes() {
    // Covers:
    //  - `@class` + `@field` + plain-global anchor (the original bug)
    //  - `@class` + `local` anchor
    //  - method/function children on a class
    //  - top-level function / local function / local var / global var
    //  - `@alias` and `@enum` (no anchor)
    let src = r#"---@class Audit
---@field enabled boolean
Audit = { enabled = true }

---@class Widget
---@field name string
local Widget = {}

function Widget:setName(n) self.name = n end
function Widget.static() end

---@alias MyId integer
---@enum Kind

function top_fn(a, b) return a + b end
local function helper() end
local x = 1
Global = 2
"#;
    let syms = collect(src);

    fn check(syms: &[tower_lsp_server::ls_types::DocumentSymbol], path: &str) {
        for s in syms {
            let r = s.range;
            let sel = s.selection_range;
            let start_ok = (r.start.line, r.start.character)
                <= (sel.start.line, sel.start.character);
            let end_ok = (r.end.line, r.end.character)
                >= (sel.end.line, sel.end.character);
            assert!(
                start_ok && end_ok,
                "`{}{}` violates LSP invariant: range={:?} selection_range={:?}",
                path, s.name, r, sel,
            );
            if let Some(children) = s.children.as_ref() {
                let child_path = format!("{}{}.", path, s.name);
                check(children, &child_path);
            }
        }
    }
    check(&syms, "");

    // Also sanity-check the original broken case specifically: the
    // Audit class must exist and its `range` starts on or before the
    // `---@class` annotation line.
    let audit = syms.iter().find(|s| s.name == "Audit").expect("Audit class present");
    assert_eq!(
        audit.range.start.line, 0,
        "Audit.range should start at `---@class Audit` (line 0), got: {:?}",
        audit.range,
    );
}

#[test]
fn symbols_field_name_range_skips_visibility() {
    // `---@field private bar integer` — the selection_range must point
    // at `bar`, not `private`.
    let src = r#"---@class Foo
---@field private bar integer
Foo = {}
"#;
    let syms = collect(src);
    let class = syms.iter().find(|s| s.name == "Foo").expect("Foo present");
    let children = class.children.as_ref().expect("has children");
    let bar = children.iter().find(|c| c.name == "bar").expect("bar field");
    let sel = bar.selection_range;
    assert_eq!(
        sel.end.character - sel.start.character,
        3, // "bar"
        "visibility skipped; selection spans just `bar`, got: {:?}", sel,
    );
}
