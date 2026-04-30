mod test_helpers;

use test_helpers::*;
use mylua_lsp::call_hierarchy;
use tower_lsp_server::ls_types::SymbolKind;

#[test]
fn prepare_on_function_declaration_name() {
    // Cursor on the function's name → item for that function.
    let src = "function hello()\n    return 1\nend\n";
    let (doc, uri, agg) = setup_single_file(src, "f.lua");
    // `hello` starts at column 9 of line 0. Cursor at column 10 (inside).
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(0, 10), &agg, &empty_docs());
    assert_eq!(items.len(), 1, "expect one item, got: {:?}", items);
    assert_eq!(items[0].name, "hello");
    assert_eq!(items[0].kind, SymbolKind::FUNCTION);
}

#[test]
fn prepare_on_call_site_resolves_to_target() {
    // `bar()` at a call site → item for `bar`'s declaration.
    let src = "local function bar() return 1 end\nlocal x = bar()\n";
    let (doc, uri, agg) = setup_single_file(src, "call.lua");
    // `bar` call is on line 1 around column 11.
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(1, 11), &agg, &empty_docs());
    assert_eq!(items.len(), 1, "expect one item, got: {:?}", items);
    assert_eq!(items[0].name, "bar");
}

#[test]
fn prepare_returns_empty_for_non_function_identifier() {
    let src = "local x = 1\n";
    let (doc, uri, agg) = setup_single_file(src, "var.lua");
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(0, 6), &agg, &empty_docs());
    assert!(items.is_empty(), "cursor on a plain local variable is not a function, got: {:?}", items);
}

#[test]
fn incoming_calls_within_file() {
    let src = r#"
local function target() return 1 end

local function caller_a()
    return target()
end

local function caller_b()
    target()
    target()
end
"#;
    let (doc, uri, agg) = setup_single_file(src, "incoming.lua");
    // Prepare via cursor on target's declaration (line 1, col 15 inside `target`).
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(1, 16), &agg, &empty_docs());
    assert!(!items.is_empty(), "prepare returned empty for target");
    let target_item = items[0].clone();
    assert_eq!(target_item.name, "target");

    let incoming = call_hierarchy::incoming_calls(&target_item, &agg, &empty_docs());
    // Two distinct callers. caller_a has 1 call, caller_b has 2 calls.
    assert_eq!(incoming.len(), 2, "two callers expected, got: {:?}", incoming);
    let a = incoming.iter().find(|c| c.from.name == "caller_a").expect("caller_a missing");
    assert_eq!(a.from_ranges.len(), 1);
    let b = incoming.iter().find(|c| c.from.name == "caller_b").expect("caller_b missing");
    assert_eq!(b.from_ranges.len(), 2, "caller_b calls target twice, got: {:?}", b.from_ranges);
}

#[test]
fn incoming_calls_cross_file() {
    let (docs, agg, _parser) = setup_workspace(&[
        (
            "a.lua",
            "function lib_fn() return 1 end\n",
        ),
        (
            "b.lua",
            "function use_fn()\n    return lib_fn()\nend\n",
        ),
    ]);
    // Build target item manually against a.lua's lib_fn.
    let target_uri = make_uri("a.lua");
    let summary = agg.summaries.get(&target_uri).expect("a.lua summary");
    let fs = summary.get_function_by_name("lib_fn").expect("lib_fn summary");
    let target_doc = docs.get(&target_uri).expect("a.lua doc");
    let lsp_range = target_doc.line_index().byte_range_to_lsp_range(fs.range);
    let target_item = tower_lsp_server::ls_types::CallHierarchyItem {
        name: "lib_fn".to_string(),
        kind: SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri: target_uri,
        range: lsp_range,
        selection_range: lsp_range,
        data: None,
    };

    let incoming = call_hierarchy::incoming_calls(&target_item, &agg, &empty_docs());
    assert_eq!(incoming.len(), 1, "expected one cross-file caller, got: {:?}", incoming);
    assert_eq!(incoming[0].from.name, "use_fn");
    // The caller URI must be b.lua, not a.lua.
    assert!(
        incoming[0].from.uri.to_string().ends_with("b.lua"),
        "caller should be in b.lua, got: {}", incoming[0].from.uri.as_str(),
    );
}

#[test]
fn outgoing_calls_within_function() {
    let src = r#"
local function helper_a() end
local function helper_b() end

local function driver()
    helper_a()
    helper_b()
    helper_a()
end
"#;
    let (doc, uri, agg) = setup_single_file(src, "outgoing.lua");
    // Prepare on `driver` declaration.
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(4, 16), &agg, &empty_docs());
    let driver = items.into_iter().find(|i| i.name == "driver").expect("driver");

    let outgoing = call_hierarchy::outgoing_calls(&driver, &agg, &empty_docs());
    // Two distinct targets: helper_a (2 calls) and helper_b (1 call).
    assert_eq!(outgoing.len(), 2, "two distinct targets, got: {:?}", outgoing);
    let a = outgoing.iter().find(|c| c.to.name == "helper_a").expect("helper_a");
    assert_eq!(a.from_ranges.len(), 2);
    let b = outgoing.iter().find(|c| c.to.name == "helper_b").expect("helper_b");
    assert_eq!(b.from_ranges.len(), 1);
}

#[test]
fn outgoing_dotted_and_method_calls_use_last_segment() {
    let src = r#"
local M = {}
function M.foo() end
function M:bar() end
local obj = M

local function driver()
    M.foo()
    obj:bar()
end
"#;
    let (doc, uri, agg) = setup_single_file(src, "dot_method.lua");
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(6, 16), &agg, &empty_docs());
    let driver = items.into_iter().find(|i| i.name == "driver").expect("driver");

    let outgoing = call_hierarchy::outgoing_calls(&driver, &agg, &empty_docs());
    let target_names: Vec<&str> = outgoing.iter().map(|c| c.to.name.as_str()).collect();
    assert!(
        target_names.contains(&"foo"),
        "`M.foo()` should appear as outgoing `foo`, got: {:?}", target_names,
    );
    assert!(
        target_names.contains(&"bar"),
        "`obj:bar()` should appear as outgoing `bar`, got: {:?}", target_names,
    );
}

#[test]
fn calls_in_nested_function_do_not_leak_to_outer() {
    // `outer` body calls `helper`; an inner anonymous function also
    // calls `helper`. The outer's outgoing list should NOT contain
    // the inner function's calls — they belong to the inner scope.
    let src = r#"
local function helper() end

local function outer()
    helper()
    local inner = function()
        helper()
    end
end
"#;
    let (doc, uri, agg) = setup_single_file(src, "nested.lua");
    let items = call_hierarchy::prepare_call_hierarchy(&doc, &uri, pos(3, 16), &agg, &empty_docs());
    let outer = items.into_iter().find(|i| i.name == "outer").expect("outer");

    let outgoing = call_hierarchy::outgoing_calls(&outer, &agg, &empty_docs());
    let helper_ranges = outgoing
        .iter()
        .find(|c| c.to.name == "helper")
        .expect("helper target")
        .from_ranges
        .len();
    // Only the one direct call inside `outer` should be listed.
    assert_eq!(
        helper_ranges, 1,
        "inner anonymous function's call must not leak into outer's outgoing list, got {} ranges",
        helper_ranges,
    );
}
