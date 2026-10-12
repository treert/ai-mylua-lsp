//! End-to-end coverage for `mylua.runtime.emmyCommentParsing`.
//!
//! This file is deliberately its own integration-test binary: the policy is
//! a process global (see `emmy::set_emmy_comment_parsing_legacy`), and the
//! legacy test below flips it. Running that in a shared binary would race
//! every other test, so both the strict and the legacy assertions live in
//! this single test function, executed sequentially.

mod test_helpers;

use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;
use mylua_lsp::emmy;
use test_helpers::*;

const BLOCK_PARAM_SRC: &str = r#"--[[
@param InX lala
]]
---@param InA number
---@param InB number
local function test_func_comment(InA, InB) end
"#;

fn block_param_mismatch_warnings() -> Vec<String> {
    let (doc, uri, mut agg) = setup_single_file(BLOCK_PARAM_SRC, "emmy_comment_parsing.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        BLOCK_PARAM_SRC.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    );
    diags
        .into_iter()
        .filter(|d| d.message.contains("does not match any Lua parameter"))
        .map(|d| d.message)
        .collect()
}

#[test]
fn emmy_comment_parsing_strict_vs_legacy() {
    // Default (and what initialize pins for "strict"): the block comment's
    // `@param InX` text is description-only — no warning.
    emmy::set_emmy_comment_parsing_legacy(false);
    let strict = block_param_mismatch_warnings();
    assert!(
        strict.is_empty(),
        "strict mode must not treat block comment text as annotations, got: {strict:?}"
    );

    // "legacy": historical behavior — the block comment's `@param InX`
    // parses and the name mismatch warns.
    emmy::set_emmy_comment_parsing_legacy(true);
    let legacy = block_param_mismatch_warnings();
    assert!(
        legacy.len() == 1 && legacy[0].contains("@param 'InX'"),
        "legacy mode must keep the historical block-comment annotation warning, got: {legacy:?}"
    );

    // Restore the default for any test added to this binary later.
    emmy::set_emmy_comment_parsing_legacy(false);
}
