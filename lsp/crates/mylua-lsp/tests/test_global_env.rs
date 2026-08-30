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
use mylua_lsp::config::{
    DiagnosticSeverityOption, DiagnosticsConfig, GotoStrategy, ReferencesConfig, ReferencesStrategy,
};
use mylua_lsp::diagnostics;
use mylua_lsp::document::DocumentStoreView;
use mylua_lsp::semantic_tokens;
use mylua_lsp::type_system::{KnownType, TypeFact};
use mylua_lsp::uri_id::intern_uri;
use mylua_lsp::{goto, hover, references};
use std::collections::HashMap;
use test_helpers::*;

// ---------------------------------------------------------------------------
// Navigation helpers — goto / hover / references on a single-file workspace
// ---------------------------------------------------------------------------

/// 0-based lines that `goto_definition` at `at` resolves to.
fn goto_lines(src: &str, filename: &str, at: tower_lsp_server::ls_types::Position) -> Vec<u32> {
    use tower_lsp_server::ls_types::GotoDefinitionResponse;
    let (doc, uri, mut agg) = setup_single_file(src, filename);
    match goto::goto_definition(&doc, intern_uri(&uri), at, &mut agg, &GotoStrategy::Auto) {
        Some(GotoDefinitionResponse::Scalar(loc)) => vec![loc.range.start.line],
        Some(GotoDefinitionResponse::Array(locs)) => {
            locs.iter().map(|l| l.range.start.line).collect()
        }
        Some(GotoDefinitionResponse::Link(links)) => {
            links.iter().map(|l| l.target_range.start.line).collect()
        }
        None => Vec::new(),
    }
}

/// Rendered hover text at `at`, or `None` when hover produces nothing.
fn hover_text(
    src: &str,
    filename: &str,
    at: tower_lsp_server::ls_types::Position,
) -> Option<String> {
    use tower_lsp_server::ls_types::{HoverContents, MarkedString};
    let (doc, uri, mut agg) = setup_single_file(src, filename);
    let uri_id = intern_uri(&uri);
    let docs = HashMap::from([(uri_id, doc)]);
    let view = DocumentStoreView::new(&docs);
    let doc = docs.get(&uri_id).expect("doc present");
    hover::hover(doc, uri_id, at, &mut agg, &view).map(|h| match h.contents {
        HoverContents::Markup(md) => md.value,
        HoverContents::Scalar(MarkedString::String(s)) => s,
        HoverContents::Scalar(MarkedString::LanguageString(ls)) => ls.value,
        HoverContents::Array(items) => items
            .into_iter()
            .map(|m| match m {
                MarkedString::String(s) => s,
                MarkedString::LanguageString(ls) => ls.value,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// Sorted `(line, character)` pairs that `find_references` at `at` reports.
fn reference_sites(
    src: &str,
    filename: &str,
    at: tower_lsp_server::ls_types::Position,
) -> Vec<(u32, u32)> {
    let (doc, uri, agg) = setup_single_file(src, filename);
    let uri_id = intern_uri(&uri);
    let docs = HashMap::from([(uri_id, doc)]);
    let view = DocumentStoreView::new(&docs);
    let doc = docs.get(&uri_id).expect("doc present");
    let cfg = ReferencesConfig {
        strategy: ReferencesStrategy::Best,
        scan_comments: true,
    };
    let mut sites: Vec<(u32, u32)> =
        references::find_references(doc, uri_id, at, true, &agg, &view, &cfg)
            .unwrap_or_default()
            .into_iter()
            .map(|l| (l.range.start.line, l.range.start.character))
            .collect();
    sites.sort();
    sites.dedup();
    sites
}

/// All diagnostic messages for a single-file workspace under an explicit
/// config, prefixed with the 0-based line number (`"L6 Undefined global 'x'"`).
fn all_diags_with_config(src: &str, filename: &str, cfg: &DiagnosticsConfig) -> Vec<String> {
    let (doc, uri, mut agg) = setup_single_file(src, filename);
    diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        cfg,
        doc.line_index(),
    )
    .into_iter()
    .map(|d| format!("L{} {}", d.range.start.line, d.message))
    .collect()
}

/// All diagnostic messages for a single-file workspace, prefixed with the
/// 0-based line number (`"L6 Undefined global 'x'"`).
fn all_diags(src: &str, filename: &str) -> Vec<String> {
    all_diags_with_config(src, filename, &DiagnosticsConfig::default())
}

/// Only the `_ENV`-field diagnostics (both the "missing field" and the
/// "read before assigned" variants share the `current _ENV` wording).
fn env_field_diags(src: &str, filename: &str) -> Vec<String> {
    all_diags(src, filename)
        .into_iter()
        .filter(|d| d.contains("the current _ENV"))
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

/// Sorted field names recorded on the table `_ENV` points at, evaluated at the
/// very end of the file. `None` when `_ENV` does not resolve to a known table.
///
/// This inspects the index directly because that is exactly what the bug is
/// about: whether a write under a redirected `_ENV` lands anywhere at all. A
/// name that reaches neither `global_shard` nor the environment's shape has
/// simply vanished, and every downstream capability (goto, hover, references,
/// diagnostics) loses it.
fn env_shape_fields(src: &str, filename: &str) -> Option<Vec<String>> {
    let (doc, uri, agg) = setup_single_file(src, filename);
    let offset = src.len().saturating_sub(1);
    let fact = doc.scope_tree.resolve_type(offset, "_ENV")?;
    let TypeFact::Known(KnownType::Table(shape_id)) = fact else {
        return None;
    };
    let shape = agg
        .summary_by_id(intern_uri(&uri))?
        .table_shapes
        .get(shape_id)?;
    let mut names: Vec<String> = shape.fields.keys().map(|k| k.as_str().to_string()).collect();
    names.sort();
    Some(names)
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
fn env_repro_reports_exactly_the_premature_read() {
    // Whole-file check. Exactly one thing is wrong in the repro: the line-6
    // (0-based) read of `g1` happens while the new environment is still the
    // empty table — `g1` is only written into it on line 8. Everything else
    // must stay quiet; in particular `print` (captured as a local on line 0)
    // must keep resolving after the `_ENV` write, and the free names must not
    // be reported as undefined globals now that they are `_ENV` fields.
    let diags = all_diags(ENV_REPRO, "env_repro_diag.lua");
    assert_eq!(
        diags.len(),
        1,
        "the `_ENV` repro must produce exactly one diagnostic, got: {:?}",
        diags
    );
    assert!(
        diags[0].starts_with("L6 ") && diags[0].contains("'g1'"),
        "the single diagnostic must be about `g1` on line 6 (0-based), got: {:?}",
        diags
    );
    assert!(
        diags[0].contains("the current _ENV"),
        "the diagnostic must be the `_ENV`-field one, not `Undefined global`, got: {:?}",
        diags
    );
}

/// `ENV_REPRO` with the premature read on line 6 removed. Negative-space
/// guard: the *only* difference is that one line, so if this file also
/// reports something the implementation is over-firing rather than
/// pinpointing the real problem.
const ENV_REPRO_WITHOUT_PREMATURE_READ: &str = r#"local print = print

g1 = 123
print(g1)

_ENV = {}

g1 = 321
print(g1)

g2 = g1 + 1000
print(g2)
"#;

#[test]
fn env_repro_without_the_premature_read_reports_nothing() {
    let diags = all_diags(
        ENV_REPRO_WITHOUT_PREMATURE_READ,
        "env_repro_clean_diag.lua",
    );
    assert!(
        diags.is_empty(),
        "with the premature read removed the repro must be completely clean, got: {:?}",
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
    assert!(
        diags.iter().all(|d| !d.contains("the current _ENV")),
        "an `_ENV` of unknown shape must not produce env-field noise either — we \
         know neither what it contains nor that a name is missing, got: {:?}",
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

// ---------------------------------------------------------------------------
// `_ENV` field diagnostics — reading a field the redirected environment
// does not have (yet)
// ---------------------------------------------------------------------------
//
// Position sensitivity is only sound along the top-level straight-line
// execution flow of a chunk. A write inside a function body has no
// relationship to the read's byte position (the function can be called
// first), and a write inside a top-level branch is a flow-sensitivity
// problem we deliberately do not solve. Both therefore silence the check
// for that field rather than risk a false positive. The tests below pin
// each of those escape hatches down, plus a negative control against an
// inverted predicate.

#[test]
fn env_field_read_inside_a_function_body_is_not_flagged() {
    // The byte position of `print(gg)` is before `gg = 1`, but `f`'s call
    // time is unrelated to its definition site — calling `f` after the
    // write is perfectly normal. Crossing a function boundary must abandon
    // the positional judgement entirely.
    let src = r#"local print = print
_ENV = {}
local function f() print(gg) end
f()
gg = 1
"#;
    let diags = env_field_diags(src, "env_field_in_function.lua");
    assert!(
        diags.is_empty(),
        "a read inside a function body must not be judged positionally, got: {:?}",
        diags
    );
}

#[test]
fn env_field_assigned_in_a_top_level_branch_is_not_flagged() {
    // Whether the branch runs is a flow-sensitivity question. Treat the
    // field as defined (conservative) instead of guessing.
    let src = r#"local print = print
local cond = true
_ENV = {}
if cond then
    gg = 1
end
print(gg)
"#;
    let diags = env_field_diags(src, "env_field_branch.lua");
    assert!(
        diags.is_empty(),
        "a field written inside a top-level branch must be treated as \
         defined, got: {:?}",
        diags
    );
}

#[test]
fn env_literal_field_read_is_not_flagged() {
    // `local _ENV = { allowed = 1 }` — the field exists from the moment the
    // environment is constructed, so every later read is fine.
    let src = r#"local print = print
local _ENV = { allowed = 1 }
print(allowed)
"#;
    let diags = env_field_diags(src, "env_field_literal_ok.lua");
    assert!(
        diags.is_empty(),
        "`allowed` is present in the environment literal, got: {:?}",
        diags
    );
}

#[test]
fn env_field_missing_from_the_literal_is_flagged() {
    // Counterpart of the test above and the sharpest "missing field" case:
    // the environment's shape is fully known and closed, and `nope` is not
    // in it, so the read is `nil` at run time.
    let src = r#"local print = print
local _ENV = { allowed = 1 }
print(nope)
"#;
    let diags = env_field_diags(src, "env_field_literal_missing.lua");
    assert_eq!(
        diags.len(),
        1,
        "`nope` is not a field of the environment literal, got: {:?}",
        diags
    );
    assert!(
        diags[0].starts_with("L2 ") && diags[0].contains("'nope'"),
        "the diagnostic must point at `nope` on line 2 (0-based), got: {:?}",
        diags
    );
}

#[test]
fn builtin_names_are_reported_in_a_fully_known_env() {
    // Noise boundary, take two. After `_ENV = {}` the stdlib genuinely is
    // unreachable by its bare name — that is exactly why `ENV_REPRO` has to
    // open with `local print = print`. When the environment's shape is fully
    // known there is nothing speculative about saying so, so built-ins get no
    // exemption. The sandbox idiom that *would* make them reachable
    // (`setmetatable({}, { __index = _G })`) is handled by shape certainty
    // instead — see `setmetatable_env_reports_nothing`.
    let src = r#"_ENV = {}
local s = string
local p = print
"#;
    let diags = env_field_diags(src, "env_field_builtins.lua");
    assert_eq!(
        diags.len(),
        2,
        "in a fully known environment built-ins are missing fields like any \
         other name, got: {:?}",
        diags
    );
    assert!(
        diags.iter().any(|d| d.contains("'string'"))
            && diags.iter().any(|d| d.contains("'print'")),
        "both built-in reads must be reported, got: {:?}",
        diags
    );
}

#[test]
fn bare_g_is_reported_in_a_fully_known_env() {
    // `_G` must not be treated as the global environment table
    // unconditionally: after `_ENV = {}` the *name* `_G` resolves through the
    // new environment, which does not have it, so `_G` is nil here. The
    // built-in `_G` recognition exists to keep `_G.X` independent of stdlib
    // stub contents — not to override `_ENV` redirection.
    let src = r#"local print = print
_ENV = {}
print(_G)
"#;
    let diags = env_field_diags(src, "env_field_bare_g.lua");
    assert_eq!(
        diags.len(),
        1,
        "bare `_G` under a fully known redirected environment must be \
         reported, got: {:?}",
        diags
    );
    assert!(
        diags[0].starts_with("L2 ") && diags[0].contains("'_G'"),
        "the diagnostic must point at `_G` on line 2 (0-based), got: {:?}",
        diags
    );
}

#[test]
fn g_qualified_read_under_redirected_env_does_not_reach_the_old_global() {
    // The user-visible bug this pins down: `_G.g1` used to resolve straight to
    // the pre-redirection global `g1`, so goto/hover jumped to a symbol that is
    // unreachable at run time, and the field check reported the misleading
    // "unknown field on type '_G'" (the real problem is that `_G` itself is
    // nil). Now the base `_G` is what gets reported, and the field read stays
    // silent because nothing is known about `nil.g1`.
    let src = r#"local print = print
g1 = 123
_ENV = {}
print(_G.g1)
"#;
    let diags = all_diags(src, "env_field_g_dotted.lua");
    assert!(
        diags.iter().any(|d| d.contains("'_G'") && d.contains("the current _ENV")),
        "the base `_G` must be reported as missing from the current _ENV, got: {:?}",
        diags
    );
    assert!(
        diags.iter().all(|d| !d.contains("on type '_G'")),
        "`_G` is nil here, so no diagnostic may claim to know its fields, got: {:?}",
        diags
    );
}

#[test]
fn setmetatable_env_reports_nothing() {
    // REGRESSION GUARD, and the reason built-ins need no exemption.
    //
    // `setmetatable({}, { __index = _G })` is *the* idiomatic sandbox: every
    // name missing from the table falls through to the real global
    // environment, so `print`, `_G` and even a plain global really are
    // reachable. We do not follow `__index`, so the only sound answer is
    // silence — the table's field set is not an exhaustive description of the
    // environment.
    //
    // Today this also happens to fall out of `setmetatable`'s return type not
    // resolving to a table (its `---@generic T ... @return T` is not
    // back-filled from the call site). That is *not* what this test relies on:
    // if generic back-filling is ever implemented, `_ENV` would resolve to the
    // `{}` literal's shape and every name here would light up. Attaching a
    // metatable must keep marking the shape as non-exhaustive on its own.
    let src = r#"_ENV = setmetatable({}, { __index = _G })
local a = print
local b = _G
local c = whatever
"#;
    let diags = env_field_diags(src, "env_field_setmetatable.lua");
    assert!(
        diags.is_empty(),
        "an environment with an `__index` metatable must silence the check, got: {:?}",
        diags
    );
}

#[test]
fn setmetatable_applied_after_construction_reports_nothing() {
    // Same as above, but the metatable is attached in a separate statement, so
    // `_ENV`'s fact *is* the literal's `Known(Table)` shape. Shape certainty
    // alone therefore says "fully known" here and would report everything —
    // only tracking the `setmetatable` call itself gets this right.
    let src = r#"local t = {}
setmetatable(t, { __index = _G })
_ENV = t
local a = print
local c = whatever
"#;
    let diags = env_field_diags(src, "env_field_setmetatable_late.lua");
    assert!(
        diags.is_empty(),
        "a metatable attached in a separate statement must silence the check \
         just the same, got: {:?}",
        diags
    );
}

#[test]
fn rawset_on_the_env_table_reports_nothing() {
    // `rawset` writes a field we do not record on the shape, so the shape
    // stops being exhaustive.
    let src = r#"local print = print
_ENV = {}
rawset(_ENV, "injected", 1)
print(injected)
"#;
    let diags = env_field_diags(src, "env_field_rawset.lua");
    assert!(
        diags.is_empty(),
        "a `rawset` on the environment table must silence the check, got: {:?}",
        diags
    );
}

// ---------------------------------------------------------------------------
// Writes must not leak into the global index once `_ENV` is redirected
// ---------------------------------------------------------------------------
//
// The bare-name write path (`x = 1`) has been gated on `_ENV` redirection from
// the start. The *dotted* write path had not been, so `Foo.bar = 1` inside a
// sandbox was still exported as the global `Foo.bar` — polluting the whole
// workspace index with symbols that do not exist at run time. `_G.x = 1` was
// the same bug wearing a `_G` hat: `GlobalShard` normalizes the `_G.` prefix
// away, so it landed on the bare key `x`.

#[test]
fn g_qualified_write_under_redirected_env_is_not_a_global() {
    let paths = global_shard_paths("_ENV = {}\n_G.leaked = 1\n", "env_g_write_leak.lua");
    assert!(
        paths.is_empty(),
        "`_G.leaked = 1` under a redirected `_ENV` targets a field of `nil`, \
         nothing may reach global_shard, got: {:?}",
        paths
    );
}

#[test]
fn dotted_write_under_redirected_env_is_not_a_global() {
    // Not a `_G` problem at all — any dotted write leaked.
    let paths = global_shard_paths("_ENV = {}\nFoo.bar = 1\n", "env_dotted_write_leak.lua");
    assert!(
        paths.is_empty(),
        "`Foo.bar = 1` under a redirected `_ENV` must not reach global_shard, got: {:?}",
        paths
    );
}

#[test]
fn g_qualified_function_declaration_under_redirected_env_is_not_a_global() {
    let paths = global_shard_paths(
        "_ENV = {}\nfunction _G.f() end\n",
        "env_g_func_leak.lua",
    );
    assert!(
        paths.is_empty(),
        "`function _G.f()` under a redirected `_ENV` must not reach \
         global_shard, got: {:?}",
        paths
    );
}

#[test]
fn dotted_function_declaration_under_redirected_env_is_not_a_global() {
    let paths = global_shard_paths(
        "_ENV = {}\nfunction Foo.f() end\n",
        "env_dotted_func_leak.lua",
    );
    assert!(
        paths.is_empty(),
        "`function Foo.f()` under a redirected `_ENV` must not reach \
         global_shard, got: {:?}",
        paths
    );
}

#[test]
fn g_keeps_working_when_env_is_bound_back_to_g() {
    // SYMMETRY GUARD for the read-side fix. Moving the built-in `_G`
    // recognition *after* the redirection check must not disturb the case
    // where `_ENV` still is the global environment — whether implicitly or via
    // an explicit `local _ENV = _G`.
    let src = r#"local _ENV = _G
GBound = 1
print(_G.GBound)
"#;
    let paths = global_shard_paths(src, "env_g_symmetry_paths.lua");
    assert!(
        paths.contains(&"GBound".to_string()),
        "`local _ENV = _G` keeps free names global, so `GBound` must be \
         registered, got: {:?}",
        paths
    );
    let diags = all_diags(src, "env_g_symmetry_diags.lua");
    assert!(
        diags.iter().all(|d| !d.contains("the current _ENV")),
        "`_G` still denotes the global environment here, got: {:?}",
        diags
    );
}

#[test]
fn g_is_not_flagged_without_env_redirection() {
    // Negative control: with no redirection anywhere, `_G` must keep its
    // built-in meaning and produce no env-field noise.
    let src = "GPlain = 1\nprint(_G.GPlain)\nprint(_G)\n";
    let diags = env_field_diags(src, "env_g_no_redirect.lua");
    assert!(
        diags.is_empty(),
        "`_G` outside any redirected environment must not be flagged, got: {:?}",
        diags
    );
}

// ---------------------------------------------------------------------------
// A sandboxed `function foo() end` writes into the new environment
// ---------------------------------------------------------------------------
//
// Keeping a write out of `global_shard` is only half the job. `function foo()`
// under a redirected `_ENV` is perfectly ordinary sandbox code — it puts a
// function into the new environment — so it has to land on that table's shape,
// exactly like the assignment spelling `foo = function() end` already does.
// Gating it without redirecting it made the name vanish from the index
// altogether, which is worse than the leak it replaced: goto, hover and
// references all lose the symbol.

#[test]
fn assignment_style_function_lands_on_the_env_shape() {
    // Control for the test below — establishes that the assignment spelling
    // already works, so a failure of the declaration spelling is attributable
    // to the declaration path rather than to sandboxed writes in general.
    let src = "_ENV = {}\nbar = function() end\nlocal probe = 1\n";
    assert_eq!(
        env_shape_fields(src, "env_fn_assign_shape.lua"),
        Some(vec!["bar".to_string()]),
        "`bar = function() end` must land on the environment's shape"
    );
}

#[test]
fn declaration_style_function_lands_on_the_env_shape() {
    let src = "_ENV = {}\nfunction foo() end\nlocal probe = 1\n";
    assert_eq!(
        env_shape_fields(src, "env_fn_decl_shape.lua"),
        Some(vec!["foo".to_string()]),
        "`function foo() end` under a redirected `_ENV` must land on the \
         environment's shape, not vanish"
    );
}

#[test]
fn declaration_style_function_lands_on_a_local_env_shape() {
    // Same for the `local _ENV` sandbox form.
    let src = "local _ENV = {}\nfunction foo() end\nlocal probe = 1\n";
    assert_eq!(
        env_shape_fields(src, "env_fn_decl_local_shape.lua"),
        Some(vec!["foo".to_string()]),
        "`function foo() end` under `local _ENV = {{}}` must land on the \
         environment's shape"
    );
}

#[test]
fn sandboxed_function_declaration_is_still_not_a_global() {
    // The redirect must not reintroduce the leak it replaced.
    let paths = global_shard_paths(
        "_ENV = {}\nfunction foo() end\n",
        "env_fn_decl_not_global.lua",
    );
    assert!(
        paths.is_empty(),
        "`function foo() end` under a redirected `_ENV` must stay out of \
         global_shard, got: {:?}",
        paths
    );
}

#[test]
fn goto_resolves_a_sandboxed_function_declaration() {
    // The user-visible payoff: the symbol is reachable again.
    let src = r#"local print = print
_ENV = {}
function foo() end
print(foo)
"#;
    // `foo` inside `print(foo)` on line 3: p=0 … ( =5, f=6
    let lines = goto_lines(src, "env_fn_decl_goto.lua", pos(3, 6));
    assert!(
        lines.contains(&2),
        "goto on a sandboxed `foo` must reach its declaration on line 2 \
         (0-based), got lines: {:?}",
        lines
    );
}

#[test]
fn sandboxed_dotted_function_declaration_writes_nothing() {
    // Contrast with the bare form: `function Foo.f()` needs `Foo` to already
    // exist, and the sandbox does not provide it, so at run time this indexes
    // nil. Nothing may be recorded anywhere — neither a global nor an
    // environment field named `Foo`.
    let src = "_ENV = {}\nfunction Foo.f() end\nlocal probe = 1\n";
    let paths = global_shard_paths(src, "env_fn_dotted_nothing.lua");
    assert!(
        paths.is_empty(),
        "`function Foo.f()` in a sandbox must not reach global_shard, got: {:?}",
        paths
    );
    assert_eq!(
        env_shape_fields(src, "env_fn_dotted_shape.lua"),
        Some(Vec::new()),
        "`function Foo.f()` must not invent a `Foo` field on the environment"
    );
}

#[test]
fn function_declarations_outside_a_sandbox_are_unaffected() {
    // Negative control for the redirect: without `_ENV` redirection the
    // declaration must still register as an ordinary global.
    let paths = global_shard_paths(
        "function plain_decl() end\nfunction Holder.method() end\n",
        "env_fn_decl_control.lua",
    );
    assert!(
        paths.contains(&"plain_decl".to_string()),
        "`function plain_decl()` must still be a global, got: {:?}",
        paths
    );
}

// ---------------------------------------------------------------------------
// §1.6 name resolution is shared: goto / hover / references must agree
// ---------------------------------------------------------------------------
//
// §1.6 declares one resolution order shared by goto, hover and references.
// That used to be aspirational — each capability re-implemented the order
// inline, so a rule added to one silently failed in the others. `_ENV`
// redirection was exactly that: goto learned about it, hover returned nothing,
// and references fell back to whole-file text matching that could not tell the
// pre- and post-redirect `g` apart.
//
// These tests pin the *shared* behavior. A regression in the common layer
// breaks several at once, which is the point.

/// Two environments, one name. `g` before the `_ENV` write and `g` after it are
/// different variables at run time, so every navigation capability must keep
/// them apart.
const ENV_BOUNDARY: &str = r#"g = 1
print(g)
_ENV = {}
g = 2
print(g)
"#;

#[test]
fn hover_resolves_a_sandboxed_function_declaration() {
    let src = r#"local print = print
_ENV = {}
function sandboxed_fn() end
print(sandboxed_fn)
"#;
    let text = hover_text(src, "env_hover_fn.lua", pos(3, 6));
    let text = text.expect("hover on a sandboxed function must produce content");
    assert!(
        text.contains("sandboxed_fn"),
        "hover must name the symbol, got: {:?}",
        text
    );
}

#[test]
fn hover_resolves_a_sandboxed_variable() {
    let src = r#"local print = print
_ENV = {}
sbx_var = 123
print(sbx_var)
"#;
    let text = hover_text(src, "env_hover_var.lua", pos(3, 6));
    let text = text.expect("hover on a sandboxed variable must produce content");
    assert!(
        text.contains("sbx_var"),
        "hover must name the symbol, got: {:?}",
        text
    );
}

#[test]
fn hover_stays_silent_for_an_unknown_shape_env() {
    // The silence contract survives the shared layer: nothing is known about
    // the environment, so nothing may be claimed about its fields.
    let src = r#"function make_env() end
local _ENV = make_env()
local v = mystery
"#;
    assert!(
        hover_text(src, "env_hover_unknown.lua", pos(2, 10)).is_none(),
        "an `_ENV` of unknown shape must not produce hover content"
    );
}

#[test]
fn goto_keeps_the_two_environments_apart() {
    // Control for the references test below, and a guard on the claim in §1.3.
    let pre = goto_lines(ENV_BOUNDARY, "env_boundary_goto_pre.lua", pos(1, 6));
    assert!(
        pre.contains(&0) && !pre.contains(&3),
        "the pre-redirect read must resolve to the pre-redirect write (line 0) \
         only, got: {:?}",
        pre
    );
    let post = goto_lines(ENV_BOUNDARY, "env_boundary_goto_post.lua", pos(4, 6));
    assert!(
        post.contains(&3) && !post.contains(&0),
        "the post-redirect read must resolve to the post-redirect write \
         (line 3) only, got: {:?}",
        post
    );
}

#[test]
fn references_keep_the_two_environments_apart() {
    // §1.3 has always claimed goto/references do not link the two `g`s.
    // references did not honor it: `Identity::Global` scans the whole file by
    // text, so clicking either `g` returned all four sites.
    let pre = reference_sites(ENV_BOUNDARY, "env_boundary_refs_pre.lua", pos(1, 6));
    assert_eq!(
        pre,
        vec![(0, 0), (1, 6)],
        "clicking the pre-redirect `g` must report only the pre-redirect sites"
    );
    let post = reference_sites(ENV_BOUNDARY, "env_boundary_refs_post.lua", pos(4, 6));
    assert_eq!(
        post,
        vec![(3, 0), (4, 6)],
        "clicking the post-redirect `g` must report only the post-redirect sites"
    );
}

#[test]
fn references_find_every_use_of_a_sandboxed_name() {
    // Positive counterpart: within one environment all uses must still be
    // found — the boundary test above must not be satisfiable by simply
    // reporting nothing.
    let src = r#"local print = print
_ENV = {}
sbx = 1
print(sbx)
sbx = 2
print(sbx)
"#;
    let sites = reference_sites(src, "env_refs_all_uses.lua", pos(3, 6));
    assert_eq!(
        sites,
        vec![(2, 0), (3, 6), (4, 0), (5, 6)],
        "all four uses of the sandboxed `sbx` must be reported"
    );
}

#[test]
fn ordinary_globals_keep_their_reference_behavior() {
    // Negative control on the shared layer: without redirection, references on
    // a plain global must behave exactly as before.
    let src = "plain = 1\nprint(plain)\nplain = 2\n";
    let sites = reference_sites(src, "env_refs_plain_global.lua", pos(1, 6));
    assert!(
        sites.contains(&(0, 0)) && sites.contains(&(1, 6)) && sites.contains(&(2, 0)),
        "an ordinary global must still report all its sites, got: {:?}",
        sites
    );
}

#[test]
fn navigation_agrees_on_a_sandboxed_name() {
    // The consistency assertion §1.6 is really about: one cursor position, one
    // answer. Whatever the shared layer decides, all three capabilities act on
    // the same decision.
    let src = r#"local print = print
_ENV = {}
shared_name = 1
print(shared_name)
"#;
    let at = pos(3, 6);
    assert_eq!(
        goto_lines(src, "env_agree_goto.lua", at),
        vec![2],
        "goto must reach the write on line 2"
    );
    assert!(
        hover_text(src, "env_agree_hover.lua", at)
            .is_some_and(|t| t.contains("shared_name")),
        "hover must resolve the same symbol"
    );
    assert_eq!(
        reference_sites(src, "env_agree_refs.lua", at),
        vec![(2, 0), (3, 6)],
        "references must report exactly the two sites of that symbol"
    );
}

#[test]
fn env_field_defined_by_a_top_level_function_declaration_is_not_flagged() {
    // `function foo() end` under a redirected `_ENV` writes `foo` into the
    // new environment. The write is not recorded on the environment's table
    // shape, so a shape-only check would report a false positive here.
    let src = r#"local print = print
_ENV = {}
function foo() end
print(foo)
"#;
    let diags = env_field_diags(src, "env_field_func_decl.lua");
    assert!(
        diags.is_empty(),
        "`foo` is defined by a top-level function declaration before the \
         read, got: {:?}",
        diags
    );
}

#[test]
fn env_field_read_before_its_function_declaration_is_flagged() {
    // Same construct, reversed order — `foo` really is `nil` at the read.
    let src = r#"local print = print
_ENV = {}
print(foo)
function foo() end
"#;
    let diags = env_field_diags(src, "env_field_func_decl_early.lua");
    assert_eq!(
        diags.len(),
        1,
        "reading `foo` before its declaration must be flagged, got: {:?}",
        diags
    );
    assert!(
        diags[0].starts_with("L2 ") && diags[0].contains("'foo'"),
        "the diagnostic must point at `foo` on line 2 (0-based), got: {:?}",
        diags
    );
}

#[test]
fn dynamic_env_write_silences_env_field_diagnostics() {
    // `_ENV[k] = v` adds a field we cannot name statically, so the shape is
    // no longer an exhaustive description of the environment. Anything could
    // be in there — stay silent.
    let src = r#"local print = print
local k = "dyn"
_ENV = {}
_ENV[k] = 1
print(anything)
"#;
    let diags = env_field_diags(src, "env_field_dynamic.lua");
    assert!(
        diags.is_empty(),
        "a dynamic-key write through `_ENV` must silence the check, got: {:?}",
        diags
    );
}

#[test]
fn files_without_env_redirection_get_no_env_field_diagnostics() {
    // Negative control guarding against an inverted predicate. With no
    // `_ENV` redirection anywhere, this check must never fire — a mistake
    // that treats every file as sandboxed would otherwise flood the whole
    // workspace with env-field diagnostics.
    let src = r#"plain = 1
print(plain)
print(not_defined_anywhere)
"#;
    let diags = env_field_diags(src, "env_field_none.lua");
    assert!(
        diags.is_empty(),
        "files without `_ENV` redirection must produce no env-field \
         diagnostics (`not_defined_anywhere` is an undefined *global*), got: {:?}",
        diags
    );
    // ... and the ordinary global check must still be doing its job, so the
    // test above cannot pass merely because diagnostics are off.
    let all = all_diags(src, "env_field_none_control.lua");
    assert!(
        all.iter().any(|d| d.contains("Undefined global")),
        "the undefined-global check must still fire here, got: {:?}",
        all
    );
}

#[test]
fn env_field_diagnostic_can_be_switched_off() {
    let cfg = DiagnosticsConfig {
        env_unknown_field: DiagnosticSeverityOption::Off,
        ..Default::default()
    };
    let diags = all_diags_with_config(ENV_REPRO, "env_field_off.lua", &cfg);
    assert!(
        diags.is_empty(),
        "`diagnostics.envUnknownField = off` must silence the check, got: {:?}",
        diags
    );
}

#[test]
fn env_field_diagnostic_defaults_to_warning() {
    let cfg = DiagnosticsConfig::default();
    assert_eq!(cfg.env_unknown_field, DiagnosticSeverityOption::Warning);
}
