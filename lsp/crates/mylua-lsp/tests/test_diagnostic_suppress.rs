//! End-to-end behavior of `---@diagnostic disable-*` / `enable`
//! directives, operating on the combined syntax+semantic diagnostic
//! list that `apply_diagnostic_suppressions` receives from lib.rs.

mod test_helpers;

use mylua_lsp::config::{DiagnosticSeverityOption, DiagnosticsConfig};
use mylua_lsp::diagnostics;
use test_helpers::*;
use tower_lsp_server::ls_types::NumberOrString;

/// Convenience: run both syntax + semantic passes then apply the
/// suppression post-process. Mirrors what `publish_diagnostics` does
/// in `lib.rs`.
fn collect_all(
    src: &str,
    name: &str,
    cfg: DiagnosticsConfig,
) -> Vec<tower_lsp_server::ls_types::Diagnostic> {
    let (doc, uri, mut agg) = setup_single_file(src, name);
    let mut all =
        diagnostics::collect_diagnostics(doc.tree.root_node(), src.as_bytes(), doc.line_index());
    let semantic = diagnostics::collect_semantic_diagnostics_id(
        doc.tree.root_node(),
        src.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        &cfg,
        doc.line_index(),
    );
    all.extend(semantic);
    diagnostics::apply_diagnostic_suppressions(doc.tree.root_node(), src.as_bytes(), all)
}

#[test]
fn disable_next_line_all_codes() {
    // `use_undefined` would normally trigger `undefined-global`.
    let src = r#"
---@diagnostic disable-next-line
print(use_undefined)
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "disable_next_all.lua", cfg);
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("Undefined global")),
        "disable-next-line should suppress all diagnostics on next line, got: {:?}",
        diags,
    );
}

#[test]
fn disable_next_line_specific_code_only_suppresses_matching() {
    // Suppress only `unused-local` — but we have `undefined-global`
    // on the target line, which must still fire.
    let src = r#"
---@diagnostic disable-next-line: unused-local
print(use_undefined)
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "disable_next_specific.lua", cfg);
    assert!(
        diags.iter().any(|d| d.message.contains("Undefined global")),
        "non-matching code should not be suppressed, got: {:?}",
        diags,
    );
}

#[test]
fn disable_next_line_param_annotation() {
    let src = r#"
---@diagnostic disable-next-line: param-annotation
---@param x number
local function f(a) return a end
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "disable_next_param_annotation.lua", cfg);
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("does not match any Lua parameter")),
        "param-annotation should be suppressible, got: {:?}",
        diags,
    );
}

#[test]
fn disable_line_same_line() {
    let src = r#"
print(use_undefined) ---@diagnostic disable-line
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "disable_line.lua", cfg);
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("Undefined global")),
        "disable-line on the same line must suppress, got: {:?}",
        diags,
    );
}

#[test]
fn disable_enable_pair_scopes_region() {
    let src = r#"
---@diagnostic disable: undefined-global
print(first_undef)
print(second_undef)
---@diagnostic enable: undefined-global
print(third_undef)
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "disable_enable.lua", cfg);
    let undef: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("Undefined global"))
        .collect();
    assert_eq!(
        undef.len(),
        1,
        "only `third_undef` (after enable) should fire; got: {:?}",
        diags,
    );
    assert!(
        undef[0].message.contains("third_undef"),
        "reported undefined should be 'third_undef', got: {}",
        undef[0].message,
    );
}

#[test]
fn disable_star_suppresses_everything_until_enable() {
    let src = r#"
---@diagnostic disable: *
print(uuu)
local function f(a, b) return a end
f(1, 2, 3)
---@diagnostic enable
print(more_undef)
"#;
    let mut cfg = DiagnosticsConfig::default();
    cfg.argument_count_mismatch = DiagnosticSeverityOption::Warning;
    let diags = collect_all(src, "disable_star.lua", cfg);
    // Only `more_undef` should remain.
    let remaining: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(
        remaining.iter().any(|m| m.contains("more_undef")),
        "post-enable diagnostic should remain, got: {:?}",
        remaining,
    );
    assert!(
        !remaining.iter().any(|m| m.contains("uuu")),
        "pre-enable undefined should be suppressed, got: {:?}",
        remaining,
    );
    assert!(
        !remaining.iter().any(|m| m.contains("argument(s)")),
        "pre-enable arg count should be suppressed, got: {:?}",
        remaining,
    );
}

#[test]
fn survivors_carry_stable_code_field() {
    // Every surviving diagnostic should have `code = Some(String(slug))`.
    let src = r#"
print(undef_a)
local x = 1
x = "str"
"#;
    let mut cfg = DiagnosticsConfig::default();
    cfg.emmy_type_mismatch = DiagnosticSeverityOption::Off; // irrelevant
    cfg.unused_local = DiagnosticSeverityOption::Warning;
    let diags = collect_all(src, "codes.lua", cfg);
    assert!(!diags.is_empty(), "expected at least one diagnostic");
    for d in &diags {
        match &d.code {
            Some(NumberOrString::String(s)) => assert!(
                !s.is_empty(),
                "code slug must not be empty, got diag: {:?}",
                d,
            ),
            other => panic!(
                "every survivor must have a String code, got {:?} for {:?}",
                other, d,
            ),
        }
    }
}

#[test]
fn unknown_tag_is_silently_ignored() {
    // `---@diagnostic whatever` shouldn't panic or affect anything.
    let src = r#"
---@diagnostic whatever
print(undef)
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "unknown_tag.lua", cfg);
    assert!(
        diags.iter().any(|d| d.message.contains("Undefined global")),
        "unknown directive tag must not suppress, got: {:?}",
        diags,
    );
}

#[test]
fn disable_next_line_at_eof_is_no_op() {
    // Directive on the very last line with no "next" line — should
    // not panic.
    let src = "---@diagnostic disable-next-line\n";
    let cfg = DiagnosticsConfig::default();
    let _ = collect_all(src, "eof_directive.lua", cfg);
}

#[test]
fn disable_line_with_multiple_codes() {
    let src = r#"
print(use_undef) ---@diagnostic disable-line: undefined-global, unused-local
"#;
    let cfg = DiagnosticsConfig::default();
    let diags = collect_all(src, "disable_line_multi.lua", cfg);
    assert!(
        diags
            .iter()
            .all(|d| !d.message.contains("Undefined global")),
        "multi-code list should cover listed codes, got: {:?}",
        diags,
    );
}
