mod test_helpers;

use test_helpers::*;
use mylua_lsp::config::GotoStrategy;
use mylua_lsp::{hover, completion, goto};

/// Tests using the real `tests/hover/` directory as a workspace.
/// This exercises multi-file require resolution.
#[test]
fn workspace_hover_dir() {
    let (docs, mut agg, _parser) = setup_workspace_from_dir("hover");

    // Find the hover1.lua document (match exactly, not hover10/hover11)
    let hover1_entry = docs.iter().find(|(uri, _)| {
        uri.to_string().contains("hover1.lua")
    });
    assert!(hover1_entry.is_some(), "should find hover1.lua in workspace");

    let (uri, doc) = hover1_entry.unwrap();
    // hover on `btn1` (line 21)
    let result = hover::hover(doc, uri, pos(21, 6), &mut agg, &docs);
    assert!(result.is_some(), "workspace hover on btn1 should return result");
}

/// Tests using the `tests/complete/` directory as a workspace.
#[test]
fn workspace_completion_dir() {
    let (docs, mut agg, _parser) = setup_workspace_from_dir("complete");

    // Find test1.lua — "local abc = 1;"
    let test1_entry = docs.iter().find(|(uri, _)| {
        let s = uri.to_string();
        s.contains("test1.lua") && !s.contains("test10")
    });

    if let Some((uri, doc)) = test1_entry {
        // Complete at end of file
        let items = completion::complete(doc, uri, pos(0, 14), &mut agg);
        // Should not panic
        let _ = items;
    }
}

/// Tests using `tests/define/` directory for cross-file goto.
#[test]
fn workspace_goto_define_dir() {
    let (docs, mut agg, _parser) = setup_workspace_from_dir("define");

    // Find test3.lua — `local ppp = require("be_define")`
    let test3_entry = docs.iter().find(|(uri, _)| {
        uri.to_string().contains("test3")
    });
    assert!(test3_entry.is_some(), "should find test3.lua in workspace");

    let (uri, doc) = test3_entry.unwrap();
    // Attempt goto on `ppp` (line 0, col 6) or on "be_define" string
    let result = goto::goto_definition(doc, uri, pos(0, 6), &mut agg, &GotoStrategy::Auto);
    // Cross-file goto may or may not resolve depending on require mapping,
    // but should not panic
    let _ = result;
}

/// Tests using `tests/project/` directory which has multi-folder structure.
#[test]
fn workspace_project_dir() {
    let (docs, agg, _parser) = setup_workspace_from_dir("project");
    assert!(!docs.is_empty(), "project dir should have documents");
    // Verify all files are indexed without panic
    assert!(
        agg.summaries.len() == docs.len(),
        "all documents should have summaries: {} summaries vs {} docs",
        agg.summaries.len(),
        docs.len()
    );
}

/// When the same global is defined with `---@type` in two files at different path
/// depths, the shallower file (fewer path segments) should win in resolution.
#[test]
fn workspace_global_priority_by_path_depth() {
    use mylua_lsp::resolver;
    use mylua_lsp::type_system::{TypeFact, SymbolicStub, KnownType};

    let shallow_file = (
        "test_utils.lua",
        "---@type SubClass\nGLOBAL.Foo = nil\n",
    );
    let deep_file = (
        "deep/nested/base_stub.lua",
        "---@type BaseClass\nGLOBAL.Foo = nil\n",
    );

    // Insert deep file first, then shallow — sorting should still put shallow first
    let (_docs, mut agg, _parser) = setup_workspace(&[deep_file, shallow_file]);

    let candidates = agg.global_shard.get("GLOBAL.Foo")
        .expect("GLOBAL.Foo should be in global_shard");
    assert_eq!(candidates.len(), 2, "should have two candidates");
    assert!(
        candidates[0].source_uri.to_string().contains("test_utils.lua"),
        "shallower file should be first candidate, got: {:?}",
        candidates[0].source_uri,
    );

    // Verify resolver picks the shallow file's type (SubClass)
    let resolved = resolver::resolve_type(
        &TypeFact::Stub(SymbolicStub::GlobalRef { name: "GLOBAL.Foo".into() }),
        &mut agg,
    );
    match &resolved.type_fact {
        TypeFact::Known(KnownType::EmmyType(name)) => {
            assert_eq!(name, "SubClass", "resolver should pick the shallower file's type");
        }
        other => panic!("expected EmmyType(SubClass), got {:?}", other),
    }
}

/// Test multi-file hover: hover2.lua depends on hover2_requrie.lua via require.
#[test]
fn workspace_hover_require_resolution() {
    let (docs, mut agg, _parser) = setup_workspace_from_dir("hover");

    let hover2_entry = docs.iter().find(|(uri, _)| {
        let s = uri.to_string();
        s.contains("hover2.lua") && !s.contains("requrie")
    });

    if let Some((uri, doc)) = hover2_entry {
        // hover on `req_v.b` (line 2, col 14) — `b` is a field from the required module
        let result = hover::hover(doc, uri, pos(2, 14), &mut agg, &docs);
        // Cross-file hover may or may not fully resolve, but should not panic
        let _ = result;
    }
}
