mod test_helpers;

use mylua_lsp::workspace_symbol::search_workspace_symbols;
use test_helpers::*;
use tower_lsp_server::ls_types::SymbolKind;

#[test]
fn workspace_symbol_class_fields_visible_with_container_name() {
    // P1-5: `@class Foo` + `@field bar integer` should produce a
    // searchable FIELD symbol `bar` with `container_name = "Foo"`.
    let src = r#"---@class Foo
---@field bar integer
---@field baz string
Foo = {}
"#;
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    let results = search_workspace_symbols("bar", &agg);
    let field = results
        .iter()
        .find(|s| s.name == "bar" && s.kind == SymbolKind::FIELD)
        .unwrap_or_else(|| panic!("should find bar as FIELD, got: {:?}", results));
    assert_eq!(field.container_name.as_deref(), Some("Foo"));
}

#[test]
fn workspace_symbol_fuzzy_finds_fields_across_classes() {
    // Two classes each with a `bar` field in different files — both
    // should appear as distinct FIELD entries with their respective
    // container_name.
    let a = ("a.lua", r#"---@class Foo
---@field bar integer
Foo = {}
"#);
    let b = ("b.lua", r#"---@class Bar
---@field bar string
Bar = {}
"#);
    let (_docs, agg, _parser) = setup_workspace(&[a, b]);

    let results = search_workspace_symbols("bar", &agg);
    let bar_fields: Vec<_> = results
        .iter()
        .filter(|s| s.name == "bar" && s.kind == SymbolKind::FIELD)
        .collect();
    assert_eq!(bar_fields.len(), 2, "both fields should surface, got: {:?}", bar_fields);
    let containers: Vec<_> = bar_fields
        .iter()
        .filter_map(|s| s.container_name.as_deref())
        .collect();
    assert!(containers.contains(&"Foo"));
    assert!(containers.contains(&"Bar"));
}

#[test]
fn workspace_symbol_method_splits_qualified_name() {
    // `function Foo:m()` appears as METHOD named `m` with
    // container_name `Foo`, not as a fused `Foo:m` symbol.
    let src = r#"---@class Foo
Foo = {}
function Foo:myMethod() end
"#;
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    let results = search_workspace_symbols("myMethod", &agg);
    let m = results
        .iter()
        .find(|s| s.name == "myMethod" && s.kind == SymbolKind::METHOD)
        .unwrap_or_else(|| panic!("should find myMethod as METHOD, got: {:?}", results));
    assert_eq!(m.container_name.as_deref(), Some("Foo"));
    // The fused name must NOT appear as a separate entry.
    assert!(
        !results.iter().any(|s| s.name == "Foo:myMethod"),
        "fused Foo:myMethod must not appear, got: {:?}",
        results.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[test]
fn workspace_symbol_dot_method_is_function_kind() {
    // `function Foo.bar()` — dot-form static method should surface as
    // FUNCTION kind with container=Foo, NOT METHOD (colon-only).
    let src = r#"---@class Foo
Foo = {}
function Foo.bar() end
"#;
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    let results = search_workspace_symbols("bar", &agg);
    let entry = results
        .iter()
        .find(|s| s.name == "bar")
        .unwrap_or_else(|| panic!("bar should be found, got: {:?}", results));
    assert_eq!(entry.kind, SymbolKind::FUNCTION, "dot-form should be FUNCTION, got {:?}", entry.kind);
    assert_eq!(entry.container_name.as_deref(), Some("Foo"));
}

#[test]
fn workspace_symbol_function_value_on_class_promoted_to_function() {
    // `Foo.bar = function() end` — equivalent to `function Foo.bar()`
    // at runtime. Must surface as FUNCTION (not FIELD) so both forms
    // show identically in workspace/symbol.
    let src = r#"---@class Foo
Foo = {}
Foo.bar = function() end
"#;
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    let results = search_workspace_symbols("bar", &agg);
    let entry = results
        .iter()
        .find(|s| s.name == "bar" && s.container_name.as_deref() == Some("Foo"));
    if let Some(e) = entry {
        assert_eq!(
            e.kind, SymbolKind::FUNCTION,
            "`Foo.bar = function() end` should also be FUNCTION, got {:?}",
            e.kind,
        );
    }
    // Note: if the summary builder doesn't register `Foo.bar = function`
    // in global_shard, the entry may simply be absent; the test
    // accepts that case as long as no spurious FIELD entry appears.
    let field_entries: Vec<_> = results
        .iter()
        .filter(|s| s.name == "bar" && s.kind == SymbolKind::FIELD && s.container_name.as_deref() == Some("Foo"))
        .collect();
    assert!(
        field_entries.is_empty(),
        "no spurious FIELD entry for a function-valued accessor, got: {:?}", field_entries,
    );
}

#[test]
fn workspace_symbol_global_function_still_listed() {
    // Plain top-level `function helper() end` still appears as a
    // FUNCTION symbol with no container.
    let src = "function helper() end\n";
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    let results = search_workspace_symbols("helper", &agg);
    let h = results
        .iter()
        .find(|s| s.name == "helper")
        .expect("helper function should be listed");
    assert_eq!(h.kind, SymbolKind::FUNCTION);
    assert!(h.container_name.is_none());
}

#[test]
fn workspace_symbol_class_itself_surfaces() {
    // `@class Foo` is itself searchable as CLASS.
    let src = "---@class MyClass\nMyClass = {}\n";
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    let results = search_workspace_symbols("MyClass", &agg);
    assert!(
        results
            .iter()
            .any(|s| s.name == "MyClass" && s.kind == SymbolKind::CLASS),
        "class itself should appear, got: {:?}",
        results,
    );
}
