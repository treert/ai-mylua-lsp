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
fn signature_help_merges_overloads_from_impl_file_when_class_declared_elsewhere() {
    // P0-R1 regression: `@class Foo` + `@field init fun(...)` lives in one
    // file while `function Foo:init() end` + `---@overload` lives in
    // another. The declaration file's resolver gives us a single
    // `FunctionSignature` (from `@field`), but the implementation file is
    // where the extra overloads are declared — we must merge both.
    let decl = (
        "a.lua",
        r#"---@class Foo
---@field init fun(name: string): Foo
Foo = {}
"#,
    );
    let impl_file = (
        "b.lua",
        r#"---@overload fun(n: number): Foo
---@param self Foo
function Foo:init() end
"#,
    );
    let caller = (
        "caller.lua",
        r#"---@type Foo
local f = nil
f:init()
"#,
    );
    let (docs, mut agg, _parser) = setup_workspace(&[decl, impl_file, caller]);
    let caller_uri = make_uri("caller.lua");
    let doc = docs.get(&caller_uri).expect("caller doc present");

    // Cursor inside `f:init(|)` on line 2, col 7.
    let h = mylua_lsp::signature_help::signature_help(doc, &caller_uri, pos(2, 7), &mut agg)
        .expect("signatureHelp should resolve for f:init()");
    let labels: Vec<String> = h.signatures.iter().map(|s| s.label.clone()).collect();

    // Exactly 2 signatures: `@field` primary + impl `@overload`. The impl
    // file's own primary (`function Foo:init() end` with only `self`) is
    // a visually-empty stub once `self` is hidden for `:` calls — it must
    // be filtered to avoid a blank `f:init()` entry duplicating the
    // `@field` sig.
    assert_eq!(
        h.signatures.len(),
        2,
        "expected exactly 2 merged signatures, got: {:?}",
        labels,
    );
    assert_eq!(
        h.active_signature,
        Some(0),
        "active_signature should point at the primary (`@field`-declared) sig",
    );
    assert!(
        h.signatures[0].label.contains("name: string") && h.signatures[0].label.contains(": Foo"),
        "signatures[0] should be `@field`-declared `(name: string): Foo`, got: {}",
        h.signatures[0].label,
    );
    assert!(
        h.signatures[1].label.contains("n: number") && h.signatures[1].label.contains(": Foo"),
        "signatures[1] should be impl-file `@overload fun(n: number): Foo`, got: {}",
        h.signatures[1].label,
    );
    assert!(
        !labels.iter().any(|l| l == "f:init()"),
        "impl file's self-only primary must not appear as blank `f:init()` entry, got: {:?}",
        labels,
    );
}

#[test]
fn signature_help_emmy_field_method_does_not_pick_unrelated_global() {
    // P0-R3 regression: in `lookup_function_signatures_by_field` the
    // Function branch used to fall through to a bare
    // `summary.function_summaries.get(field_name)` lookup whenever the
    // class-qualified key `{class}:{field}` / `{class}.{field}` missed.
    // Since `function_summaries` is keyed by the fully-qualified
    // declaration name (`Foo:m`, not `m`), a `get("m")` hit could only
    // come from a TOP-LEVEL `function m() end` declared in the same
    // file — semantically unrelated to the method call. For a class
    // with a `@field m fun(...)` but no `function Class:m() end` body
    // in the same file, that wrong global `m` would shadow the
    // correctly-resolved `@field` signature. Fix: the bare fallback is
    // removed; the resolver's `sig` (from `@field`) is authoritative
    // when no exact class-qualified or global-shard impl is found.
    let src = r#"---@class Handler
---@field shout fun(self, msg: string)
Handler = {}

---@param n number
function shout(n) end

---@type Handler
local handler = nil

handler:shout("hi")"#;
    let (doc, uri, mut agg) = setup_single_file(src, "handler.lua");

    // Cursor inside `handler:shout(|"hi")` — line 10, column 14
    let h = mylua_lsp::signature_help::signature_help(&doc, &uri, pos(10, 14), &mut agg)
        .expect("signatureHelp should resolve for handler:shout()");
    let labels: Vec<String> = h.signatures.iter().map(|s| s.label.clone()).collect();

    // Exactly one signature: the `@field`-declared `(msg: string)`.
    // A regression that re-introduces the bare fallback would add a
    // second entry derived from the top-level `function shout(n)` —
    // lock that out explicitly.
    assert_eq!(
        h.signatures.len(),
        1,
        "only the @field signature should be surfaced, got: {:?}",
        labels,
    );
    assert_eq!(h.active_signature, Some(0));
    assert!(
        labels[0].contains("msg: string"),
        "should pick up @field-declared `msg: string` param, got: {}",
        labels[0],
    );
    assert!(
        !labels[0].contains("n: number"),
        "must NOT pick up the unrelated top-level `function shout(n)`, got: {}",
        labels[0],
    );
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
