//! Tests for `get_function_by_name` semantics after removing the
//! fallback linear scan: the method now only returns **global**
//! functions indexed in `function_name_index`. Local functions must
//! be accessed via `scope_tree → FunctionRef(id) → function_summaries[id]`.

mod test_helpers;

use test_helpers::*;

// ---------------------------------------------------------------------------
// get_function_by_name — global function lookup
// ---------------------------------------------------------------------------

#[test]
fn get_function_by_name_finds_global_function() {
    let src = "function greet() return 'hi' end\n";
    let (_doc, uri, agg) = setup_single_file(src, "global_fn.lua");
    let summary = agg.summary(&uri).expect("summary");
    assert!(
        summary.get_function_by_name("greet").is_some(),
        "global function must be found via get_function_by_name",
    );
}

#[test]
fn get_function_by_name_finds_qualified_global_dot() {
    let src = "M = {}\nfunction M.foo() end\n";
    let (_doc, uri, agg) = setup_single_file(src, "qualified_dot.lua");
    let summary = agg.summary(&uri).expect("summary");
    // function_name_index normalizes colon → dot; dot form is identity.
    assert!(
        summary.get_function_by_name("M.foo").is_some(),
        "M.foo must be found",
    );
}

#[test]
fn get_function_by_name_normalizes_colon_to_dot() {
    let src = "Player = {}\nfunction Player:new() end\n";
    let (_doc, uri, agg) = setup_single_file(src, "colon_norm.lua");
    let summary = agg.summary(&uri).expect("summary");
    // Querying with colon form should work (normalized internally).
    assert!(
        summary.get_function_by_name("Player:new").is_some(),
        "colon query must match the dot-normalized index entry",
    );
    // Querying with dot form directly should also work.
    assert!(
        summary.get_function_by_name("Player.new").is_some(),
        "dot query must also match",
    );
}

// ---------------------------------------------------------------------------
// get_function_by_name — local function NOT exposed
// ---------------------------------------------------------------------------

#[test]
fn get_function_by_name_does_not_find_local_function() {
    let src = "local function helper() return 1 end\n";
    let (_doc, uri, agg) = setup_single_file(src, "local_fn.lua");
    let summary = agg.summary(&uri).expect("summary");
    assert!(
        summary.get_function_by_name("helper").is_none(),
        "local function must NOT be found via get_function_by_name",
    );
    // But it should still exist in function_summaries (accessible by ID).
    assert!(
        summary.function_summaries.values().any(|fs| fs.name == "helper"),
        "local function must still be in function_summaries",
    );
}

#[test]
fn get_function_by_name_does_not_find_local_table_method() {
    let src = r#"
local M = {}
function M:foo() end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "local_method.lua");
    let summary = agg.summary(&uri).expect("summary");
    // M is local, so M:foo should NOT be in function_name_index.
    assert!(
        summary.get_function_by_name("M:foo").is_none(),
        "local-table method must NOT be found via get_function_by_name",
    );
    assert!(
        summary.get_function_by_name("M.foo").is_none(),
        "local-table method (dot form) must NOT be found",
    );
}

#[test]
fn same_name_local_does_not_shadow_prior_global() {
    // Global `f` declared first, then a local `f` in a nested block.
    // get_function_by_name should still find the global one.
    let src = r#"
function f() return "global" end
do
    local function f() return "local" end
end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "shadow.lua");
    let summary = agg.summary(&uri).expect("summary");
    let fs = summary.get_function_by_name("f").expect("global f must be found");
    // The global declaration is on line 1 (0-indexed).
    assert!(
        fs.range.start_row <= 1,
        "get_function_by_name must return the global f (line <=1), got start_row={}",
        fs.range.start_row,
    );
    // The local one should exist in function_summaries but not be returned.
    let all_f: Vec<_> = summary.function_summaries.values()
        .filter(|fs| fs.name == "f")
        .collect();
    assert_eq!(all_f.len(), 2, "both local and global f should be in function_summaries");
}

// ---------------------------------------------------------------------------
// CallSite.caller_id — populated correctly
// ---------------------------------------------------------------------------

#[test]
fn call_site_caller_id_populated_for_named_functions() {
    let src = r#"
local function outer()
    print("hello")
end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "caller_id.lua");
    let summary = agg.summary(&uri).expect("summary");
    // There should be a call site for `print` inside `outer`.
    let cs = summary.call_sites.iter()
        .find(|cs| cs.callee_name == "print")
        .expect("print call site");
    assert_eq!(cs.caller_name, "outer");
    assert!(
        cs.caller_id.is_some(),
        "caller_id must be Some for a named function",
    );
    // The ID must point to a valid function summary.
    let id = cs.caller_id.unwrap();
    let fs = summary.function_summaries.get(&id).expect("function_summaries[caller_id]");
    assert_eq!(fs.name, "outer");
}

#[test]
fn call_site_caller_id_none_at_top_level() {
    let src = "print('top')\n";
    let (_doc, uri, agg) = setup_single_file(src, "top_level.lua");
    let summary = agg.summary(&uri).expect("summary");
    let cs = summary.call_sites.iter()
        .find(|cs| cs.callee_name == "print")
        .expect("print call site");
    assert!(cs.caller_name.is_empty(), "top-level caller_name must be empty");
    assert!(cs.caller_id.is_none(), "top-level caller_id must be None");
}

#[test]
fn call_site_caller_id_for_global_function() {
    let src = r#"
function G()
    print("in G")
end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "global_caller.lua");
    let summary = agg.summary(&uri).expect("summary");
    let cs = summary.call_sites.iter()
        .find(|cs| cs.callee_name == "print")
        .expect("print call site");
    assert_eq!(cs.caller_name, "G");
    assert!(cs.caller_id.is_some(), "global function caller must have an ID");
    let id = cs.caller_id.unwrap();
    let fs = summary.function_summaries.get(&id).expect("summary must contain G");
    assert_eq!(fs.name, "G");
}
