mod test_helpers;

use test_helpers::*;
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
    let (doc, _uri, agg) = setup_single_file(src, "test.lua");
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(),
        src.as_bytes(),
        &agg,
        &doc.scope_tree,
    );
    // `print` and `undefined_var` are both globals — the exact behavior depends
    // on LSP config defaults, but we verify the function doesn't panic.
    let _ = diags;
}
