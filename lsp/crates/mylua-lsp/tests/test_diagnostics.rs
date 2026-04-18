mod test_helpers;

use test_helpers::*;
use mylua_lsp::config::{DiagnosticsConfig, DiagnosticSeverityOption};
use mylua_lsp::diagnostics;

#[test]
fn no_diagnostics_for_clean_code() {
    let mut parser = new_parser();
    let src = r#"
local a = 1
local b = "hello"
print(a, b)
"#;
    let doc = parse_doc(&mut parser, src);
    let diags = diagnostics::collect_diagnostics(doc.tree.root_node(), src.as_bytes());
    assert!(diags.is_empty(), "clean code should have no diagnostics, got: {:?}", diags);
}

#[test]
fn diagnostics_for_syntax_errors() {
    let src = read_fixture("parse/test1.lua");
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, &src);
    let diags = diagnostics::collect_diagnostics(doc.tree.root_node(), src.as_bytes());
    // test1.lua contains intentional parse errors (e.g. "dfjsofjao", "if faf fsf")
    assert!(!diags.is_empty(), "parse/test1.lua should produce diagnostics");
}

#[test]
fn diagnostics_for_define_test1() {
    let src = read_fixture("define/test1.lua");
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, &src);
    let diags = diagnostics::collect_diagnostics(doc.tree.root_node(), src.as_bytes());
    // define/test1.lua has some intentionally invalid lines
    assert!(!diags.is_empty(), "define/test1.lua should produce parse-level diagnostics");
}

#[test]
fn semantic_diagnostics_undefined_global() {
    let src = r#"
local a = 1
print(undefined_var)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");
    let diag_config = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(),
        src.as_bytes(),
        &uri,
        &mut agg,
        &doc.scope_tree,
        &diag_config,
    );
    // `print` and `undefined_var` are both globals — the exact behavior depends
    // on LSP config defaults, but we verify the function doesn't panic.
    let _ = diags;
}

#[test]
fn lua_field_error_on_closed_table() {
    let src = r#"
local t = { name = "hello", age = 10 }
print(t.name)
print(t.no_exist)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "field_err.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.lua_field_error = DiagnosticSeverityOption::Error;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let field_diags: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Unknown field"))
        .collect();
    assert!(
        field_diags.iter().any(|d| d.message.contains("no_exist")),
        "should flag 'no_exist' on closed table, got: {:?}",
        field_diags
    );
}

#[test]
fn emmy_type_mismatch_string_vs_number() {
    let src = r#"
---@type string
local x = 42
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "mismatch.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.emmy_type_mismatch = DiagnosticSeverityOption::Error;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let mismatch: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Type mismatch"))
        .collect();
    assert!(
        !mismatch.is_empty(),
        "should report type mismatch: @type string but got number. diags={:?}",
        diags
    );
}

#[test]
fn emmy_type_mismatch_no_false_positive() {
    let src = r#"
---@type number
local x = 42
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "no_mismatch.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.emmy_type_mismatch = DiagnosticSeverityOption::Error;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let mismatch: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Type mismatch"))
        .collect();
    assert!(
        mismatch.is_empty(),
        "@type number with 42 should NOT flag mismatch, got: {:?}",
        mismatch
    );
}

#[test]
fn enum_type_in_workspace_symbol() {
    let src = r#"
---@enum Color
local Color = {
    Red = 1,
    Green = 2,
    Blue = 3,
}
"#;
    let (_doc, _uri, agg) = setup_single_file(src, "enum.lua");
    let results = mylua_lsp::workspace_symbol::search_workspace_symbols("Color", &agg);
    assert!(
        results.iter().any(|s| s.name == "Color"),
        "workspace/symbol should find @enum Color, got: {:?}",
        results.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[test]
fn no_unknown_field_on_chained_lhs_assignment() {
    // Regression: inner `a.b` of `a.b.c = 1` was previously not recognized as
    // part of the LHS (is_assignment_target only looked at direct parent),
    // leading to false "Unknown field 'b' on table" diagnostics.
    let src = r#"
local a = { b = { c = 0 } }
a.b.c = 1
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "chained_lhs.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let unknown: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Unknown field"))
        .collect();
    assert!(
        unknown.is_empty(),
        "LHS-of-assignment nested field accesses must not emit Unknown field diagnostics, got: {:?}",
        unknown,
    );
}

#[test]
fn unknown_field_still_reported_on_rhs_read() {
    // Sanity counter-test: actual RHS reads of missing fields should still
    // be flagged.
    let src = r#"
local t = { name = "hello", age = 10 }
print(t.no_exist)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "rhs_read.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.lua_field_error = DiagnosticSeverityOption::Error;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().any(|d| d.message.contains("no_exist")),
        "rhs reads of unknown fields must still be diagnosed, got: {:?}",
        diags,
    );
}

#[test]
fn generic_class_field_resolution() {
    use mylua_lsp::resolver;
    use mylua_lsp::type_system::{TypeFact, KnownType};

    let src = r#"
---@generic T
---@class Container
---@field value T

---@type Container<string>
local c = getContainer()
"#;
    let (_doc, uri, mut agg) = setup_single_file(src, "generic.lua");
    let resolved = resolver::resolve_local_in_file(&uri, "c", &mut agg);
    let field_result = resolver::resolve_field_chain(
        &resolved.type_fact, &["value".to_string()], &mut agg,
    );
    assert!(
        matches!(&field_result.type_fact, TypeFact::Known(KnownType::String)),
        "Container<string>.value should resolve to string, got: {}",
        field_result.type_fact
    );
}

// ---------------------------------------------------------------------------
// P2-3 — duplicate table keys
// ---------------------------------------------------------------------------

#[test]
fn duplicate_table_key_reports_warning() {
    let src = "local t = { a = 1, b = 2, a = 3 }\n";
    let (doc, uri, mut agg) = setup_single_file(src, "dup_key.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    let dup: Vec<_> = diags.iter().filter(|d| d.message.contains("Duplicate table key")).collect();
    assert_eq!(dup.len(), 1, "exactly one duplicate report, got: {:?}", diags);
    assert!(dup[0].message.contains("'a'"), "message names the key, got: {}", dup[0].message);
}

#[test]
fn duplicate_table_key_across_numeric_and_string_keys() {
    // `[1] = "x"` vs `[1] = "y"` — numeric keys also dedup.
    let src = "local t = { [1] = \"x\", [1] = \"y\" }\n";
    let (doc, uri, mut agg) = setup_single_file(src, "dup_num.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    let dup: Vec<_> = diags.iter().filter(|d| d.message.contains("Duplicate table key")).collect();
    assert_eq!(dup.len(), 1, "numeric bracket keys dedup, got: {:?}", diags);
}

#[test]
fn duplicate_table_key_off_via_config() {
    let src = "local t = { a = 1, a = 2 }\n";
    let (doc, uri, mut agg) = setup_single_file(src, "dup_off.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.duplicate_table_key = DiagnosticSeverityOption::Off;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Duplicate table key")),
        "off config should suppress duplicate-key diagnostic, got: {:?}", diags,
    );
}

// ---------------------------------------------------------------------------
// P2-3 — unused locals
// ---------------------------------------------------------------------------

#[test]
fn unused_local_off_by_default() {
    let src = "local x = 1\n";
    let (doc, uri, mut agg) = setup_single_file(src, "unused_default.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    // Default config has unused_local = Off; no such diagnostic.
    assert!(
        diags.iter().all(|d| !d.message.contains("Unused local")),
        "unused_local default off, got: {:?}", diags,
    );
}

#[test]
fn unused_local_reports_when_enabled() {
    let src = "local x = 1\nlocal y = 2\nprint(y)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "unused_on.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.unused_local = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    let unused: Vec<_> = diags.iter().filter(|d| d.message.contains("Unused local")).collect();
    assert_eq!(unused.len(), 1, "only `x` is unused, got: {:?}", diags);
    assert!(unused[0].message.contains("'x'"));
}

#[test]
fn unused_local_skips_underscore_names() {
    // Conventional `_` / `_foo` names are intentionally discarded.
    let src = "local _ = 1\nlocal _unused = 2\n";
    let (doc, uri, mut agg) = setup_single_file(src, "underscore.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.unused_local = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Unused local")),
        "underscore names shouldn't trigger unused, got: {:?}", diags,
    );
}

#[test]
fn unused_local_counts_reference_in_expression() {
    let src = "local x = 42\nreturn x + 1\n";
    let (doc, uri, mut agg) = setup_single_file(src, "used.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.unused_local = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri, &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Unused local")),
        "x is used in return expression, got: {:?}", diags,
    );
}

// ---------------------------------------------------------------------------
// P2-3 — @type follow-up assignment mismatch
// ---------------------------------------------------------------------------

#[test]
fn emmy_type_mismatch_on_reassignment() {
    // `x` is declared `---@type number`; a later `x = "str"` must
    // report mismatch in addition to the initial declaration being OK.
    let src = r#"
---@type number
local x = 0
x = "not a number"
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "reassign.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.emmy_type_mismatch = DiagnosticSeverityOption::Error;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Type mismatch on assignment"))
        .collect();
    assert_eq!(
        mismatches.len(), 1,
        "exactly one follow-up assignment mismatch, got: {:?}", diags,
    );
    assert!(
        mismatches[0].message.contains("'x'"),
        "message names the variable, got: {}", mismatches[0].message,
    );
}

#[test]
fn emmy_type_mismatch_reassignment_respects_shadowing() {
    // Inner `local x = "str"` shadows the outer typed declaration —
    // the inner assignment must NOT be reported against the outer
    // `---@type number`.
    let src = r#"
---@type number
local x = 0
do
    local x = "inner"
    x = "still string"
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "reassign_shadow.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.emmy_type_mismatch = DiagnosticSeverityOption::Error;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Type mismatch on assignment")),
        "shadowed inner `x` reassignments must not flag outer number type, got: {:?}", diags,
    );
}

// ---------------------------------------------------------------------------
// P2-3 — argument count / type mismatch
// ---------------------------------------------------------------------------

#[test]
fn argument_count_mismatch_too_many() {
    let src = r#"
local function f(a, b) return a + b end
f(1, 2, 3)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argcount_too_many.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_count_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("argument(s)"))
        .collect();
    assert_eq!(mismatches.len(), 1, "should flag over-arity, got: {:?}", diags);
    assert!(mismatches[0].message.contains("expected 2"), "expected 2 params: {}", mismatches[0].message);
}

#[test]
fn argument_count_mismatch_too_few() {
    let src = r#"
local function f(a, b) return a + b end
f(1)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argcount_too_few.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_count_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("argument(s)"))
        .collect();
    assert_eq!(mismatches.len(), 1, "should flag under-arity, got: {:?}", diags);
}

#[test]
fn argument_count_vararg_absorbs_extras() {
    let src = r#"
local function f(a, ...) return a end
f(1, 2, 3, 4, 5)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argcount_vararg.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_count_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("argument(s)")),
        "vararg must absorb extras, got: {:?}", diags,
    );
}

#[test]
fn argument_count_method_call_hides_self() {
    // `:` call passes `self` implicitly; the user-visible arg list
    // should match the `@field`-declared params after hiding `self`.
    let src = r#"
---@class Greeter
---@field hello fun(self: Greeter, name: string)
local g = nil

---@type Greeter
g = g

g:hello("world")
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argcount_method.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_count_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    // The call passes 1 visible arg ("world"); the signature has
    // 2 params (self, name); after hiding `self` it's 1 — match.
    assert!(
        diags.iter().all(|d| !d.message.contains("argument(s)")),
        "method call with matching visible arg count should not flag, got: {:?}", diags,
    );
}

#[test]
fn argument_count_overload_accepting_clears_diagnostic() {
    // One overload takes 1 arg, another takes 2 — calling with 1 arg
    // matches an overload, so nothing should be reported.
    let src = r#"
---@overload fun(a: number)
---@overload fun(a: number, b: number)
local function f(a, b) return a end
f(1)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argcount_overload.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_count_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("argument(s)")),
        "any overload match clears the count diagnostic, got: {:?}", diags,
    );
}

#[test]
fn argument_count_off_by_default() {
    let src = r#"
local function f(a) return a end
f(1, 2, 3)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argcount_default.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("argument(s)")),
        "argument count check is off by default, got: {:?}", diags,
    );
}

#[test]
fn argument_type_mismatch_basic() {
    let src = r#"
---@param a number
---@param b string
local function f(a, b) return a end
f("str", 42)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argtype.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_type_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let type_mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.starts_with("Argument "))
        .collect();
    assert_eq!(
        type_mismatches.len(), 2,
        "both args flagged: str->number, 42->string, got: {:?}", diags,
    );
}

// ---------------------------------------------------------------------------
// P2-3 — @return mismatch
// ---------------------------------------------------------------------------

#[test]
fn return_count_mismatch_reports() {
    let src = r#"
---@return number, string
local function f()
    return 42
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_count.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.return_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let return_mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Return statement yields"))
        .collect();
    assert_eq!(return_mismatches.len(), 1, "got: {:?}", diags);
    assert!(return_mismatches[0].message.contains("expected 2"));
}

#[test]
fn return_type_mismatch_reports() {
    let src = r#"
---@return number
local function f()
    return "str"
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_type.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.return_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let return_mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Return value"))
        .collect();
    assert_eq!(return_mismatches.len(), 1, "got: {:?}", diags);
}

#[test]
fn return_mismatch_nested_return_inside_if() {
    let src = r#"
---@return number
local function f(x)
    if x > 0 then
        return "bad"
    end
    return 0
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_nested.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.return_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    // The string return inside `if` should be flagged as type mismatch,
    // the outer `return 0` is correct.
    let return_mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.contains("Return value"))
        .collect();
    assert_eq!(return_mismatches.len(), 1, "nested return must be walked, got: {:?}", diags);
}

#[test]
fn return_mismatch_nested_function_scope_isolation() {
    // `return "str"` inside an inner function must NOT count against
    // the outer `---@return number` declaration.
    let src = r#"
---@return number
local function outer()
    local inner = function() return "str" end
    return 0
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_nested_fn.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.return_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Return")),
        "nested function's returns must not count against outer, got: {:?}", diags,
    );
}

#[test]
fn return_mismatch_off_by_default() {
    let src = r#"
---@return number
local function f() return "str" end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_off.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Return")),
        "return mismatch default off, got: {:?}", diags,
    );
}

#[test]
fn return_mismatch_skips_tail_call_expansion() {
    // Lua semantics: `return foo()` expands to whatever values foo()
    // returns. Static count comparison can't know the expansion size,
    // so we skip such returns to avoid false positives.
    let src = r#"
local function two() return 1, "s" end

---@return number, string
local function wrap()
    return two()
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_tailcall.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.return_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Return")),
        "tail call return should not be flagged, got: {:?}", diags,
    );
}

#[test]
fn return_mismatch_skips_vararg_expansion() {
    // `return ...` similarly expands to any number of values.
    let src = r#"
---@return number, string
local function pass(...)
    return ...
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "return_vararg.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.return_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Return")),
        "vararg return should not be flagged, got: {:?}", diags,
    );
}

#[test]
fn argument_type_mismatch_reads_emmy_annotated_local() {
    // Regression: previously `infer_literal_type` refused to return
    // an EmmyAnnotation-sourced type, so passing a `---@type string`
    // local into a `@param n number` slot slipped through silently.
    let src = r#"
---@param n number
local function takes_number(n) return n end

---@type string
local s = "hi"
takes_number(s)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "argtype_emmy.lua");
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_type_mismatch = DiagnosticSeverityOption::Warning;
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(), src.as_bytes(), &uri,
        &mut agg, &doc.scope_tree, &cfg,
    );
    let mismatches: Vec<_> = diags.iter()
        .filter(|d| d.message.starts_with("Argument "))
        .collect();
    assert_eq!(
        mismatches.len(), 1,
        "passing @type string to @param number must be flagged, got: {:?}", diags,
    );
}
