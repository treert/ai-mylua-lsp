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
fn references_local_rebind_does_not_claim_outer_rhs() {
    // `local x = x + 1` rebinds `x`; the RHS `x` on the same line refers to
    // the OUTER `x`, not the newly-declared one. Clicking on the new `x`
    // must not return the RHS occurrence as a reference.
    let src = "local x = 1\ndo\n  local x = x + 1\n  print(x)\nend";
    let (doc, uri, agg) = setup_single_file(src, "rebind.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click the inner `local x` on line 2 column 8
    let result = references::find_references(
        doc, &uri, pos(2, 8), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find references for inner x");

    // Inner `x` occurrences: the declaration itself (line 2, col 8) and
    // `print(x)` (line 3, col 8). The RHS `x` on line 2 col 12 must NOT
    // be included (it refers to the outer x).
    let inner_decl = result.iter().find(|l| l.range.start.line == 2 && l.range.start.character == 8);
    assert!(inner_decl.is_some(), "should include inner decl itself: {:?}", result);

    let printed = result.iter().find(|l| l.range.start.line == 3 && l.range.start.character == 8);
    assert!(printed.is_some(), "should include use inside the block: {:?}", result);

    let rhs_read = result.iter().find(|l| l.range.start.line == 2 && l.range.start.character == 12);
    assert!(
        rhs_read.is_none(),
        "RHS `x` on `local x = x + 1` must not be a reference of inner x, got: {:?}",
        result,
    );
}

#[test]
fn references_shadowed_outer_not_claimed_by_inner() {
    // Reverse direction: clicking on the OUTER x should not include inner
    // uses after shadowing.
    let src = "local x = 1\nprint(x)\ndo\n  local x = 2\n  print(x)\nend\nprint(x)";
    let (doc, uri, agg) = setup_single_file(src, "shadow.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click the outer `local x` on line 0 col 6
    let result = references::find_references(
        doc, &uri, pos(0, 6), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find references for outer x");

    // The `print(x)` on line 4 (inside the inner `do`) uses the SHADOWED
    // inner x; it must not be returned.
    let inner_print = result.iter().find(|l| l.range.start.line == 4);
    assert!(
        inner_print.is_none(),
        "shadowed inner use must not appear as a reference to outer x, got: {:?}",
        result,
    );

    // The two print(x) on line 1 and line 6 should both be included.
    assert!(
        result.iter().any(|l| l.range.start.line == 1),
        "outer use on line 1 should be present: {:?}", result,
    );
    assert!(
        result.iter().any(|l| l.range.start.line == 6),
        "outer use on line 6 (after do-block) should be present: {:?}", result,
    );
}

#[test]
fn references_emmy_type_scans_annotations() {
    // Regression: Emmy type names appearing inside `---@...` lines are not
    // identifier AST nodes, so the regular identifier scan misses them.
    // find_references must also scan emmy_comment text for the type name.
    use std::collections::HashMap;
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document, scope,
                    summary_builder};

    let mut parser = new_parser();
    let defn_src = "---@class Foo\n---@field x number\nFoo = {}";
    let defn_uri = make_uri("defn.lua");
    let defn_tree = parser.parse(defn_src.as_bytes(), None).unwrap();
    let defn_summary = summary_builder::build_summary(&defn_uri, &defn_tree, defn_src.as_bytes());
    let defn_scope = scope::build_scope_tree(&defn_tree, defn_src.as_bytes(), &mylua_lsp::util::LineIndex::new(defn_src.as_bytes()));
    let defn_doc = Document { text: defn_src.to_string(), tree: defn_tree, scope_tree: defn_scope, line_index: util::LineIndex::new(defn_src.as_bytes()) };

    // Three distinct Emmy mentions of Foo in different annotation positions.
    let user_src = r#"---@type Foo
local a = nil
---@param x Foo
---@return Foo
local function use(x) return x end
---@class Bar : Foo
Bar = {}"#;
    let user_uri = make_uri("user.lua");
    let user_tree = parser.parse(user_src.as_bytes(), None).unwrap();
    let user_summary = summary_builder::build_summary(&user_uri, &user_tree, user_src.as_bytes());
    let user_scope = scope::build_scope_tree(&user_tree, user_src.as_bytes(), &mylua_lsp::util::LineIndex::new(user_src.as_bytes()));
    let user_doc = Document { text: user_src.to_string(), tree: user_tree, scope_tree: user_scope, line_index: util::LineIndex::new(user_src.as_bytes()) };

    let mut agg = WorkspaceAggregation::new();
    agg.upsert_summary(defn_summary);
    agg.upsert_summary(user_summary);

    let docs = HashMap::from([
        (defn_uri.clone(), defn_doc),
        (user_uri.clone(), user_doc),
    ]);
    let defn_doc_ref = docs.get(&defn_uri).unwrap();

    // Click on `Foo` in its `---@class Foo` header (line 0, col 10).
    let locs = references::find_references(
        defn_doc_ref, &defn_uri, pos(0, 10), true,
        &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("references should resolve for Emmy class name");

    // Count how many Foo annotation references we found in user.lua.
    let user_refs: Vec<_> = locs.iter().filter(|l| l.uri == user_uri).collect();
    assert!(
        user_refs.len() >= 4,
        "should find at least 4 annotation references to Foo (@type, @param, @return, : Foo), got {}: {:?}",
        user_refs.len(), user_refs,
    );

    // Also must not emit a spurious match inside `function` / other words
    // that merely contain "Foo" substrings. Sanity check.
    for l in &user_refs {
        let text = &docs[&user_uri].text;
        let line = text.lines().nth(l.range.start.line as usize).unwrap_or("");
        // Every reported range should actually sit at a Foo occurrence.
        assert!(
            line.contains("Foo"),
            "reference line {} does not contain Foo: {:?}",
            l.range.start.line, line,
        );
    }
}

#[test]
fn references_emmy_type_word_boundary() {
    // Must not match `FooBar` when the clicked type is `Foo`.
    use std::collections::HashMap;
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document, scope,
                    summary_builder};

    let mut parser = new_parser();
    let defn_src = "---@class Foo\nFoo = {}";
    let defn_uri = make_uri("d.lua");
    let defn_tree = parser.parse(defn_src.as_bytes(), None).unwrap();
    let defn_summary = summary_builder::build_summary(&defn_uri, &defn_tree, defn_src.as_bytes());
    let defn_scope = scope::build_scope_tree(&defn_tree, defn_src.as_bytes(), &mylua_lsp::util::LineIndex::new(defn_src.as_bytes()));
    let defn_doc = Document { text: defn_src.to_string(), tree: defn_tree, scope_tree: defn_scope, line_index: util::LineIndex::new(defn_src.as_bytes()) };

    let user_src = "---@type FooBar\nlocal x = nil";
    let user_uri = make_uri("u.lua");
    let user_tree = parser.parse(user_src.as_bytes(), None).unwrap();
    let user_summary = summary_builder::build_summary(&user_uri, &user_tree, user_src.as_bytes());
    let user_scope = scope::build_scope_tree(&user_tree, user_src.as_bytes(), &mylua_lsp::util::LineIndex::new(user_src.as_bytes()));
    let user_doc = Document { text: user_src.to_string(), tree: user_tree, scope_tree: user_scope, line_index: util::LineIndex::new(user_src.as_bytes()) };

    let mut agg = WorkspaceAggregation::new();
    agg.upsert_summary(defn_summary);
    agg.upsert_summary(user_summary);

    let docs = HashMap::from([
        (defn_uri.clone(), defn_doc),
        (user_uri.clone(), user_doc),
    ]);
    let defn_doc_ref = docs.get(&defn_uri).unwrap();

    let locs = references::find_references(
        defn_doc_ref, &defn_uri, pos(0, 10), true,
        &agg, &docs, &ReferencesStrategy::Best,
    )
    .unwrap_or_default();

    // Only the class declaration itself (defn.lua) should match; no `FooBar`
    // in user.lua.
    for l in &locs {
        assert_ne!(l.uri, user_uri, "Foo must not match `FooBar`: {:?}", locs);
    }
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
