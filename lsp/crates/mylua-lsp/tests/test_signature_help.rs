mod test_helpers;

use test_helpers::*;
use mylua_lsp::signature_help;
use tower_lsp_server::ls_types::ParameterLabel;

#[test]
fn signature_help_simple_local_function() {
    let src = r#"---@param a number
---@param b string
---@return boolean
function foo(a, b) end

foo(1, "x")"#;
    let (doc, uri, mut agg) = setup_single_file(src, "sig.lua");

    // Cursor between args: `foo(1,| "x")` → active param = 1 (second arg)
    let help = signature_help::signature_help(&doc, &uri, pos(5, 7), &mut agg)
        .expect("signatureHelp should return Some");

    assert_eq!(help.signatures.len(), 1, "one signature");
    let sig = &help.signatures[0];
    assert!(
        sig.label.contains("foo(") && sig.label.contains("a: number") && sig.label.contains("b: string"),
        "label should include typed params, got: {:?}",
        sig.label,
    );
    assert_eq!(help.active_parameter, Some(1), "cursor after comma → second param active");
    // Parameter offsets should point inside the label.
    assert_eq!(sig.parameters.as_ref().unwrap().len(), 2);
    for p in sig.parameters.as_ref().unwrap() {
        if let ParameterLabel::LabelOffsets([start, end]) = &p.label {
            assert!(*start < *end && (*end as usize) <= sig.label.len());
        } else {
            panic!("expected LabelOffsets");
        }
    }
}

#[test]
fn signature_help_active_parameter_progression() {
    let src = r#"function foo(a, b, c) end
foo(1, 2, 3)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "active.lua");

    // Position just after `(`: should be 0
    let h = signature_help::signature_help(&doc, &uri, pos(1, 4), &mut agg).unwrap();
    assert_eq!(h.active_parameter, Some(0));

    // After first comma
    let h = signature_help::signature_help(&doc, &uri, pos(1, 7), &mut agg).unwrap();
    assert_eq!(h.active_parameter, Some(1));

    // After second comma
    let h = signature_help::signature_help(&doc, &uri, pos(1, 10), &mut agg).unwrap();
    assert_eq!(h.active_parameter, Some(2));
}

#[test]
fn signature_help_ignores_commas_inside_nested_table() {
    let src = r#"function foo(a, b) end
foo({1, 2, 3}, 5)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "nested.lua");

    // Cursor right before ", 5)" — still on the first arg (the table).
    let h = signature_help::signature_help(&doc, &uri, pos(1, 12), &mut agg).unwrap();
    assert_eq!(
        h.active_parameter,
        Some(0),
        "commas inside nested {{ ... }} must not advance the active parameter",
    );
}

#[test]
fn signature_help_with_overload() {
    let src = r#"---@overload fun(s: string): boolean
---@param n number
---@return number
function work(n) return n end

work("hi")"#;
    let (doc, uri, mut agg) = setup_single_file(src, "overload.lua");

    let h = signature_help::signature_help(&doc, &uri, pos(5, 6), &mut agg)
        .expect("signatureHelp should return Some");

    assert_eq!(
        h.signatures.len(),
        2,
        "primary + 1 overload expected; got: {:?}",
        h.signatures.iter().map(|s| &s.label).collect::<Vec<_>>(),
    );
    let primary = &h.signatures[0].label;
    let overload = &h.signatures[1].label;
    assert!(
        primary.contains("n: number") && primary.contains(": number"),
        "primary label: {}", primary,
    );
    assert!(
        overload.contains("s: string") && overload.contains(": boolean"),
        "overload label: {}", overload,
    );
}

#[test]
fn signature_help_table_call_active_param_is_zero() {
    // Regression: previously treated `{` as `(` so commas inside the table
    // would advance active_parameter. `foo{a=1, b=2, c=3}` is a single
    // implicit argument — active must stay 0.
    let src = r#"---@param t table
function foo(t) end

foo{a=1, b=2, c=3}"#;
    let (doc, uri, mut agg) = setup_single_file(src, "tablecall.lua");

    // Cursor inside the table, after two commas.
    let h = signature_help::signature_help(&doc, &uri, pos(3, 15), &mut agg)
        .expect("table-call still produces a signature");
    assert_eq!(
        h.active_parameter,
        Some(0),
        "commas inside `{{ ... }}` table-call argument must not advance active_parameter",
    );
}

#[test]
fn signature_help_picks_correct_overload_for_class_with_same_method_name() {
    // Regression: the previous `ends_with(".m")` / `ends_with(":m")` scan
    // iterated a HashMap and could pick the wrong class's overloads when
    // two classes in the same file share a method name (e.g. both
    // `Foo:init` and `Bar:init`). The new code uses exact class-qualified
    // keys driven by the resolved type of the base expression.
    let src = r#"---@class Foo
Foo = {}
---@overload fun(name: string): Foo
---@param self Foo
function Foo:init() end
---@class Bar
Bar = {}
---@overload fun(n: number): Bar
---@param self Bar
function Bar:init() end
---@type Foo
local f = nil
f:init()"#;
    let (doc, uri, mut agg) = setup_single_file(src, "shared_name.lua");

    // Cursor inside `f:init(|)` on line 12, col 7.
    let h = signature_help::signature_help(&doc, &uri, pos(12, 7), &mut agg)
        .expect("signatureHelp should resolve for f:init()");
    let labels: Vec<String> = h.signatures.iter().map(|s| s.label.clone()).collect();

    // Must include Foo's overload (string), not Bar's (number).
    assert!(
        labels.iter().any(|l| l.contains("name: string") && l.contains(": Foo")),
        "should include Foo:init's overload `(name: string): Foo`, got: {:?}",
        labels,
    );
    assert!(
        !labels.iter().any(|l| l.contains("n: number") && l.contains(": Bar")),
        "must not include Bar:init's overload when calling on Foo, got: {:?}",
        labels,
    );
}

#[test]
fn signature_help_returns_none_outside_call() {
    let src = "local x = 1";
    let (doc, uri, mut agg) = setup_single_file(src, "none.lua");
    let h = signature_help::signature_help(&doc, &uri, pos(0, 4), &mut agg);
    assert!(h.is_none(), "cursor outside any call → None");
}

#[test]
fn signature_help_method_call_hides_self() {
    let src = r#"---@class Obj
---@field x integer
local Obj = {}
---@param self Obj
---@param dx integer
function Obj:move(dx) end

local o = Obj
o:move(10)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "method.lua");

    let h = signature_help::signature_help(&doc, &uri, pos(8, 7), &mut agg);
    if let Some(h) = h {
        assert!(!h.signatures.is_empty(), "method call should return a signature");
        let label = &h.signatures[0].label;
        // `self` must not appear in the visible parameter list for `:` call.
        assert!(
            !label.contains("self:"),
            "method call signature must hide self, got: {}", label,
        );
    }
}
