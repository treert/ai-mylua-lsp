mod test_helpers;

use std::collections::HashMap;
use test_helpers::*;
use mylua_lsp::config::ReferencesStrategy;
use mylua_lsp::references;
use mylua_lsp::uri_id::intern;

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
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document,
                    summary_builder};

    let mut parser = new_parser();
    let defn_src = "---@class Foo\n---@field x number\nFoo = {}";
    let defn_uri = make_uri("defn.lua");
    let defn_tree = parser.parse(defn_src.as_bytes(), None).unwrap();
    let defn_summary = summary_builder::build_file_analysis(&defn_uri, &defn_tree, defn_src.as_bytes(), &mylua_lsp::util::LineIndex::new(defn_src.as_bytes()));
    let defn_doc = Document { lua_source: mylua_lsp::util::LuaSource::new(defn_src.to_string()), tree: defn_tree, scope_tree: defn_summary.1 };
    let defn_summary = defn_summary.0;

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
    let user_result = summary_builder::build_file_analysis(&user_uri, &user_tree, user_src.as_bytes(), &mylua_lsp::util::LineIndex::new(user_src.as_bytes()));
    let user_doc = Document { lua_source: mylua_lsp::util::LuaSource::new(user_src.to_string()), tree: user_tree, scope_tree: user_result.1 };
    let user_summary = user_result.0;

    let mut agg = WorkspaceAggregation::new();
    let defn_uri_id = intern(defn_uri.clone());
    let user_uri_id = intern(user_uri.clone());
    agg.upsert_summary(defn_uri_id, defn_summary);
    agg.upsert_summary(user_uri_id, user_summary);

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
        let text = docs[&user_uri].text();
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
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document,
                    summary_builder};

    let mut parser = new_parser();
    let defn_src = "---@class Foo\nFoo = {}";
    let defn_uri = make_uri("d.lua");
    let defn_tree = parser.parse(defn_src.as_bytes(), None).unwrap();
    let defn_summary = summary_builder::build_file_analysis(&defn_uri, &defn_tree, defn_src.as_bytes(), &mylua_lsp::util::LineIndex::new(defn_src.as_bytes()));
    let defn_doc = Document { lua_source: mylua_lsp::util::LuaSource::new(defn_src.to_string()), tree: defn_tree, scope_tree: defn_summary.1 };
    let defn_summary = defn_summary.0;

    let user_src = "---@type FooBar\nlocal x = nil";
    let user_uri = make_uri("u.lua");
    let user_tree = parser.parse(user_src.as_bytes(), None).unwrap();
    let user_result = summary_builder::build_file_analysis(&user_uri, &user_tree, user_src.as_bytes(), &mylua_lsp::util::LineIndex::new(user_src.as_bytes()));
    let user_doc = Document { lua_source: mylua_lsp::util::LuaSource::new(user_src.to_string()), tree: user_tree, scope_tree: user_result.1 };
    let user_summary = user_result.0;

    let mut agg = WorkspaceAggregation::new();
    let defn_uri_id = intern(defn_uri.clone());
    let user_uri_id = intern(user_uri.clone());
    agg.upsert_summary(defn_uri_id, defn_summary);
    agg.upsert_summary(user_uri_id, user_summary);

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

#[test]
fn references_field_same_type_different_variable() {
    // Two local variables typed as the same class: references to a field
    // on one should include accesses on the other.
    use std::collections::HashMap;
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document, summary_builder};

    let mut parser = new_parser();
    let src = r#"---@class Player
---@field hp number
---@field name string

---@type Player
local a = {}
a.hp = 100

---@type Player
local b = {}
b.hp = 50
print(b.name)"#;
    let uri = make_uri("field_ref.lua");
    let tree = parser.parse(src.as_bytes(), None).unwrap();
    let result = summary_builder::build_file_analysis(
        &uri, &tree, src.as_bytes(), &mylua_lsp::util::LineIndex::new(src.as_bytes()),
    );
    let doc = Document {
        lua_source: mylua_lsp::util::LuaSource::new(src.to_string()),
        tree,
        scope_tree: result.1,
    };

    let mut agg = WorkspaceAggregation::new();
    let uri_id = intern(uri.clone());
    agg.upsert_summary(uri_id, result.0);

    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click on `hp` in `a.hp = 100` (line 6, col 2)
    let locs = references::find_references(
        doc, &uri, pos(6, 2), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find field references for hp");

    // Should find: declaration (@field hp), a.hp, b.hp — at least 3
    assert!(
        locs.len() >= 3,
        "expected at least 3 references to field `hp` (decl + a.hp + b.hp), got {}: {:?}",
        locs.len(), locs,
    );

    // b.name should NOT be included
    let name_refs: Vec<_> = locs.iter().filter(|l| {
        let line = src.lines().nth(l.range.start.line as usize).unwrap_or("");
        line.contains("name")
    }).collect();
    assert!(name_refs.is_empty(), "hp references must not include 'name' accesses: {:?}", name_refs);
}

#[test]
fn references_field_across_files_returns_definition_and_usage_uris() {
    let def_src = r#"---@class Player
---@field hp number
local Player = {}
return Player"#;
    let use_src = r#"---@type Player
local player = require("player")
player.hp = 100"#;

    let (docs, agg, _parser) = setup_workspace(&[
        ("player.lua", def_src),
        ("main.lua", use_src),
    ]);
    let def_uri = make_uri("player.lua");
    let use_uri = make_uri("main.lua");
    let use_doc = docs.get(&use_uri).unwrap();

    let locations = references::find_references(
        use_doc, &use_uri, pos(2, 7), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find cross-file field references for hp");

    assert!(
        locations.iter().any(|loc| loc.uri == def_uri),
        "field references should include the declaration URI: {:?}",
        locations,
    );
    assert!(
        locations.iter().any(|loc| loc.uri == use_uri),
        "field references should include the usage URI: {:?}",
        locations,
    );
}

#[test]
fn references_field_inference_fails_no_false_positive() {
    // When the base type cannot be inferred, the field should not be reported.
    let src = r#"local unknown = getStuff()
unknown.foo = 1

local other = getOther()
other.foo = 2"#;
    let (doc, uri, agg) = setup_single_file(src, "no_type.lua");
    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click on `foo` in `unknown.foo` (line 1, col 8)
    let result = references::find_references(
        doc, &uri, pos(1, 8), true, &agg, &docs, &ReferencesStrategy::Best,
    );
    // Should return Some (can still be a global identity or produce empty
    // results) — the key assertion is it must NOT panic and must NOT include
    // `other.foo` as a reference (different unresolved base).
    if let Some(locs) = result {
        // Since neither base resolves to a known type, identify_at_cursor
        // should fall through to Global or return None. Either way, we
        // should not get cross-variable matches.
        for l in &locs {
            // Verify no match on line 4 (other.foo)
            if l.range.start.line == 4 {
                // This is acceptable ONLY if the identity was Global (name="foo")
                // which should not happen because `foo` is in field position.
                panic!(
                    "field ref with unresolved base should not cross-match: {:?}",
                    locs
                );
            }
        }
    }
}

#[test]
fn references_field_via_inheritance() {
    // A field declared on a parent class should be found when accessed on a child.
    use std::collections::HashMap;
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document, summary_builder};

    let mut parser = new_parser();
    let src = r#"---@class Animal
---@field legs number

---@class Dog : Animal
Dog = {}

---@type Animal
local a = {}
a.legs = 4

---@type Dog
local d = {}
d.legs = 4"#;
    let uri = make_uri("inherit.lua");
    let tree = parser.parse(src.as_bytes(), None).unwrap();
    let result = summary_builder::build_file_analysis(
        &uri, &tree, src.as_bytes(), &mylua_lsp::util::LineIndex::new(src.as_bytes()),
    );
    let doc = Document {
        lua_source: mylua_lsp::util::LuaSource::new(src.to_string()),
        tree,
        scope_tree: result.1,
    };

    let mut agg = WorkspaceAggregation::new();
    let uri_id = intern(uri.clone());
    agg.upsert_summary(uri_id, result.0);

    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click on `legs` in `a.legs = 4` (line 8, col 2)
    let locs = references::find_references(
        doc, &uri, pos(8, 2), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find field references for legs");

    // Should find: @field legs declaration, a.legs, d.legs — at least 3
    assert!(
        locs.len() >= 3,
        "expected at least 3 references to `legs` (decl + a.legs + d.legs), got {}: {:?}",
        locs.len(), locs,
    );

    // Verify d.legs is included (line 12)
    let dog_ref = locs.iter().find(|l| l.range.start.line == 12);
    assert!(
        dog_ref.is_some(),
        "d.legs (Dog inherits Animal.legs) should be included: {:?}",
        locs,
    );
}

#[test]
fn references_field_on_local_class_function_name() {
    // Regression: `function ClassB1:bbb()` where ClassB1 is a local variable.
    // Clicking on `bbb` at the declaration site must find references including
    // `self:bbb()` inside `test_bbb`. Previously this returned empty because
    // resolve_segments_to_field used byte offset 0 which is before the local
    // ClassB1's visibility range.
    use std::collections::HashMap;
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document, summary_builder};

    let mut parser = new_parser();
    let src = r#"---@class ClassB1
local ClassB1 = class("ClassB1")

function ClassB1:bbb()
    print("bbb")
end

function ClassB1:test_bbb()
    self:bbb()
end"#;
    let uri = make_uri("local_class.lua");
    let tree = parser.parse(src.as_bytes(), None).unwrap();
    let result = summary_builder::build_file_analysis(
        &uri, &tree, src.as_bytes(), &mylua_lsp::util::LineIndex::new(src.as_bytes()),
    );
    let doc = Document {
        lua_source: mylua_lsp::util::LuaSource::new(src.to_string()),
        tree,
        scope_tree: result.1,
    };

    let mut agg = WorkspaceAggregation::new();
    let uri_id = intern(uri.clone());
    agg.upsert_summary(uri_id, result.0);

    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click on `bbb` in `function ClassB1:bbb()` (line 3, col 18)
    let locs = references::find_references(
        doc, &uri, pos(3, 18), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find references for method bbb on local class");

    // Should find: declaration (ClassB1:bbb) + self:bbb() — at least 2
    assert!(
        locs.len() >= 2,
        "expected at least 2 references to `bbb` (decl + self:bbb()), got {}: {:?}",
        locs.len(), locs,
    );

    // self:bbb() is on line 8
    let self_call = locs.iter().find(|l| l.range.start.line == 8);
    assert!(
        self_call.is_some(),
        "self:bbb() call should be found as a reference: {:?}",
        locs,
    );
}

#[test]
fn references_field_on_local_class_table_literal() {
    // Same as references_field_on_local_class_function_name but with `= {}`
    // instead of `= class(...)`. This tests the table-shape path:
    // bound_class resolves to EmmyType, while type_fact is Table(shape_id).
    // Both paths should agree on def_range so find_references works from
    // both declaration site and usage site.
    use std::collections::HashMap;
    use mylua_lsp::{aggregation::WorkspaceAggregation, document::Document, summary_builder};

    let mut parser = new_parser();
    let src = r#"---@class MyObj
local MyObj = {}

function MyObj:hello()
    print("hello")
end

function MyObj:caller()
    self:hello()
end"#;
    let uri = make_uri("local_table_class.lua");
    let tree = parser.parse(src.as_bytes(), None).unwrap();
    let result = summary_builder::build_file_analysis(
        &uri, &tree, src.as_bytes(), &mylua_lsp::util::LineIndex::new(src.as_bytes()),
    );
    let doc = Document {
        lua_source: mylua_lsp::util::LuaSource::new(src.to_string()),
        tree,
        scope_tree: result.1,
    };

    let mut agg = WorkspaceAggregation::new();
    let uri_id = intern(uri.clone());
    agg.upsert_summary(uri_id, result.0);

    let docs = HashMap::from([(uri.clone(), doc)]);
    let doc = docs.get(&uri).unwrap();

    // Click on `hello` at declaration: `function MyObj:hello()` (line 3, col 15)
    let from_decl = references::find_references(
        doc, &uri, pos(3, 15), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find references for hello from declaration");

    // Click on `hello` at usage: `self:hello()` (line 8, col 9)
    let from_usage = references::find_references(
        doc, &uri, pos(8, 9), true, &agg, &docs, &ReferencesStrategy::Best,
    )
    .expect("should find references for hello from usage");

    // Both should find at least 2 (declaration + self:hello())
    assert!(
        from_decl.len() >= 2,
        "from declaration site: expected >= 2, got {}: {:?}",
        from_decl.len(), from_decl,
    );
    assert!(
        from_usage.len() >= 2,
        "from usage site: expected >= 2, got {}: {:?}",
        from_usage.len(), from_usage,
    );

    // Both should return the same set of locations
    assert_eq!(
        from_decl, from_usage,
        "references from declaration and usage should match",
    );
}
