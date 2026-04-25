mod test_helpers;

use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;
use test_helpers::*;

fn run_diagnostics(src: &str, version: &str) -> Vec<tower_lsp_server::ls_types::Diagnostic> {
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    diagnostics::collect_semantic_diagnostics_with_version(
        doc.tree.root_node(),
        src.as_bytes(),
        &uri,
        &mut agg,
        &doc.scope_tree,
        &DiagnosticsConfig::default(),
        version,
        doc.line_index(),
    )
}

fn has_undefined(diags: &[tower_lsp_server::ls_types::Diagnostic], name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    diags.iter().any(|d| {
        let msg_lower = d.message.to_ascii_lowercase();
        msg_lower.contains("undefined")
            && (msg_lower.contains(&format!("'{}'", lower))
                || msg_lower.contains(&format!("`{}`", lower))
                || msg_lower.contains(&format!(" {}", lower)))
    })
}

#[test]
fn runtime_53_treats_utf8_as_builtin() {
    let src = "print(utf8.char(65))\n";
    let diags = run_diagnostics(src, "5.3");
    assert!(!has_undefined(&diags, "utf8"), "utf8 is a Lua 5.3+ builtin, got diags: {:?}", diags);
}

#[test]
fn runtime_51_does_not_treat_utf8_as_builtin() {
    let src = "print(utf8.char(65))\n";
    let diags = run_diagnostics(src, "5.1");
    assert!(
        has_undefined(&diags, "utf8"),
        "utf8 should be undefined under 5.1, got diags: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn runtime_52_treats_bit32_as_builtin() {
    let src = "print(bit32.band(1, 2))\n";
    let diags_52 = run_diagnostics(src, "5.2");
    assert!(!has_undefined(&diags_52, "bit32"), "bit32 is a 5.2 builtin");

    let diags_53 = run_diagnostics(src, "5.3");
    assert!(
        has_undefined(&diags_53, "bit32"),
        "bit32 is removed in 5.3 — should be undefined, got: {:?}",
        diags_53.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn runtime_luajit_treats_jit_and_bit_as_builtins() {
    let src = "print(bit.band(1, 2)) print(jit.version)\n";
    let diags = run_diagnostics(src, "luajit");
    assert!(!has_undefined(&diags, "bit"));
    assert!(!has_undefined(&diags, "jit"));
}

#[test]
fn runtime_51_treats_unpack_as_builtin() {
    let src = "local a, b = unpack({1, 2})\n";
    let diags_51 = run_diagnostics(src, "5.1");
    assert!(!has_undefined(&diags_51, "unpack"), "unpack is a 5.1 builtin");

    let diags_53 = run_diagnostics(src, "5.3");
    assert!(
        has_undefined(&diags_53, "unpack"),
        "unpack is removed in 5.3 (moved to table.unpack), got: {:?}",
        diags_53.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}
