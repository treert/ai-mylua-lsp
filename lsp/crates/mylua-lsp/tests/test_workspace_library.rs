//! Tests for `mylua.workspace.library` — external stdlib / stub
//! indexing alongside the user workspace. These exercise the
//! contract that `run_workspace_scan` guarantees:
//!
//! 1. Library roots contribute their Lua files to `global_shard` /
//!    `type_shard` / `require_map` (so `print`, `string.format`,
//!    `io.open`, etc. are known names with real `FunctionSummary`s).
//! 2. Library file summaries are flagged `is_meta = true` even when
//!    the source lacks an explicit `---@meta` header (stdlib stubs
//!    traditionally don't carry it).
//! 3. Require resolution for library-contributed modules
//!    (`require("string")`, `require("math")`) points at the library
//!    file URI.
//!
//! The diagnostic-suppression half of the feature — publishing empty
//! Diagnostics for library URIs — lives in `consumer_loop` and is
//! covered by `test_diagnostics.rs` where the summary-level
//! `is_meta` flag gates `undefinedGlobal` reporting at the semantic
//! stage. These tests focus on the **index contents**; the
//! full end-to-end publish path requires an LSP client which is out
//! of scope for the integration harness.

mod test_helpers;

use test_helpers::*;

/// Library roots contribute known globals (`print`, `assert`,
/// `string`, `table`, ...) into `global_shard`. A pure-library
/// workspace (no user files) is a degenerate but useful case — it
/// proves the library path is not predicated on the user having any
/// `.lua` of their own.
#[test]
fn library_contributes_stdlib_globals() {
    let lib = bundled_lua54_library_path();
    assert!(
        lib.is_dir(),
        "bundled lua 5.4 stubs missing at {}; \
         did you move assets out of vscode-extension/assets?",
        lib.display()
    );

    let (_docs, agg, _parser, library_uris) =
        setup_workspace_with_library(&[], &[lib.clone()]);

    // Every .lua in the lib directory should be counted as library.
    assert!(
        !library_uris.is_empty(),
        "library URIs should be populated from the bundled stub tree"
    );

    // `print` is declared in basic.lua as `function print(...) end`.
    // `build_summary` records this as a function_summary keyed on
    // the global name, and `upsert_summary` promotes it into
    // `global_shard`.
    assert!(
        agg.global_shard.contains_key("print"),
        "print must appear in global_shard after library scan; \
         global_shard keys = {:?}",
        agg.global_shard.iter_all_entries().into_iter().map(|(k, _)| k).take(20).collect::<Vec<_>>()
    );
}

/// `string`, `table`, `math`, `io`, `os` are all declared as
/// top-level `tab = {}` then extended with `function tab.field()
/// end` methods. Each should carry a table contribution in
/// `global_shard` so dotted-field hover (`string.format`) can resolve.
#[test]
fn library_stdlib_modules_present_in_global_shard() {
    let lib = bundled_lua54_library_path();
    let (_docs, agg, _parser, _library_uris) =
        setup_workspace_with_library(&[], &[lib]);

    for name in &["string", "table", "math", "io", "os"] {
        assert!(
            agg.global_shard.contains_key(*name),
            "stdlib table `{}` missing from global_shard",
            name
        );
    }
}

/// `require("string")` from a user file must resolve into the
/// library's `string.lua`. This proves `require_map` is populated
/// from library roots (not just user workspace roots).
#[test]
fn library_modules_are_requirable() {
    let lib = bundled_lua54_library_path();
    let user_file = (
        "main.lua",
        "local s = require(\"string\")\nlocal t = require(\"table\")\n",
    );

    let (_docs, agg, _parser, _library_uris) =
        setup_workspace_with_library(&[user_file], &[lib.clone()]);

    let string_uri = agg
        .resolve_module_to_uri("string")
        .expect("require(\"string\") should resolve to the library file");
    assert!(
        string_uri.to_string().ends_with("string.lua"),
        "resolved string module should end with string.lua, got {:?}",
        string_uri
    );

    let table_uri = agg
        .resolve_module_to_uri("table")
        .expect("require(\"table\") should resolve to the library file");
    assert!(
        table_uri.to_string().ends_with("table.lua"),
        "resolved table module should end with table.lua, got {:?}",
        table_uri
    );
}

/// Every library file's summary must carry `is_meta = true` even
/// though the bundled stubs lack an explicit `---@meta` header.
/// Without this, `undefinedGlobal` would fire inside the stub files
/// themselves on identifiers they happen to reference.
#[test]
fn library_files_are_forced_meta() {
    let lib = bundled_lua54_library_path();
    let (_docs, agg, _parser, library_uris) =
        setup_workspace_with_library(&[], &[lib]);

    assert!(
        !library_uris.is_empty(),
        "library URIs should be non-empty for this test to be meaningful"
    );

    for uri in &library_uris {
        let summary = agg
            .summaries
            .get(uri)
            .unwrap_or_else(|| panic!("library URI {:?} missing from summaries", uri));
        assert!(
            summary.is_meta,
            "library file {:?} must have is_meta=true",
            uri
        );
    }
}

/// User workspace files scanned alongside a library keep their
/// normal (is_meta = false unless annotated) treatment. This is a
/// regression guard: the blanket library flag must not leak onto
/// genuine workspace files that happen to share the aggregation
/// layer.
#[test]
fn user_files_remain_non_meta_when_library_is_configured() {
    let lib = bundled_lua54_library_path();
    let user_file = ("app.lua", "local x = 1\n");

    let (_docs, agg, _parser, library_uris) =
        setup_workspace_with_library(&[user_file], &[lib]);

    let user_uri = make_uri("app.lua");
    assert!(
        !library_uris.contains(&user_uri),
        "user URI must not be in library_uris set"
    );
    let s = agg
        .summaries
        .get(&user_uri)
        .expect("user file summary present");
    assert!(
        !s.is_meta,
        "plain user file must remain is_meta=false even with library configured"
    );
}
