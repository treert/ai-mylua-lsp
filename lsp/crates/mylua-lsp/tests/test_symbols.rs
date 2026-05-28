mod test_helpers;

use mylua_lsp::summary_builder;
use mylua_lsp::symbols::{self, DocumentSymbolDetailLevel};
use test_helpers::*;
use tower_lsp_server::ls_types::SymbolKind;

/// Helper: parse + build summary + call collect_document_symbols.
fn collect(src: &str) -> Vec<tower_lsp_server::ls_types::DocumentSymbol> {
    collect_with_detail(src, DocumentSymbolDetailLevel::Compact)
}

fn collect_with_detail(
    src: &str,
    detail_level: DocumentSymbolDetailLevel,
) -> Vec<tower_lsp_server::ls_types::DocumentSymbol> {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let uri = make_uri("test.lua");
    let summary = summary_builder::build_file_analysis(
        &uri,
        doc.tree().unwrap(),
        doc.source(),
        doc.line_index(),
    )
    .0;
    symbols::collect_document_symbols(
        doc.root_node().unwrap(),
        doc.source(),
        Some(&summary),
        doc.line_index(),
        detail_level,
    )
}

fn assert_no_empty_symbol_names(syms: &[tower_lsp_server::ls_types::DocumentSymbol]) {
    for sym in syms {
        assert!(
            !sym.name.is_empty(),
            "documentSymbol must not emit empty names: {:?}",
            syms,
        );
        if let Some(children) = sym.children.as_ref() {
            assert_no_empty_symbol_names(children);
        }
    }
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
    assert!(
        names.contains(&"hello"),
        "should contain function `hello`, got: {:?}",
        names
    );
    assert!(
        names.contains(&"world"),
        "should contain function `world`, got: {:?}",
        names
    );
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
    assert!(
        names.contains(&"myHelper"),
        "should contain `myHelper`, got: {:?}",
        names
    );
}

#[test]
fn symbols_compact_skips_nested_declarations() {
    let src = r#"
function outer()
    local function inner()
    end
    local x = 1
end
"#;
    let syms = collect(src);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    assert!(
        outer
            .children
            .as_ref()
            .map(|c| c.is_empty())
            .unwrap_or(true),
        "compact outline should keep nested declarations hidden, got: {:?}",
        outer.children
    );
}

#[test]
fn symbols_functions_mode_nests_named_functions_only() {
    let src = r#"
function outer()
    local x = 1
    local function inner()
    end
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::Functions);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = outer.children.as_ref().expect("outer children");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["inner"]);
    assert_eq!(children[0].kind, SymbolKind::FUNCTION);
}

#[test]
fn symbols_all_declarations_preserves_shadowed_locals() {
    let src = r#"
function outer()
    local x = 1
    do
        local x = 2
    end
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AllDeclarations);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = outer.children.as_ref().expect("outer children");
    let local_x_count = children
        .iter()
        .filter(|c| c.name == "x" && c.kind == SymbolKind::VARIABLE)
        .count();
    assert_eq!(
        local_x_count,
        2,
        "allDeclarations should keep both shadowed local declarations, got: {:?}",
        children
            .iter()
            .map(|c| (c.name.as_str(), c.kind))
            .collect::<Vec<_>>()
    );
}

#[test]
fn symbols_all_declarations_includes_parameters_and_for_variables() {
    let src = r#"
function outer(a)
    for i = 1, 2 do
    end
    for k, v in pairs({}) do
    end
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AllDeclarations);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = outer.children.as_ref().expect("outer children");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "i", "k", "v"]);
    assert!(children.iter().all(|c| c.kind == SymbolKind::VARIABLE));
}

#[test]
fn symbols_detail_modes_do_not_cross_anonymous_function_boundaries() {
    let src = r#"
function outer()
    local cb = function()
        local function hidden()
        end
        local x = 1
    end
end
"#;
    let function_syms = collect_with_detail(src, DocumentSymbolDetailLevel::Functions);
    let function_outer = function_syms
        .iter()
        .find(|s| s.name == "outer")
        .expect("outer");
    assert!(
        function_outer
            .children
            .as_ref()
            .map(|c| c.is_empty())
            .unwrap_or(true),
        "functions mode should not show declarations from anonymous function bodies, got: {:?}",
        function_outer.children
    );

    let all_syms = collect_with_detail(src, DocumentSymbolDetailLevel::AllDeclarations);
    let all_outer = all_syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = all_outer.children.as_ref().expect("outer children");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["cb"],
        "allDeclarations should keep the anonymous function body's locals scoped away"
    );
}

#[test]
fn symbols_anonymous_functions_mode_nests_anonymous_function_declarations() {
    let src = r#"
function outer()
    local cb = function(a)
        local x = 1
        local function hidden()
        end
    end
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AnonymousFunctions);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = outer.children.as_ref().expect("outer children");
    let cb = children.iter().find(|s| s.name == "cb").expect("cb");

    assert_eq!(cb.kind, SymbolKind::FUNCTION);
    let cb_children = cb.children.as_ref().expect("cb children");
    let cb_child_names: Vec<&str> = cb_children.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(cb_child_names, vec!["a", "x", "hidden"]);
    assert_eq!(cb_children[0].kind, SymbolKind::VARIABLE);
    assert_eq!(cb_children[1].kind, SymbolKind::VARIABLE);
    assert_eq!(cb_children[2].kind, SymbolKind::FUNCTION);
}

#[test]
fn symbols_anonymous_functions_mode_names_nested_assignment_functions() {
    let src = r#"
function outer()
    cb = function(a)
        local x = 1
    end
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AnonymousFunctions);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = outer.children.as_ref().expect("outer children");
    let cb = children.iter().find(|s| s.name == "cb").expect("cb");

    assert_eq!(cb.kind, SymbolKind::FUNCTION);
    let cb_child_names: Vec<&str> = cb
        .children
        .as_ref()
        .expect("cb children")
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(cb_child_names, vec!["a", "x"]);
}

#[test]
fn symbols_anonymous_functions_mode_includes_unbound_anonymous_functions() {
    let src = r#"
function outer()
    register(function(a)
        local x = 1
    end)
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AnonymousFunctions);
    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let children = outer.children.as_ref().expect("outer children");
    let anon = children
        .iter()
        .find(|s| s.name == "<anonymous>")
        .expect("<anonymous>");

    assert_eq!(anon.kind, SymbolKind::FUNCTION);
    let anon_child_names: Vec<&str> = anon
        .children
        .as_ref()
        .expect("anonymous children")
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(anon_child_names, vec!["a", "x"]);
}

#[test]
fn symbols_anonymous_functions_mode_includes_top_level_wrapped_anonymous_functions() {
    let src = r#"
cb = wrap(function(a)
    local x = 1
end)
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AnonymousFunctions);
    let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["cb", "<anonymous>"]);
}

#[test]
fn symbols_anonymous_functions_mode_does_not_expose_top_level_block_locals() {
    let src = r#"
if ready then
    local x = 1
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AnonymousFunctions);

    assert!(
        syms.is_empty(),
        "top-level block locals should remain hidden, got: {:?}",
        syms
    );
}

#[test]
fn symbols_detail_modes_apply_inside_class_methods() {
    let src = r#"---@class Foo
Foo = {}

function Foo:m(a)
    local x = 1
    local function inner()
    end
end
"#;
    let function_syms = collect_with_detail(src, DocumentSymbolDetailLevel::Functions);
    let foo = function_syms.iter().find(|s| s.name == "Foo").expect("Foo");
    let method = foo
        .children
        .as_ref()
        .expect("Foo children")
        .iter()
        .find(|s| s.name == "m")
        .expect("m");
    let function_child_names: Vec<&str> = method
        .children
        .as_ref()
        .expect("method children")
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(function_child_names, vec!["inner"]);

    let all_syms = collect_with_detail(src, DocumentSymbolDetailLevel::AllDeclarations);
    let foo = all_syms.iter().find(|s| s.name == "Foo").expect("Foo");
    let method = foo
        .children
        .as_ref()
        .expect("Foo children")
        .iter()
        .find(|s| s.name == "m")
        .expect("m");
    let all_child_names: Vec<&str> = method
        .children
        .as_ref()
        .expect("method children")
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(all_child_names, vec!["a", "x", "inner"]);
}

#[test]
fn symbols_detail_modes_still_skip_dotted_assignments() {
    let src = r#"
t = {}
t.foo = 1

function outer()
    local x = 1
    x.y = 2
end
"#;
    let syms = collect_with_detail(src, DocumentSymbolDetailLevel::AllDeclarations);
    let top_names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(top_names, vec!["t", "outer"]);

    let outer = syms.iter().find(|s| s.name == "outer").expect("outer");
    let child_names: Vec<&str> = outer
        .children
        .as_ref()
        .expect("outer children")
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(child_names, vec!["x"]);
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
    let foo = syms.iter().find(|s| s.name == "Foo").unwrap_or_else(|| {
        panic!(
            "Foo class missing, got: {:?}",
            syms.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });
    assert_eq!(foo.kind, SymbolKind::CLASS);
    let children = foo.children.as_ref().expect("children");
    let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.contains(&"x"),
        "Foo.x as Field child, got: {:?}",
        names
    );
    assert!(
        names.contains(&"m"),
        "Foo:m as Method child, got: {:?}",
        names
    );

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
fn symbols_class_method_backfilled_field_not_duplicated() {
    let src = r#"---@class Vector2
---@field x number
---@field y number
local Vector2 = {}

function Vector2:length()
    return math.sqrt(self.x * self.x + self.y * self.y)
end
"#;
    let syms = collect(src);
    let vector = syms.iter().find(|s| s.name == "Vector2").expect("Vector2");
    let children = vector.children.as_ref().expect("children");
    let length_entries: Vec<_> = children.iter().filter(|c| c.name == "length").collect();

    assert_eq!(
        length_entries.len(),
        1,
        "length should appear once in class outline, got: {:?}",
        children
            .iter()
            .map(|c| (c.name.as_str(), c.kind))
            .collect::<Vec<_>>()
    );
    assert_eq!(length_entries[0].kind, SymbolKind::METHOD);
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
        !names
            .iter()
            .any(|n| *n == "x" && syms.iter().filter(|s| s.name == "x").count() > 1),
        "x must appear only once, got: {:?}",
        names,
    );
    // Critically: no "foo" or "x.foo" entry.
    assert!(
        !names.iter().any(|n| *n == "foo" || *n == "x.foo"),
        "dotted LHS must not generate an outline symbol, got: {:?}",
        names,
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
    let ui = syms
        .iter()
        .find(|s| s.name == "uiButton")
        .expect("uiButton class");
    assert_eq!(ui.kind, SymbolKind::CLASS);
    let child_names: Vec<&str> = ui
        .children
        .as_ref()
        .expect("children")
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert!(
        child_names.contains(&"setX"),
        "setX as child, got: {:?}",
        child_names
    );
    assert!(
        child_names.contains(&"setY"),
        "setY as child, got: {:?}",
        child_names
    );
}

#[test]
fn symbols_empty_file() {
    let syms = collect("");
    assert!(syms.is_empty(), "empty file should have no symbols");
}

#[test]
fn symbols_malformed_enum_does_not_emit_empty_name() {
    let src = r#"---@enum
local x = 1
"#;
    let syms = collect(src);
    assert_no_empty_symbol_names(&syms);
}

#[test]
fn symbols_incomplete_local_does_not_emit_empty_name() {
    let src = r#"---@class ABC123
---@field a integer
local ABC = {}

function ABC:init()
    self.m_a = 111
end

local "#;
    let syms = collect(src);
    assert_no_empty_symbol_names(&syms);
}

#[test]
fn symbols_fixture_hover1() {
    let src = r#"---@class uiButton
local uiButton = class('uiButton')

---@return uiButton
function uiButton.new()
    return self
end

---@return uiButton
function uiButton:setX(x)
    self.x_ = x
    return self
end

---@return uiButton
function uiButton:setY(y)
    self.y_ = y
    self.setY():setY():setY():setY()
    return self
end

local btn1 = uiButton.new()                    -- 返回类型 uiButton

local btn2 = uiButton.new():setX(10)           -- 返回类型 any

local btn3 = uiButton.new():setX(10):setY(10)  -- 返回类型 any

_G.aaa = uiButton.new()
local btn4 = _G.aaa.setX().setX():setX():setX():setX()

local btn5 = uiButton:setX(1)

---@return uiButton
function setYa(y)
    self.y_ = y
    uiButton.setY():setY():setY():setY().setX()
    return self
end

local btn6 = setYa();
local btn7 = setYa().setX();


---@return uiButton
function uiButton:setY1(y)
    local btn8 = self:setX()
    local btn9 = self:setY()
end
"#;
    let syms = collect(src);
    // hover1.lua defines uiButton (class) + setX/setY/setY1/new as its
    // children.
    let ui = syms.iter().find(|s| s.name == "uiButton");
    assert!(
        ui.is_some(),
        "should find uiButton class, got: {:?}",
        syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
    if let Some(ui) = ui {
        let child_names: Vec<&str> = ui
            .children
            .as_ref()
            .map(|v| v.iter().map(|c| c.name.as_str()).collect())
            .unwrap_or_default();
        assert!(
            child_names.iter().any(|n| *n == "new" || n.contains("new")),
            "uiButton should have `new` child, got: {:?}",
            child_names,
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
    let class = syms
        .iter()
        .find(|s| s.name == "Foo")
        .expect("Foo class present");
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
        "selection_range should span exactly `Foo`, got {:?}",
        sel,
    );
    // Meanwhile, `range` covers the anchor statement on line 1.
    assert!(
        range.end.line >= 1,
        "full range spans anchor statement, got: {:?}",
        range,
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
    let class = syms
        .iter()
        .find(|s| s.name == "Foo")
        .expect("Foo class present");
    let children = class.children.as_ref().expect("has children");
    let bar = children
        .iter()
        .find(|c| c.name == "bar")
        .expect("bar field present");
    let sel = bar.selection_range;
    // `bar` is 3 UTF-16 units wide.
    assert_eq!(
        sel.end.character - sel.start.character,
        3,
        "field selection_range spans just `bar`, got: {:?}",
        sel,
    );
    // And its start line is the `---@field bar integer` line (line 1).
    assert_eq!(sel.start.line, 1, "field on line 1, got: {:?}", sel);
}

#[test]
fn symbols_alias_name_range_is_precise() {
    let src = r#"---@alias MyString string
"#;
    let syms = collect(src);
    let alias = syms
        .iter()
        .find(|s| s.name == "MyString")
        .expect("alias present");
    let sel = alias.selection_range;
    assert_eq!(
        sel.end.character - sel.start.character,
        8, // "MyString" is 8 chars
        "alias selection_range spans just `MyString`, got: {:?}",
        sel,
    );
}

#[test]
fn symbols_enum_name_range_is_precise() {
    let src = r#"---@enum Color
local Color = { R = 1, G = 2 }
"#;
    let syms = collect(src);
    let enum_sym = syms
        .iter()
        .find(|s| s.name == "Color")
        .expect("enum present");
    let sel = enum_sym.selection_range;
    assert_eq!(
        sel.end.character - sel.start.character,
        5, // "Color" is 5 chars
        "enum selection_range spans just `Color`, got: {:?}",
        sel,
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
            let start_ok =
                (r.start.line, r.start.character) <= (sel.start.line, sel.start.character);
            let end_ok = (r.end.line, r.end.character) >= (sel.end.line, sel.end.character);
            assert!(
                start_ok && end_ok,
                "`{}{}` violates LSP invariant: range={:?} selection_range={:?}",
                path,
                s.name,
                r,
                sel,
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
    let audit = syms
        .iter()
        .find(|s| s.name == "Audit")
        .expect("Audit class present");
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
    let bar = children
        .iter()
        .find(|c| c.name == "bar")
        .expect("bar field");
    let sel = bar.selection_range;
    assert_eq!(
        sel.end.character - sel.start.character,
        3, // "bar"
        "visibility skipped; selection spans just `bar`, got: {:?}",
        sel,
    );
}
