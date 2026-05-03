//! Behavior of `---@meta` stub files (Lua-LS convention).

mod test_helpers;

use test_helpers::*;
use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;

#[test]
fn meta_file_is_detected_and_flagged() {
    let src = "---@meta\n\nfunction os_exit() end\n";
    let (_doc, uri, agg) = setup_single_file(src, "stub.lua");
    let summary = summary_by_uri(&agg, &uri).expect("summary present");
    assert!(summary.is_meta, "top-level @meta should be detected");
    assert!(summary.meta_name.is_none(), "no module name supplied");
}

#[test]
fn meta_file_with_module_name() {
    let src = "---@meta io\n\nfunction io.open(path) end\n";
    let (_doc, uri, agg) = setup_single_file(src, "io_stub.lua");
    let summary = summary_by_uri(&agg, &uri).expect("summary present");
    assert!(summary.is_meta);
    assert_eq!(summary.meta_name.as_deref(), Some("io"));
}

#[test]
fn meta_placed_after_code_is_not_treated_as_meta() {
    // `---@meta` placed AFTER runtime code is almost certainly an
    // authoring mistake; don't silently accept it.
    let src = "local x = 1\n---@meta\n";
    let (_doc, uri, agg) = setup_single_file(src, "late_meta.lua");
    let summary = summary_by_uri(&agg, &uri).expect("summary present");
    assert!(!summary.is_meta, "late `---@meta` must not flag the file as a stub");
}

#[test]
fn meta_file_suppresses_undefined_global_diagnostics() {
    // A stub file typically references runtime-provided APIs whose
    // declarations don't exist in the workspace. `---@meta` should
    // mute `undefinedGlobal` for those.
    let src = r#"---@meta

---@param path string
function io.open(path)
    return nil
end
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "io_stub.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(),
        src.as_bytes(),
        &uri,
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("Undefined global")),
        "meta files must not produce undefinedGlobal diagnostics, got: {:?}", diags,
    );
}

#[test]
fn non_meta_file_still_reports_undefined_globals() {
    // Counter-test: same code without `---@meta` should still flag
    // genuinely unknown globals. Use a clearly-fictional name that
    // is not part of any Lua standard library to avoid tripping the
    // builtins allowlist.
    let src = r#"
print(some_unknown_global)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "plain.lua");
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(),
        src.as_bytes(),
        &uri,
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    );
    assert!(
        diags.iter().any(|d| d.message.contains("some_unknown_global")),
        "non-meta file should still flag unknown globals, got: {:?}", diags,
    );
}

#[test]
fn meta_globals_still_participate_in_workspace_index() {
    // Stub file declares a non-builtin global; other files
    // referencing it must not see `undefinedGlobal`. This is the
    // practical payoff of the `---@meta` convention.
    let (_docs, mut agg, _parser) = setup_workspace(&[
        (
            "my_stub.lua",
            "---@meta mylib\n\nmylib_api = {}\nfunction mylib_api.open(path) end\n",
        ),
        (
            "user.lua",
            "local f = mylib_api.open(\"/tmp/foo\")\n",
        ),
    ]);
    let user_uri = make_uri("user.lua");
    let (doc, _, _) = setup_single_file(
        "local f = mylib_api.open(\"/tmp/foo\")\n",
        "user.lua",
    );
    let cfg = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics(
        doc.tree.root_node(),
        doc.source(),
        &user_uri,
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    );
    assert!(
        diags.iter().all(|d| !d.message.contains("mylib_api")),
        "meta-contributed global must suppress undefinedGlobal in consumers, got: {:?}", diags,
    );
}
