mod test_helpers;

use test_helpers::*;

/// P1-7 — `WorkspaceAggregation.type_dependants` must reverse-index
/// every Emmy type name referenced in a summary so that cascade
/// diagnostic re-computation can walk the graph when a class
/// definition changes. This file exercises the data-structure layer
/// directly; the lib.rs `collect_dependant_uris` path that consumes
/// `type_dependants` is integration-level and would require an LSP
/// runtime to observe diagnostic scheduling.

#[test]
fn type_dependants_registers_at_type_ref() {
    let a = (
        "a.lua",
        "---@class Foo\n---@field x integer\nFoo = {}\n",
    );
    let b = (
        "b.lua",
        "---@type Foo\nlocal f = nil\nprint(f)\n",
    );
    let (_docs, agg, _parser) = setup_workspace(&[a, b]);
    let b_uri = make_uri("b.lua");

    let deps = agg.type_dependants.get("Foo")
        .expect("Foo should have dependants registered");
    assert!(
        deps.iter().any(|u| u == &b_uri),
        "b.lua should be listed as a Foo-dependant, got: {:?}", deps,
    );
}

#[test]
fn type_dependants_registers_param_return_and_inheritance() {
    let a = (
        "a.lua",
        "---@class Foo\nFoo = {}\n",
    );
    let b = (
        "b.lua",
        "---@param x Foo\n---@return Foo\nfunction use(x) return x end\n",
    );
    let c = (
        "c.lua",
        "---@class Bar : Foo\nBar = {}\n",
    );
    let (_docs, agg, _parser) = setup_workspace(&[a, b, c]);
    let b_uri = make_uri("b.lua");
    let c_uri = make_uri("c.lua");

    let deps = agg.type_dependants.get("Foo").expect("Foo deps");
    assert!(deps.iter().any(|u| u == &b_uri), "b.lua via @param/@return");
    assert!(deps.iter().any(|u| u == &c_uri), "c.lua via @class : Foo");
}

#[test]
fn type_dependants_registers_field_type_and_alias() {
    let a = (
        "a.lua",
        "---@class Foo\nFoo = {}\n",
    );
    let b = (
        "b.lua",
        "---@class Container\n---@field first Foo\n---@field second Foo\nContainer = {}\n",
    );
    let c = (
        "c.lua",
        "---@alias FooAlias Foo\n",
    );
    let (_docs, agg, _parser) = setup_workspace(&[a, b, c]);
    let b_uri = make_uri("b.lua");
    let c_uri = make_uri("c.lua");

    let deps = agg.type_dependants.get("Foo").expect("Foo deps");
    assert!(deps.iter().any(|u| u == &b_uri), "b.lua via @field");
    assert!(deps.iter().any(|u| u == &c_uri), "c.lua via @alias");
}

#[test]
fn type_dependants_excludes_self_defining_file() {
    // `a.lua` defines `Foo` itself — it must NOT list itself as a
    // dependant (avoid pointless self-invalidation loops).
    let a = (
        "a.lua",
        "---@class Foo\n---@field inner Foo\nFoo = {}\n",
    );
    let (_docs, agg, _parser) = setup_workspace(&[a]);
    let a_uri = make_uri("a.lua");

    // `Foo` should either have no entry at all (all refs were self-
    // references, filtered out), or an entry that does NOT contain
    // a.lua itself.
    if let Some(deps) = agg.type_dependants.get("Foo") {
        assert!(
            !deps.iter().any(|u| u == &a_uri),
            "self-defining file must be excluded from its own dependants, got: {:?}", deps,
        );
    }
}

#[test]
fn type_dependants_updates_on_resummary() {
    // When `b.lua` is re-indexed (e.g. user edits it to stop
    // referencing Foo), its entry must be removed from
    // `type_dependants["Foo"]`.
    let mut parser = new_parser();
    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    let a_uri = make_uri("a.lua");
    let b_uri = make_uri("b.lua");

    let a_src = "---@class Foo\nFoo = {}\n";
    let b_src_with = "---@type Foo\nlocal f = nil\n";
    let b_src_without = "local n = 1\n";

    // Initial: both files reference Foo; a defines it, b uses it.
    for (uri, src) in &[(&a_uri, a_src), (&b_uri, b_src_with)] {
        let doc = parse_doc(&mut parser, src);
        let summary = mylua_lsp::summary_builder::build_summary(uri, &doc.tree, doc.source(), doc.line_index());
        agg.upsert_summary(summary);
    }
    assert!(
        agg.type_dependants.get("Foo").map_or(false, |v| v.contains(&b_uri)),
        "initial: b.lua should be a Foo-dependant",
    );

    // Re-index b.lua without the `---@type Foo` reference.
    let doc = parse_doc(&mut parser, b_src_without);
    let summary = mylua_lsp::summary_builder::build_summary(
        &b_uri, &doc.tree, doc.source(), doc.line_index()
    );
    agg.upsert_summary(summary);

    let still_present = agg.type_dependants.get("Foo")
        .map_or(false, |v| v.contains(&b_uri));
    assert!(!still_present, "after re-summary without Foo ref, b.lua must be purged from type_dependants[Foo]");
}

#[test]
fn type_dependants_registers_global_at_type_annotation() {
    // `---@type Foo MyGlobal = nil` stores the typed annotation on a
    // `GlobalContribution` (not `local_type_facts`). Regression guard
    // that the scan catches this source.
    let a = ("a.lua", "---@class Foo\nFoo = {}\n");
    let b = ("b.lua", "---@type Foo\nMyGlobal = nil\n");
    let (_docs, agg, _parser) = setup_workspace(&[a, b]);
    let b_uri = make_uri("b.lua");

    let deps = agg.type_dependants.get("Foo").expect("Foo deps");
    assert!(
        deps.iter().any(|u| u == &b_uri),
        "b.lua with `---@type Foo MyGlobal` should be in Foo-dependants, got: {:?}", deps,
    );
}

#[test]
fn type_dependants_excludes_generic_params() {
    // `---@class Box` + `---@generic T` + `---@field value T` must
    // NOT register `T` as a real external type-dependant key (T is
    // a generic parameter local to Box's scope). Otherwise editing
    // an unrelated `@class T` in a different file would spuriously
    // cascade to this file.
    let src = r#"---@class Box
---@generic T
---@field value T
Box = {}
"#;
    let (_docs, agg, _parser) = setup_workspace(&[("a.lua", src)]);

    assert!(
        agg.type_dependants.get("T").is_none(),
        "generic parameter `T` must not appear as a type_dependants key, got: {:?}",
        agg.type_dependants.get("T"),
    );
}

#[test]
fn type_dependants_preserves_old_name_after_class_rename() {
    // BLOCKING regression: when `a.lua` is edited to rename
    // `@class Foo` → `@class FooBar`, the old `type_dependants["Foo"]`
    // entries must still be retrievable so the lib.rs caller can
    // cascade-invalidate every file that referenced the old name.
    //
    // This test exercises the data-structure invariant only (the
    // lib.rs cascade uses the `old_type_names` snapshot passed to
    // `collect_dependant_uris`). It checks that `type_dependants`
    // itself does NOT drop `Foo` just because `a.lua`'s new summary
    // no longer defines it — the key survives until the dependent
    // file (b.lua) is itself re-indexed.
    let mut parser = new_parser();
    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    let a_uri = make_uri("a.lua");
    let b_uri = make_uri("b.lua");

    let a_before = "---@class Foo\nFoo = {}\n";
    let a_after = "---@class FooBar\nFooBar = {}\n";
    let b_src = "---@type Foo\nlocal f = nil\n";

    for (uri, src) in &[(&a_uri, a_before), (&b_uri, b_src)] {
        let doc = parse_doc(&mut parser, src);
        let summary = mylua_lsp::summary_builder::build_summary(uri, &doc.tree, doc.source(), doc.line_index());
        agg.upsert_summary(summary);
    }

    // Before the rename: `Foo` has b.lua as a dependant.
    assert!(
        agg.type_dependants.get("Foo").map_or(false, |v| v.contains(&b_uri)),
        "initial state: b.lua must be a Foo-dependant",
    );

    // Rename `@class Foo` → `@class FooBar` in a.lua.
    let doc = parse_doc(&mut parser, a_after);
    let summary = mylua_lsp::summary_builder::build_summary(&a_uri, &doc.tree, doc.source(), doc.line_index());
    agg.upsert_summary(summary);

    // `type_dependants["Foo"]` must STILL include b.lua — b.lua
    // hasn't been re-indexed yet and still references `Foo`. Only
    // re-indexing b.lua would drop it (tested by
    // `type_dependants_updates_on_resummary`).
    assert!(
        agg.type_dependants.get("Foo").map_or(false, |v| v.contains(&b_uri)),
        "after rename in a.lua, b.lua must still be in type_dependants[Foo] (it still references Foo)",
    );
}

#[test]
fn type_dependants_cleared_on_remove_file() {
    let a = ("a.lua", "---@class Foo\nFoo = {}\n");
    let b = ("b.lua", "---@type Foo\nlocal f = nil\n");
    let (_docs, mut agg, _parser) = setup_workspace(&[a, b]);
    let b_uri = make_uri("b.lua");

    assert!(
        agg.type_dependants.get("Foo").map_or(false, |v| v.contains(&b_uri)),
        "before remove: b.lua present",
    );

    agg.remove_file(&b_uri);

    let still = agg.type_dependants.get("Foo").map_or(false, |v| v.contains(&b_uri));
    assert!(!still, "after remove_file, b.lua must be gone from type_dependants");
}
