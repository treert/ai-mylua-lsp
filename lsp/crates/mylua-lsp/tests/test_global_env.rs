//! `_G` / `_ENV` global-environment semantics.
//!
//! Two distinct invariants live here, and they are **not** symmetric:
//!
//! 1. **`_G.X` aliases the bare global `X`** — a *key-space* identity. `_G` is
//!    an ordinary field of the global table that happens to point back at it,
//!    so `_G.X`, `_G._G.X` and `X` name the same global. This is normalized
//!    once, at the single `global_shard` key entry point. It does **not** hold
//!    when a local named `_G` shadows it, nor after `_G` is reassigned.
//!
//! 2. **A free name `x` *is* `_ENV.x`** — a *scope-resolution* rule. `_ENV` is
//!    a lexical upvalue, not a field of the global table (`_G._ENV` is nil).
//!    With no user-declared `_ENV` in scope, free names resolve against the
//!    global environment (the ordinary case). Once `local _ENV = t`,
//!    `function f(_ENV)` or `_ENV = t` is in scope, free names read and write
//!    `t`'s fields instead, and must not reach `global_shard` at all.
//!
//! Both invariants used to be implemented as scattered `== "_G"` special
//! cases across the summary builder, the resolver and the diagnostics layer;
//! each new call site had to re-implement them, and one (the raw-text prefix
//! in `diagnostics/field_access.rs`) got the shadowed case wrong.

mod test_helpers;

use mylua_lsp::completion;
use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;
use mylua_lsp::semantic_tokens;
use mylua_lsp::uri_id::intern_uri;
use test_helpers::*;

/// All diagnostic messages for a single-file workspace, prefixed with the
/// 0-based line number (`"L6 Undefined global 'x'"`).
fn all_diags(src: &str, filename: &str) -> Vec<String> {
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
    .map(|d| format!("L{} {}", d.range.start.line, d.message))
    .collect()
}

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

/// Number of `global_shard` candidates registered at an exact path.
fn global_candidate_count(src: &str, filename: &str, path: &str) -> usize {
    let (_doc, _uri, agg) = setup_single_file(src, filename);
    agg.global_shard.get(path).map(|c| c.len()).unwrap_or(0)
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

// ---------------------------------------------------------------------------
// `_ENV` — a free name `x` IS `_ENV.x`
// ---------------------------------------------------------------------------

/// The hand-written repro from `tests/lua-root/test_env.lua`, inlined so the
/// Rust suite does not depend on that scratch directory.
///
/// Note `local print = print` on line 1: after `_ENV = {}` the new environment
/// has no `print`, so the real Lua program must capture it as a local first.
/// Any implementation that breaks local resolution after the `_ENV` write
/// would start flagging `print`.
const ENV_REPRO: &str = r#"local print = print

g1 = 123
print(g1)

_ENV = {}
print(g1)

g1 = 321
print(g1)

g2 = g1 + 1000
print(g2)
"#;

#[test]
fn env_assignment_is_not_registered_as_a_global() {
    // `_ENV = {}` assigns the *upvalue* that holds the current environment;
    // it does not define a global named `_ENV` (`_G._ENV` is nil in Lua).
    let paths = global_shard_paths("_ENV = {}\n", "env_assign.lua");
    assert!(
        !paths.contains(&"_ENV".to_string()),
        "`_ENV = {{}}` must not register a global named `_ENV`, got: {:?}",
        paths
    );
}

#[test]
fn free_name_before_env_assignment_is_a_real_global() {
    // Positional control: before the `_ENV` write the environment is still
    // the global one, so `g1 = 123` really is a global definition.
    let paths = global_shard_paths(ENV_REPRO, "env_repro_before.lua");
    assert!(
        paths.contains(&"g1".to_string()),
        "`g1 = 123` before the `_ENV` write must stay a real global, got: {:?}",
        paths
    );
}

#[test]
fn free_name_after_env_assignment_does_not_merge_into_the_global() {
    // The sharpest assertion in this file. `g1` on line 3 (real global) and
    // `g1` on line 9 (field of the new `_ENV` table) are different variables
    // at run time. If the line-9 write also lands in `global_shard`, the two
    // collapse into one symbol and goto/references wrongly link them.
    let count = global_candidate_count(ENV_REPRO, "env_repro_merge.lua", "g1");
    assert_eq!(
        count, 1,
        "`g1` must have exactly one global candidate (the pre-`_ENV` write); \
         the post-`_ENV` write belongs to the new environment table"
    );
}

#[test]
fn free_name_after_env_assignment_is_not_a_global() {
    // `g2` is only ever written after the `_ENV` write, so it must not exist
    // as a global at all.
    let paths = global_shard_paths(ENV_REPRO, "env_repro_g2.lua");
    assert!(
        !paths.contains(&"g2".to_string()),
        "`g2` is written only after the `_ENV` write and must not be a global, got: {:?}",
        paths
    );
}

#[test]
fn env_repro_reports_no_false_diagnostics() {
    // Whole-file check: nothing in the repro is undefined. In particular
    // `print` (captured as a local on line 1) must keep resolving after the
    // `_ENV` write, and the free names must not be reported as undefined
    // globals now that they are `_ENV` fields.
    let diags = all_diags(ENV_REPRO, "env_repro_diag.lua");
    assert!(
        diags.is_empty(),
        "the `_ENV` repro must produce no diagnostics, got: {:?}",
        diags
    );
}

#[test]
fn local_env_declaration_redirects_free_names() {
    // `local _ENV = {}` — the classic sandbox form.
    let paths = global_shard_paths(
        "local _ENV = {}\nsandboxed = 1\n",
        "env_local_decl.lua",
    );
    assert!(
        paths.is_empty(),
        "writes under `local _ENV` must not reach global_shard, got: {:?}",
        paths
    );
}

#[test]
fn env_parameter_redirects_free_names() {
    // A parameter named `_ENV` shadows the environment too — the idiomatic
    // Lua 5.2+ sandbox entry point.
    let paths = global_shard_paths(
        "local function sandbox(_ENV)\n    inside = 1\nend\n",
        "env_param.lua",
    );
    assert!(
        paths.is_empty(),
        "writes under a `_ENV` parameter must not reach global_shard, got: {:?}",
        paths
    );
}

#[test]
fn free_name_read_under_local_env_is_not_undefined_global() {
    // Reads of free names under a redirected `_ENV` are field reads on that
    // table, not global reads, so `undefinedGlobal` must stay silent.
    let src = "local _ENV = { allowed = 1 }\nprint(allowed)\n";
    let diags: Vec<String> = all_diags(src, "env_local_read.lua")
        .into_iter()
        .filter(|d| d.contains("Undefined global"))
        .collect();
    assert!(
        diags.is_empty(),
        "free-name reads under a redirected `_ENV` must not be flagged as \
         undefined globals, got: {:?}",
        diags
    );
}

#[test]
fn env_with_unknown_shape_reports_nothing() {
    // Chosen trade-off (a): when `_ENV` points at a value whose shape is
    // unknown, we know neither what it contains nor that a name is missing.
    // Stay silent rather than guess — and still keep the writes out of the
    // global index.
    //
    // `make_env` is declared as a real global so that the factory call on the
    // `_ENV` line is not itself an undefined global (it sits *before* the
    // `_ENV` declaration takes effect, so it resolves against the real global
    // environment — which is correct).
    let src = r#"function make_env() end
local _ENV = make_env()
whatever = 1
print(mystery)
"#;
    let diags = all_diags(src, "env_unknown.lua");
    assert!(
        diags.iter().all(|d| !d.contains("Undefined global")),
        "an `_ENV` of unknown shape must not produce undefined-global noise, got: {:?}",
        diags
    );
    let paths = global_shard_paths(src, "env_unknown2.lua");
    assert!(
        !paths.contains(&"whatever".to_string()),
        "writes under an unknown-shape `_ENV` must stay out of global_shard, got: {:?}",
        paths
    );
}

#[test]
fn free_names_outside_any_env_declaration_remain_globals() {
    // Negative control guarding against an inverted predicate: with no
    // `_ENV` in scope anywhere, ordinary globals must behave exactly as
    // before. Without this, a mistake that treats every file as sandboxed
    // would silently empty the whole workspace index.
    let paths = global_shard_paths(
        "plain_global = 1\nfunction plain_fn() end\n",
        "env_none.lua",
    );
    assert!(
        paths.contains(&"plain_global".to_string())
            && paths.contains(&"plain_fn".to_string()),
        "files without any `_ENV` declaration must keep registering globals, got: {:?}",
        paths
    );
}

#[test]
fn env_declaration_scope_ends_with_its_block() {
    // `local _ENV` inside a function must not leak out: after the function
    // body ends, free names are globals again.
    let src = r#"local function sandbox()
    local _ENV = {}
    inside = 1
end
outside = 2
"#;
    let paths = global_shard_paths(src, "env_scope_end.lua");
    assert!(
        !paths.contains(&"inside".to_string()),
        "`inside` is written under a sandboxed `_ENV`, got: {:?}",
        paths
    );
    assert!(
        paths.contains(&"outside".to_string()),
        "`outside` is written outside the sandbox and must stay a global, got: {:?}",
        paths
    );
}

// ---------------------------------------------------------------------------
// Bare `_ENV` is the environment upvalue, not a global variable
// ---------------------------------------------------------------------------

#[test]
fn bare_env_read_is_not_a_global_variable() {
    // `local xx = _ENV` reads the environment *upvalue*. It must not be
    // modelled as a global variable named `_ENV` — `_G._ENV` is nil in Lua,
    // so such a global does not exist and nothing may register it.
    let paths = global_shard_paths("local xx = _ENV\n", "bare_env_read.lua");
    assert!(
        !paths.contains(&"_ENV".to_string()),
        "reading bare `_ENV` must not create a global named `_ENV`, got: {:?}",
        paths
    );
}

#[test]
fn env_is_not_highlighted_as_a_global_variable() {
    // `_ENV` is a lexical upvalue, so semantic highlighting must treat it as a
    // local — not decorate it with the `global` modifier. Every chunk declares
    // `_ENV` implicitly (as if `local _ENV = _G`), which is what makes the
    // ordinary "is this name a local?" question answer correctly here without
    // the highlighter needing an `_ENV` branch of its own.
    let src = "local xx = _ENV\n";
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let tokens = semantic_tokens::collect_semantic_tokens(
        doc.root_node().unwrap(),
        src.as_bytes(),
        &doc.scope_tree,
        doc.line_index(),
    );
    // Legend: modifier bit 1 is "global" (bit 0 is "defaultLibrary").
    const TM_GLOBAL: u32 = 1 << 1;
    // Tokens are delta-encoded; `_ENV` is the last identifier on line 0.
    let env_token = tokens
        .last()
        .expect("at least one semantic token expected");
    assert_eq!(
        env_token.token_modifiers_bitset & TM_GLOBAL,
        0,
        "`_ENV` must not carry the `global` semantic-token modifier \
         (bitset was {:#b})",
        env_token.token_modifiers_bitset
    );
}

#[test]
fn explicit_local_env_bound_to_g_keeps_free_names_global() {
    // `local _ENV = _G` restates the default environment, so it must behave
    // exactly like the implicit case: free names stay ordinary globals. This
    // is why the redirect keys off *what `_ENV` points at* rather than merely
    // whether a declaration exists.
    let paths = global_shard_paths(
        "local _ENV = _G\nstill_global = 1\n",
        "env_bound_to_g.lua",
    );
    assert!(
        paths.contains(&"still_global".to_string()),
        "`local _ENV = _G` must keep free names as real globals, got: {:?}",
        paths
    );
}

#[test]
fn captured_env_resolves_field_reads_against_the_global_namespace() {
    // The implicit chunk-level `_ENV` *is* the global table, so a value
    // captured from it exposes the global namespace as its fields. Requires
    // the bundled stdlib, which declares `---@class _G`.
    let lib = bundled_lua54_library_path();
    let src = r#"CapturedTarget = 1
local env = _ENV
print(env.CapturedTarget)
"#;
    let (docs, mut agg, _parser, _library_uris) =
        setup_workspace_with_library(&[("env_captured.lua", src)], &[lib]);
    let uri = make_uri("env_captured.lua");
    let uri_id = intern_uri(&uri);
    let doc = docs.get(&uri_id).expect("user document present");
    let cfg = DiagnosticsConfig::default();
    let diags: Vec<String> = diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        uri_id,
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    )
    .into_iter()
    .map(|d| d.message)
    .collect();
    assert!(
        diags.iter().all(|m| !m.contains("CapturedTarget")),
        "`local env = _ENV; env.CapturedTarget` must resolve against the \
         global namespace, got: {:?}",
        diags
    );
}

#[test]
fn env_qualified_write_normalizes_to_the_bare_global() {
    // `_ENV.foo = 1` is just `foo = 1`, so it must land on the bare key
    // rather than a bogus `_ENV.foo` entry.
    let paths = global_shard_paths("_ENV.foo = 1\n", "env_dotted_write.lua");
    assert_eq!(
        paths,
        vec!["foo".to_string()],
        "`_ENV.foo = 1` must register the bare global `foo`"
    );
}

#[test]
fn env_qualified_read_of_defined_global_is_not_flagged() {
    let src = "EnvDefined = 1\nprint(_ENV.EnvDefined)\n";
    assert!(
        unknown_field_diags(src, "env_dotted_read.lua").is_empty(),
        "`_ENV.X` must resolve like the bare global `X`"
    );
}

#[test]
fn env_and_g_prefixes_mix_down_to_one_entry() {
    // `_ENV._G.X`, `_G.X` and bare `X` all name the same global.
    let paths = global_shard_paths(
        "_ENV._G.Mixed = 1\n_G.Mixed = 2\nMixed = 3\n",
        "env_g_mixed.lua",
    );
    assert_eq!(
        paths,
        vec!["Mixed".to_string()],
        "`_ENV._G.X`, `_G.X` and `X` must collapse to one entry"
    );
}

#[test]
fn g_qualified_env_is_not_normalized_away() {
    // ASYMMETRY GUARD. `_ENV` is a lexical upvalue, NOT a field of the global
    // table, so `_G._ENV` is nil and `_G._ENV.x` indexes nil at run time. It
    // must NOT collapse to the real global `x` — otherwise broken code would
    // silently resolve, and a genuine global could be masked.
    let paths = global_shard_paths("_G._ENV.x = 1\n", "g_env_no_collapse.lua");
    assert!(
        !paths.contains(&"x".to_string()),
        "`_G._ENV.x` must not normalize to the bare global `x`, got: {:?}",
        paths
    );
}

#[test]
fn doubled_env_is_not_normalized_away() {
    // Same asymmetry: `_ENV._ENV` is nil, so only the head `_ENV.` is
    // stripped and the result stays an unresolvable pseudo-key.
    let paths = global_shard_paths("_ENV._ENV.y = 1\n", "env_env_no_collapse.lua");
    assert!(
        !paths.contains(&"y".to_string()),
        "`_ENV._ENV.y` must not normalize to the bare global `y`, got: {:?}",
        paths
    );
}

#[test]
fn shadowed_local_env_field_read_is_flagged() {
    // Mirror of `shadowed_local_g_field_read_is_flagged`. A local `_ENV`
    // shadows the environment, so `_ENV.x` is a plain table field read and
    // must not be excused by the existence of a bare global `x`. Guards the
    // raw-text prefix fallback in `diagnostics::field_access`.
    let src = "realglobal = 1\nlocal _ENV = {}\nprint(_ENV.realglobal)\n";
    let diags = unknown_field_diags(src, "env_shadow_field.lua");
    assert!(
        diags.iter().any(|m| m.contains("realglobal")),
        "a local `_ENV` shadows the global env: `_ENV.realglobal` must still \
         be flagged, got: {:?}",
        diags
    );
}

#[test]
fn shadowed_local_env_qualified_write_is_not_a_global() {
    // Writes through a shadowed `_ENV` target the local table.
    let paths = global_shard_paths(
        "local _ENV = {}\n_ENV.leaked = 1\n",
        "env_shadow_write.lua",
    );
    assert!(
        paths.is_empty(),
        "writes through a shadowed local `_ENV` must not be exported as \
         globals, got: {:?}",
        paths
    );
}

// ---------------------------------------------------------------------------
// The bundled stdlib must survive `_ENV` redirection
// ---------------------------------------------------------------------------

/// Assignment-style globals declared by the bundled stdlib must reach the
/// global index.
///
/// Regression: `basic.lua` used to declare `_ENV = {}` to document the
/// environment table. By Lua semantics that statement rebinds the environment
/// to a fresh empty table, so once `_ENV` redirection was implemented, every
/// assignment-style global *below* that line (`_G` on line 83, `_VERSION` on
/// line 304) was recorded as a field of the throwaway table and disappeared
/// from `global_shard`. Function-style declarations (`function print() end`)
/// were unaffected, which is what made the breakage so selective — and why it
/// silently disabled every `_G.<field>` diagnostic instead of failing loudly.
#[test]
fn stdlib_assignment_style_globals_are_indexed() {
    let lib = bundled_lua54_library_path();
    let (_docs, agg, _parser, _library_uris) = setup_workspace_with_library(&[], &[lib]);
    // `_G` is deliberately absent from this list: it is a built-in concept in
    // the language server, not something a stub has to declare. See
    // `g_dot_undefined_field_is_flagged_with_stdlib_loaded`.
    for name in ["_VERSION", "arg", "string", "table", "math"] {
        assert!(
            agg.global_shard.contains_key(name),
            "stdlib global `{}` must be present in global_shard",
            name
        );
    }
}

#[test]
fn g_dot_undefined_field_is_flagged_with_stdlib_loaded() {
    // End-to-end guard for the regression above: `_G` must resolve to the
    // stdlib `---@class _G`, so reading an undefined member through it is
    // reported. This is the user-visible symptom that regressed.
    let lib = bundled_lua54_library_path();
    let src = "print(_G.definitely_not_defined)\n";
    let (docs, mut agg, _parser, _library_uris) =
        setup_workspace_with_library(&[("g_dot_undef.lua", src)], &[lib]);
    let uri = make_uri("g_dot_undef.lua");
    let uri_id = intern_uri(&uri);
    let doc = docs.get(&uri_id).expect("user document present");
    let cfg = DiagnosticsConfig::default();
    let diags: Vec<String> = diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        uri_id,
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    )
    .into_iter()
    .map(|d| d.message)
    .collect();
    assert!(
        diags.iter().any(|m| m.contains("definitely_not_defined")),
        "`_G.<undefined>` must be flagged when the stdlib is loaded, got: {:?}",
        diags
    );
}

#[test]
fn g_dot_env_is_flagged_with_stdlib_loaded() {
    // `_G._ENV` is nil in Lua — `_ENV` is an upvalue, not a field of the
    // global table. Reading it must be reported rather than silently resolving.
    let lib = bundled_lua54_library_path();
    let src = "local x = _G._ENV\n";
    let (docs, mut agg, _parser, _library_uris) =
        setup_workspace_with_library(&[("g_dot_env_diag.lua", src)], &[lib]);
    let uri = make_uri("g_dot_env_diag.lua");
    let uri_id = intern_uri(&uri);
    let doc = docs.get(&uri_id).expect("user document present");
    let cfg = DiagnosticsConfig::default();
    let diags: Vec<String> = diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        uri_id,
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    )
    .into_iter()
    .map(|d| d.message)
    .collect();
    assert!(
        diags.iter().any(|m| m.contains("_ENV")),
        "`_G._ENV` must be flagged — `_ENV` is not a field of the global \
         table, got: {:?}",
        diags
    );
}
