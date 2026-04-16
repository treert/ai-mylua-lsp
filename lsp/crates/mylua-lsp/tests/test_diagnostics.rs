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
