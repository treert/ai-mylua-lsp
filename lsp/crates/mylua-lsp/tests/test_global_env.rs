//! `_G` global-environment alias semantics.
//!
//! Invariant under test: **`_G.X` is an alias of the bare global `X`** — for
//! reads, writes, diagnostics, completion and the `global_shard` key space —
//! *unless* a local named `_G` shadows the global environment in that scope.
//!
//! These tests exist because the aliasing used to be implemented as a handful
//! of scattered `== "_G"` special cases across the summary builder, the
//! resolver and the diagnostics layer. Each new call site had to re-implement
//! the alias, and one of them (the raw-text prefix in `diagnostics/
//! field_access.rs`) got it wrong for the shadowed case. The alias is now
//! normalized once, at the single `global_shard` key entry point.

mod test_helpers;

use mylua_lsp::completion;
use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;
use mylua_lsp::uri_id::intern_uri;
use test_helpers::*;

/// Collect `Unknown field` diagnostic messages for a single-file workspace.
fn unknown_field_diags(src: &str, filename: &str) -> Vec<String> {
    let (doc, uri, mut agg) = setup_single_file(src, filename);
    let cfg = DiagnosticsConfig::default();
    diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    )
    .into_iter()
    .filter(|d| d.message.contains("Unknown field"))
    .map(|d| d.message)
    .collect()
}

/// Sorted `global_shard` entry paths for a single-file workspace.
fn global_shard_paths(src: &str, filename: &str) -> Vec<String> {
    let (_doc, _uri, agg) = setup_single_file(src, filename);
    let mut names: Vec<String> = agg
        .global_shard
        .iter_all_entries()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// Key space: `_G.X` and bare `X` must be the SAME global_shard entry
// ---------------------------------------------------------------------------

#[test]
fn g_qualified_write_produces_a_single_bare_entry() {
    // `_G.Foo = 1` must land on exactly one key (`Foo`). Storing both
    // `Foo` and `_G.Foo` duplicates the symbol, which surfaces as two
    // separate results in workspace-symbol search.
    let paths = global_shard_paths("_G.Foo = 1\n", "g_write_single.lua");
    assert_eq!(
        paths,
        vec!["Foo".to_string()],
        "`_G.Foo = 1` must produce exactly one bare `Foo` entry"
    );
}

#[test]
fn g_qualified_and_bare_writes_share_one_entry() {
    // Both spellings target the same global; the shard must not grow a
    // second key for the `_G.`-qualified one.
    let paths = global_shard_paths("_G.Shared = 1\nShared = 2\n", "g_write_merge.lua");
    assert_eq!(
        paths,
        vec!["Shared".to_string()],
        "`_G.Shared` and `Shared` must be the same shard entry"
    );
}

#[test]
fn nested_g_qualified_write_is_normalized() {
    // `_G` is itself reachable through `_G`, so `_G._G.X` is still `X`.
    let paths = global_shard_paths("_G._G.Deep = 1\n", "g_write_nested.lua");
    assert_eq!(
        paths,
        vec!["Deep".to_string()],
        "`_G._G.Deep` must normalize down to `Deep`"
    );
}

#[test]
fn g_qualified_table_extension_is_normalized() {
    // Multi-segment paths keep every segment after the `_G.` prefix.
    let paths = global_shard_paths("Tbl = {}\n_G.Tbl.field = 1\n", "g_write_ext.lua");
    assert_eq!(
        paths,
        vec!["Tbl".to_string(), "Tbl.field".to_string()],
        "`_G.Tbl.field` must normalize to `Tbl.field`"
    );
}

// ---------------------------------------------------------------------------
// Reads: `_G.X` resolves like bare `X`
// ---------------------------------------------------------------------------

#[test]
fn g_qualified_read_of_defined_global_is_not_flagged() {
    let src = r#"Defined = {}
print(_G.Defined)
"#;
    assert!(
        unknown_field_diags(src, "g_read_ok.lua").is_empty(),
        "`_G.Defined` must resolve like the bare global"
    );
}

#[test]
fn bare_read_of_field_on_defined_global_is_not_flagged() {
    // Control for the `_G.`-qualified variant below: establishes whether the
    // bare spelling already works, so a failure of the qualified one can be
    // attributed to the `_G` alias rather than to global table-extension
    // reads in general.
    let src = r#"Holder = {}
Holder.member = 1
print(Holder.member)
"#;
    assert!(
        unknown_field_diags(src, "bare_read_field.lua").is_empty(),
        "`Holder.member` (bare) must resolve"
    );
}

#[test]
fn g_qualified_read_of_field_on_defined_global_is_not_flagged() {
    let src = r#"Holder = {}
Holder.member = 1
print(_G.Holder.member)
"#;
    assert!(
        unknown_field_diags(src, "g_read_field.lua").is_empty(),
        "`_G.Holder.member` must resolve like `Holder.member`"
    );
}

// ---------------------------------------------------------------------------
// Shadowing: `local _G` makes `_G.X` an ordinary table field access
// ---------------------------------------------------------------------------

#[test]
fn shadowed_local_g_field_read_is_flagged() {
    // A local named `_G` shadows the global environment, so `_G.foo` is a
    // plain (empty) table field read and must NOT be excused by the
    // existence of a bare global `foo`.
    let src = r#"foo = 1
local _G = {}
print(_G.foo)
"#;
    let diags = unknown_field_diags(src, "g_shadow_read.lua");
    assert!(
        diags.iter().any(|m| m.contains("foo")),
        "a local `_G` shadows the global env: `_G.foo` must still be flagged \
         as an unknown field, got: {:?}",
        diags
    );
}

#[test]
fn shadowed_local_g_write_is_not_exported_as_a_global() {
    // `local _G = {}; _G.leaked = 1` writes to a local table. Nothing may
    // reach `global_shard` — least of all under the bare name `leaked`.
    let paths = global_shard_paths("local _G = {}\n_G.leaked = 1\n", "g_shadow_write.lua");
    assert!(
        paths.is_empty(),
        "writes through a shadowed local `_G` must not be exported as globals, got: {:?}",
        paths
    );
}

// ---------------------------------------------------------------------------
// Local aliases of globals (the `LuaPanda` regression) — and its negative
// ---------------------------------------------------------------------------

#[test]
fn local_alias_of_global_exports_function_decl_to_global_shard() {
    // `local this = LuaPanda` aliases a global table, so
    // `function this.f1()` writes onto the global table.
    let paths = global_shard_paths(
        "LuaPanda = {}\nlocal this = LuaPanda\nfunction this.f1() end\n",
        "g_alias_export.lua",
    );
    assert!(
        paths.contains(&"LuaPanda.f1".to_string()),
        "`function <alias-of-global>.f1()` must be exported as `LuaPanda.f1`, got: {:?}",
        paths
    );
}

#[test]
fn plain_local_table_function_decl_is_not_exported_to_global_shard() {
    // Negative control for the test above: a plain local table is NOT a
    // global alias, so its methods must stay out of `global_shard`.
    let paths = global_shard_paths(
        "local priv = {}\nfunction priv.hidden() end\n",
        "g_alias_negative.lua",
    );
    assert!(
        paths.is_empty(),
        "methods on a plain local table must not leak into global_shard, got: {:?}",
        paths
    );
}

// ---------------------------------------------------------------------------
// Completion: `_G.` offers the global namespace
// ---------------------------------------------------------------------------

#[test]
fn g_dot_completion_lists_bare_globals() {
    let src = r#"CompletionTarget = {}
function some_global_fn() end
print(_G.
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "g_completion.lua");
    // Cursor right after `_G.` on line 2 (0-based), col 9.
    let items = completion::complete(&doc, intern_uri(&uri), pos(2, 9), &mut agg);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"CompletionTarget"),
        "`_G.` completion must list bare globals, got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"some_global_fn"),
        "`_G.` completion must list global functions, got: {:?}",
        labels
    );
}
