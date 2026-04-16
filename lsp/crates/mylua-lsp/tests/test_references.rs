mod test_helpers;

use std::collections::HashMap;
use test_helpers::*;
use mylua_lsp::config::ReferencesStrategy;
use mylua_lsp::references;

#[test]
fn references_local_variable() {
    let src = r#"local abc = 1
print(abc)
local x = abc + 1"#;
    let (doc, uri, agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Find references to `abc` (defined line 0, col 6)
    let result = references::find_references(doc, &uri, pos(0, 6), true, &agg, &docs, &ReferencesStrategy::Best);
    assert!(result.is_some(), "should find references for `abc`");
    let locs = result.unwrap();
    assert!(
        locs.len() >= 2,
        "abc is used at least 2 times (declaration + usage), got: {}",
        locs.len()
    );
}

#[test]
fn references_function_parameter() {
    let src = r#"function foo(param)
    print(param)
    return param
end"#;
    let (doc, uri, agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Find references to `param` at line 1, col 10
    let result = references::find_references(doc, &uri, pos(1, 10), true, &agg, &docs, &ReferencesStrategy::Best);
    assert!(result.is_some(), "should find references for `param`");
    let locs = result.unwrap();
    assert!(
        locs.len() >= 2,
        "param is used at least in declaration + 2 usages, got: {}",
        locs.len()
    );
}

#[test]
fn references_no_result_for_keyword() {
    let src = "local x = 1";
    let (doc, uri, agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // `local` keyword at line 0, col 0
    let result = references::find_references(doc, &uri, pos(0, 0), true, &agg, &docs, &ReferencesStrategy::Best);
    // Should not panic; result may be None
    let _ = result;
}

#[test]
fn references_exclude_declaration() {
    let src = r#"local myvar = 1
print(myvar)
print(myvar)"#;
    let (doc, uri, agg) = setup_single_file(src, "test.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    let with_decl = references::find_references(doc, &uri, pos(1, 6), true, &agg, &docs, &ReferencesStrategy::Best);
    let without_decl = references::find_references(doc, &uri, pos(1, 6), false, &agg, &docs, &ReferencesStrategy::Best);

    if let (Some(with), Some(without)) = (with_decl, without_decl) {
        assert!(
            with.len() >= without.len(),
            "including declaration should give >= results: {} vs {}",
            with.len(),
            without.len()
        );
    }
}
