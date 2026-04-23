mod test_helpers;

use mylua_lsp::config::InlayHintConfig;
use mylua_lsp::inlay_hint;
use test_helpers::*;
use tower_lsp_server::ls_types::{InlayHintKind, InlayHintLabel, Range};

fn cfg(enable: bool, params: bool, types: bool) -> InlayHintConfig {
    InlayHintConfig {
        enable,
        parameter_names: params,
        variable_types: types,
    }
}

fn full_range() -> Range {
    Range {
        start: pos(0, 0),
        end: pos(1000, 0),
    }
}

fn label(h: &tower_lsp_server::ls_types::InlayHint) -> String {
    match &h.label {
        InlayHintLabel::String(s) => s.clone(),
        InlayHintLabel::LabelParts(parts) => parts.iter().map(|p| p.value.clone()).collect(),
    }
}

#[test]
fn inlay_hints_disabled_by_default() {
    // Master enable defaults off — asking with `enable=false` should
    // return empty regardless of the rest.
    let src = "local function foo(a, b) end\nfoo(1, 2)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let hints = inlay_hint::inlay_hints(&doc, &uri, full_range(), &mut agg, &cfg(false, true, true));
    assert!(hints.is_empty(), "disabled config returns nothing, got: {:?}", hints);
}

#[test]
fn inlay_hints_parameter_names_at_call_site() {
    let src = "local function foo(a, b) end\nfoo(1, 2)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let hints = inlay_hint::inlay_hints(&doc, &uri, full_range(), &mut agg, &cfg(true, true, false));
    let labels: Vec<String> = hints.iter().map(label).collect();
    assert!(
        labels.iter().any(|l| l == "a:"),
        "should include `a:` hint, got: {:?}", labels,
    );
    assert!(
        labels.iter().any(|l| l == "b:"),
        "should include `b:` hint, got: {:?}", labels,
    );
    // All hints should be PARAMETER kind here.
    assert!(hints.iter().all(|h| h.kind == Some(InlayHintKind::PARAMETER)));
}

#[test]
fn inlay_hints_skip_argument_same_name_as_param() {
    // `foo(a, b)` where the argument names match the parameter names
    // — hint would be redundant noise, skip.
    let src = "local function foo(a, b) end\nlocal a = 1\nlocal b = 2\nfoo(a, b)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let hints = inlay_hint::inlay_hints(&doc, &uri, full_range(), &mut agg, &cfg(true, true, false));
    let labels: Vec<String> = hints.iter().map(label).collect();
    let param_hints: Vec<&String> = labels.iter().filter(|l| l.ends_with(":")).collect();
    assert!(
        param_hints.is_empty() || !param_hints.iter().any(|l| *l == "a:" || *l == "b:"),
        "same-name arg hints must be filtered, got: {:?}", param_hints,
    );
}

#[test]
fn inlay_hints_variable_type_for_primitive() {
    let src = "local n = 42\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let hints = inlay_hint::inlay_hints(&doc, &uri, full_range(), &mut agg, &cfg(true, false, true));
    let labels: Vec<String> = hints.iter().map(label).collect();
    assert!(
        labels.iter().any(|l| l.contains("integer") || l.contains("number")),
        "should hint `n` as a number-like type, got: {:?}", labels,
    );
    assert!(hints.iter().any(|h| h.kind == Some(InlayHintKind::TYPE)));
}

#[test]
fn inlay_hints_variable_type_skipped_when_emmy_annotated() {
    // `---@type Foo local x = nil` — user already wrote the type,
    // don't duplicate as an inlay hint.
    let src = "---@type Foo\nlocal x = nil\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");
    let hints = inlay_hint::inlay_hints(&doc, &uri, full_range(), &mut agg, &cfg(true, false, true));
    let types: Vec<_> = hints
        .iter()
        .filter(|h| h.kind == Some(InlayHintKind::TYPE))
        .collect();
    assert!(types.is_empty(), "no TYPE hint when emmy annotation present, got: {:?}", types);
}

#[test]
fn inlay_hints_respect_range_filter() {
    // Only hints inside the requested range should be emitted.
    let src = "local function foo(a, b) end\nfoo(1, 2)\nfoo(3, 4)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "a.lua");

    // Narrow range: only line 1 (first call site).
    let range = Range {
        start: pos(1, 0),
        end: pos(1, 100),
    };
    let hints = inlay_hint::inlay_hints(&doc, &uri, range, &mut agg, &cfg(true, true, false));
    // Line 2's call should not contribute hints.
    for h in &hints {
        assert!(h.position.line == 1, "hint out of requested range: {:?}", h);
    }
}
