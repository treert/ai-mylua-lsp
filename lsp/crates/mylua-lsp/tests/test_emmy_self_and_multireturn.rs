//! Tests for two Emmy type-expression extensions:
//!
//! 1. `fun(): A, B` — multi-return function types parse into a
//!    `FunctionSignature` with multiple `returns` entries.
//! 2. `---@return self` / `---@param x self` on a method with name
//!    `Foo:m` / `Foo.m` resolves `self` to `Foo` so fluent-style
//!    chains like `obj:a():b()` hover / completion resolve properly.

mod test_helpers;

use test_helpers::*;
use mylua_lsp::type_system::{substitute_self, class_prefix_of, KnownType, TypeFact};

#[test]
fn class_prefix_extraction() {
    assert_eq!(class_prefix_of("Foo:m"), "Foo");
    assert_eq!(class_prefix_of("Foo.m"), "Foo");
    assert_eq!(class_prefix_of("a.b.c"), "a.b");
    assert_eq!(class_prefix_of("top_level"), "");
    assert_eq!(class_prefix_of(""), "");
}

#[test]
fn substitute_self_replaces_emmy_type() {
    let fact = TypeFact::Known(KnownType::EmmyType("self".to_string()));
    let out = substitute_self(&fact, "Foo");
    assert!(
        matches!(&out, TypeFact::Known(KnownType::EmmyType(n)) if n == "Foo"),
        "self must become Foo, got: {:?}", out,
    );
}

#[test]
fn substitute_self_nested_inside_union() {
    let fact = TypeFact::Union(vec![
        TypeFact::Known(KnownType::EmmyType("self".into())),
        TypeFact::Known(KnownType::Nil),
    ]);
    let out = substitute_self(&fact, "Bar");
    match &out {
        TypeFact::Union(parts) => {
            assert_eq!(parts.len(), 2);
            assert!(
                matches!(&parts[0], TypeFact::Known(KnownType::EmmyType(n)) if n == "Bar"),
                "first part replaced, got: {:?}", parts,
            );
        }
        _ => panic!("expected union, got: {:?}", out),
    }
}

#[test]
fn substitute_self_noop_when_class_empty() {
    let fact = TypeFact::Known(KnownType::EmmyType("self".into()));
    let out = substitute_self(&fact, "");
    assert_eq!(out, fact, "no substitution when class is empty");
}

#[test]
fn method_return_self_resolves_to_owner_class() {
    let src = r#"
---@class Builder
local Builder = {}

---@return self
function Builder:chain()
    return self
end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "builder.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let fs = summary.function_summaries.get("Builder:chain").expect("method");
    assert_eq!(fs.signature.returns.len(), 1);
    match &fs.signature.returns[0] {
        TypeFact::Known(KnownType::EmmyType(n)) => {
            assert_eq!(n, "Builder", "self should become Builder, got: {}", n);
        }
        other => panic!("expected EmmyType(Builder), got: {:?}", other),
    }
}

#[test]
fn method_param_self_resolves_to_owner_class() {
    let src = r#"
---@class Builder
local Builder = {}

---@param other self
function Builder:merge(other)
    return other
end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "builder_param.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let fs = summary.function_summaries.get("Builder:merge").expect("method");
    let other = fs.signature.params.iter().find(|p| p.name == "other").expect("other param");
    match &other.type_fact {
        TypeFact::Known(KnownType::EmmyType(n)) => {
            assert_eq!(n, "Builder", "param `self` must resolve to Builder");
        }
        other => panic!("expected Builder, got: {:?}", other),
    }
}

#[test]
fn free_function_self_is_not_resolved() {
    // A free function has no class context — `self` in a `@return`
    // there stays as the `self` literal (best-effort; user is
    // expected to use this pattern only on methods).
    let src = r#"
---@return self
local function free() return nil end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "free.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let fs = summary.function_summaries.get("free").expect("func");
    match &fs.signature.returns[0] {
        TypeFact::Known(KnownType::EmmyType(n)) => {
            assert_eq!(n, "self", "free function keeps `self` literal");
        }
        _ => panic!("expected EmmyType(self)"),
    }
}

#[test]
fn fun_type_multi_return_parses_end_to_end() {
    // `fun(): A, B` as a @param type should produce a
    // FunctionSignature with 2 returns.
    let src = r#"
---@class A
local A = {}
---@class B
local B = {}

---@param cb fun(): A, B
local function takes(cb) end
"#;
    let (_doc, uri, agg) = setup_single_file(src, "multi_ret_param.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let fs = summary.function_summaries.get("takes").expect("takes");
    let cb = fs.signature.params.iter().find(|p| p.name == "cb").expect("cb");
    match &cb.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => {
            assert_eq!(
                sig.returns.len(), 2,
                "fun(): A, B must yield 2 returns, got: {:?}", sig.returns,
            );
        }
        other => panic!("expected Function fact, got: {:?}", other),
    }
}
