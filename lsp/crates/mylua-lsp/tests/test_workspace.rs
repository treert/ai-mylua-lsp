mod test_helpers;

use test_helpers::*;

/// When the same global is defined with `---@type` in two files at different path
/// depths, the shallower file (fewer path segments) should win in resolution.
#[test]
fn workspace_global_priority_by_path_depth() {
    use mylua_lsp::resolver;
    use mylua_lsp::type_system::{KnownType, SymbolicStub, TypeFact};

    let shallow_file = ("test_utils.lua", "---@type SubClass\nGLOBAL.Foo = nil\n");
    let deep_file = (
        "deep/nested/base_stub.lua",
        "---@type BaseClass\nGLOBAL.Foo = nil\n",
    );

    // Insert deep file first, then shallow — sorting should still put shallow first
    let (_docs, mut agg, _parser) = setup_workspace(&[deep_file, shallow_file]);

    let candidates = agg
        .global_shard
        .get("GLOBAL.Foo")
        .expect("GLOBAL.Foo should be in global_shard");
    assert_eq!(candidates.len(), 2, "should have two candidates");

    // Verify resolver picks the shallow file's type (SubClass)
    let owner_uri_id = summary_id_by_uri(&agg, &make_uri("test_utils.lua"));
    let resolved = resolver::resolve_type(
        owner_uri_id,
        &TypeFact::Stub(SymbolicStub::GlobalRef {
            name: "GLOBAL.Foo".into(),
        }),
        &mut agg,
    );
    match &resolved.type_fact {
        TypeFact::Known(KnownType::EmmyType(name)) => {
            assert_eq!(
                name, "SubClass",
                "resolver should pick the shallower file's type"
            );
        }
        other => panic!("expected EmmyType(SubClass), got {:?}", other),
    }
}

/// Regression for bug where `upsert_summary` dropped a file's `require_map`
/// entries as a side effect of `remove_contributions`. After editing (re-upserting)
/// a file, other files that `require()` it must still resolve to it.
#[test]
fn require_map_survives_upsert() {
    use mylua_lsp::summary_builder;

    let mut parser = new_parser();
    let mod_uri = make_uri("mymod.lua");
    let mod_uri_id = intern_uri(&mod_uri);
    let mod_src = "return { x = 1 }";
    let mod_doc = parse_doc(&mut parser, mod_src);
    let mod_summary = summary_builder::build_file_analysis(
        &mod_uri,
        mod_doc.tree().unwrap(),
        mod_doc.source(),
        mod_doc.line_index(),
    )
    .0;

    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    agg.set_require_mapping("mymod".to_string(), mod_uri_id);
    agg.upsert_summary(mod_uri_id, mod_summary);

    assert_eq!(
        agg.resolve_module_to_id("mymod"),
        Some(mod_uri_id),
        "baseline: require(\"mymod\") should resolve before any edit"
    );

    let new_src = "return { x = 2, y = 3 }";
    let new_doc = parse_doc(&mut parser, new_src);
    let new_summary = summary_builder::build_file_analysis(
        &mod_uri,
        new_doc.tree().unwrap(),
        new_doc.source(),
        new_doc.line_index(),
    )
    .0;
    agg.upsert_summary(mod_uri_id, new_summary);

    assert_eq!(
        agg.resolve_module_to_id("mymod"),
        Some(mod_uri_id),
        "after re-upserting (editing) mymod.lua, require(\"mymod\") must still resolve to it",
    );

    agg.remove_file(mod_uri_id);
    assert!(
        agg.resolve_module_to_id("mymod").is_none(),
        "after remove_file, require(\"mymod\") should no longer resolve",
    );
}

#[test]
fn require_resolution_uses_the_same_uri_id_for_module_and_summary() {
    use mylua_lsp::{resolver, summary_builder};

    let mut parser = new_parser();
    let main_uri = make_uri("main.lua");
    let main_uri_id = intern_uri(&main_uri);
    let main_doc = parse_doc(&mut parser, "local Player = require(\"player\")\n");
    let main_summary = summary_builder::build_file_analysis(
        &main_uri,
        main_doc.tree().unwrap(),
        main_doc.source(),
        main_doc.line_index(),
    )
    .0;

    let player_uri = make_uri("player.lua");
    let player_uri_id = intern_uri(&player_uri);
    let player_doc = parse_doc(&mut parser, "Player = {}\nreturn Player\n");
    let player_summary = summary_builder::build_file_analysis(
        &player_uri,
        player_doc.tree().unwrap(),
        player_doc.source(),
        player_doc.line_index(),
    )
    .0;

    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    agg.set_require_mapping("player".to_string(), player_uri_id);
    agg.build_initial(vec![
        (main_uri_id, main_summary),
        (player_uri_id, player_summary),
    ]);

    assert_eq!(
        resolver::resolve_require_global_name("player", &agg).as_deref(),
        Some("Player"),
    );
}

/// Test that uri_priority_key only matches "annotation" as a separate path segment,
/// not as a substring within a directory name (e.g., "my-annotation-helper").
#[test]
fn workspace_global_priority_annotation_path_segment() {
    use mylua_lsp::resolver;
    use mylua_lsp::type_system::{KnownType, SymbolicStub, TypeFact};

    // Case 1: Pure "annotation" directory should be highest priority
    let annotation_file = (
        "annotation/module.lua",
        "---@type SubClass\nGLOBAL.Foo = nil\n",
    );

    // Case 2: "my-annotation-helper" contains substring but is NOT a segment match
    let helper_file = (
        "my-annotation-helper/module.lua",
        "---@type BaseClass\nGLOBAL.Foo = nil\n",
    );

    // Case 3: Regular file for baseline
    let normal_file = (
        "normal/module.lua",
        "---@type OtherClass\nGLOBAL.Foo = nil\n",
    );

    // Setup with all three files
    let (_docs, agg, _parser) = setup_workspace(&[helper_file, normal_file, annotation_file]);

    let candidates = agg
        .global_shard
        .get("GLOBAL.Foo")
        .expect("GLOBAL.Foo should be in global_shard");

    assert_eq!(candidates.len(), 3, "should have three candidates");

    let owner_uri_id = summary_id_by_uri(&agg, &make_uri("annotation/module.lua"));
    let resolved = resolver::resolve_type(
        owner_uri_id,
        &TypeFact::Stub(SymbolicStub::GlobalRef {
            name: "GLOBAL.Foo".into(),
        }),
        &agg,
    );
    match &resolved.type_fact {
        TypeFact::Known(KnownType::EmmyType(name)) => {
            assert_eq!(
                name, "SubClass",
                "annotation/module.lua should be highest priority; substring-only helper path must not win",
            );
        }
        other => panic!("expected EmmyType(SubClass), got {:?}", other),
    }
}
