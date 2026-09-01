//! `diagnostics.narrowByConditionGuard` — existence checks silence
//! "undefined" reports inside the region they guard.
//!
//! Mirrors `tests/lua-root/test_if.lua`.

mod test_helpers;

use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;
use test_helpers::*;

/// Messages of the two guardable codes, in source order.
fn guardable_messages(src: &str, narrow: bool) -> Vec<String> {
    let (doc, uri, mut agg) = setup_single_file(src, "guard.lua");
    let diag_config = DiagnosticsConfig {
        narrow_by_condition_guard: narrow,
        ..Default::default()
    };
    let mut diags = diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        &diag_config,
        doc.line_index(),
    );
    diags.sort_by_key(|d| (d.range.start.line, d.range.start.character));
    diags
        .into_iter()
        .filter(|d| {
            let code = diagnostics::classify_diagnostic_code(&d.message);
            code == "undefined-global" || code == "unknown-field"
        })
        .map(|d| format!("{}: {}", d.range.start.line, d.message))
        .collect()
}

fn assert_all_guarded(src: &str) {
    let with_narrow = guardable_messages(src, true);
    assert!(
        with_narrow.is_empty(),
        "every read is guarded, expected no diagnostics, got: {:#?}",
        with_narrow
    );
    let without = guardable_messages(src, false);
    assert!(
        !without.is_empty(),
        "test is vacuous — nothing was reported even with the guard off"
    );
}

// ---------------------------------------------------------------------------
// Globals a host application registers at run time
// ---------------------------------------------------------------------------

#[test]
fn truthy_if_guards_then_branch() {
    assert_all_guarded(
        "\
if gg_cpp_define_some then
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn not_nil_comparison_guards_then_branch() {
    assert_all_guarded(
        "\
if gg_cpp_define_some ~= nil then
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn nil_comparison_guards_else_branch() {
    assert_all_guarded(
        "\
if gg_cpp_define_some == nil then
else
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn negated_condition_guards_else_branch() {
    assert_all_guarded(
        "\
if not gg_cpp_define_some then
else
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn reversed_nil_comparison_is_recognized() {
    assert_all_guarded(
        "\
if nil ~= gg_cpp_define_some then
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn and_short_circuit_guards_right_operand() {
    assert_all_guarded("local xx = gg_cpp_define_some and gg_cpp_define_some.some_func()\n");
}

#[test]
fn and_or_idiom_is_fully_guarded() {
    assert_all_guarded(
        "local xx = gg_cpp_define_some and gg_cpp_define_some.some_func() or \"\"\nprint(xx)\n",
    );
}

#[test]
fn while_condition_guards_loop_body() {
    assert_all_guarded(
        "\
while gg_cpp_define_some do
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn elseif_condition_guards_its_own_branch() {
    assert_all_guarded(
        "\
if false then
elseif gg_cpp_define_some then
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn else_branch_is_guarded_by_negated_elseif_conditions() {
    assert_all_guarded(
        "\
if false then
elseif gg_cpp_define_some == nil then
else
    print(gg_cpp_define_some)
end
",
    );
}

#[test]
fn nested_reads_inside_the_guarded_branch_are_covered() {
    assert_all_guarded(
        "\
if gg_cpp_define_some then
    if true then
        for i = 1, 10 do
            print(gg_cpp_define_some)
        end
    end
end
",
    );
}

// ---------------------------------------------------------------------------
// Fields — the guard is keyed by access path, not just by name
// ---------------------------------------------------------------------------

#[test]
fn field_read_on_empty_class_is_guarded() {
    assert_all_guarded(
        "\
---@class SomeClsForIf

---@type SomeClsForIf
local x = {}

if x.m_some then
    print(x.m_some)
end
",
    );
}

#[test]
fn guard_on_a_prefix_covers_deeper_reads() {
    // Checking `x.cfg` vouches for `x.cfg.opt` too.
    assert_all_guarded(
        "\
---@class CfgHolder

---@type CfgHolder
local x = {}

if x.cfg then
    print(x.cfg.opt)
end
",
    );
}

#[test]
fn field_guard_does_not_leak_to_a_different_field() {
    let src = "\
---@class TwoFields

---@type TwoFields
local x = {}

if x.first then
    print(x.second)
end
";
    let msgs = guardable_messages(src, true);
    assert!(
        msgs.iter().any(|m| m.contains("second")),
        "a guard on `first` must not cover `second`, got: {:#?}",
        msgs
    );
    assert!(
        !msgs.iter().any(|m| m.contains("first")),
        "the guarded `first` should stay silent, got: {:#?}",
        msgs
    );
}

#[test]
fn deep_guard_does_not_vouch_for_its_own_prefix() {
    // Checking `x.a.b` says nothing about a bare `x.a` read elsewhere.
    let src = "\
---@class DeepHolder

---@type DeepHolder
local x = {}

if x.a.b then
end
print(x.a)
";
    let msgs = guardable_messages(src, true);
    assert!(
        msgs.iter().any(|m| m.starts_with("7:")),
        "the unguarded `x.a` read on line 7 should still report, got: {:#?}",
        msgs
    );
}

// ---------------------------------------------------------------------------
// Reads the guard must NOT cover
// ---------------------------------------------------------------------------

#[test]
fn read_after_the_if_statement_still_reports() {
    let src = "\
if gg_cpp_define_some then
    print(gg_cpp_define_some)
end
print(gg_cpp_define_some)
";
    let msgs = guardable_messages(src, true);
    assert_eq!(
        msgs.len(),
        1,
        "only the read after `end` should report, got: {:#?}",
        msgs
    );
    assert!(msgs[0].starts_with("3:"), "got: {:#?}", msgs);
}

#[test]
fn truthy_guard_does_not_cover_the_else_branch() {
    let src = "\
if gg_cpp_define_some then
else
    print(gg_cpp_define_some)
end
";
    let msgs = guardable_messages(src, true);
    assert_eq!(
        msgs.len(),
        1,
        "`if X then` says X is missing in the else branch, got: {:#?}",
        msgs
    );
    assert!(msgs[0].starts_with("2:"), "got: {:#?}", msgs);
}

#[test]
fn negated_guard_does_not_cover_the_then_branch() {
    let src = "\
if not gg_cpp_define_some then
    print(gg_cpp_define_some)
end
";
    let msgs = guardable_messages(src, true);
    assert_eq!(
        msgs.len(),
        1,
        "`if not X then` says X is missing in the then branch, got: {:#?}",
        msgs
    );
    assert!(msgs[0].starts_with("1:"), "got: {:#?}", msgs);
}

#[test]
fn guard_on_one_name_does_not_cover_another() {
    let src = "\
if gg_first then
    print(gg_second)
end
";
    let msgs = guardable_messages(src, true);
    assert_eq!(msgs.len(), 1, "got: {:#?}", msgs);
    assert!(msgs[0].contains("gg_second"), "got: {:#?}", msgs);
}

#[test]
fn repeat_until_condition_guards_nothing() {
    // `until` is evaluated *after* the body, so the body is unguarded.
    let src = "\
repeat
    print(gg_cpp_define_some)
until gg_cpp_define_some
";
    let msgs = guardable_messages(src, true);
    assert!(
        msgs.iter().any(|m| m.starts_with("1:")),
        "the body read must still report, got: {:#?}",
        msgs
    );
}

#[test]
fn or_right_operand_is_not_guarded() {
    let src = "local v = gg_first or gg_second\n";
    let msgs = guardable_messages(src, true);
    assert!(
        msgs.iter().any(|m| m.contains("gg_second")),
        "`or` does not guard its right operand, got: {:#?}",
        msgs
    );
}

#[test]
fn early_return_guard_is_intentionally_not_supported() {
    // Not a TODO — evaluated and declined.
    //
    // The `if not P then return end` idiom does establish that `P` exists
    // for everything after it, and recognizing it would need statement-order
    // data flow: a fact set accumulated across sibling statements, plus a
    // "does this branch definitely terminate" analysis (`return` / `break` /
    // `error()` / all-branches-terminate), plus a rule for how far a fact
    // propagates out of its block. `assert(P)` and the lazy-init
    // `if not P then P = {} end` are the same family and would share ~70% of
    // that machinery.
    //
    // It is deliberately left out, for a reason that is about product
    // direction rather than difficulty: the more existence idioms get
    // suppressed, the less reason anyone has to write the annotation that
    // would actually give them types. Annotations (`---@class`, `---@meta`)
    // are exact and predictable; suppression is inherently heuristic, and
    // Lua admits unboundedly many ways to say "this might not exist", so
    // each new form bought here also buys a new way to hide a real bug.
    //
    // What this pass covers — reads lexically nested inside the guarded
    // region, reachable by walking ancestors — is the deliberate stopping
    // point: enough to clear the obvious noise, not enough to make skipping
    // annotations free.
    //
    // So a failure here means suppression got *wider* than intended. Check
    // that against the above before updating the expectation.
    let src = "\
if not gg_cpp_define_some then
    return
end
print(gg_cpp_define_some)
";
    let msgs = guardable_messages(src, true);
    assert!(
        msgs.iter().any(|m| m.starts_with("3:")),
        "early return is intentionally not treated as a guard; suppression \
         appears to have widened — got: {:#?}",
        msgs
    );
}

// ---------------------------------------------------------------------------
// The switch itself
// ---------------------------------------------------------------------------

#[test]
fn disabling_the_switch_restores_every_report() {
    let src = "\
if gg_cpp_define_some then
    print(gg_cpp_define_some)
end
";
    assert!(guardable_messages(src, true).is_empty());
    let without = guardable_messages(src, false);
    assert_eq!(
        without.len(),
        2,
        "with the guard off both the condition and the body report, got: {:#?}",
        without
    );
}

#[test]
fn other_diagnostic_codes_are_left_alone() {
    // A duplicate key inside a guarded branch is unrelated to existence,
    // so the guard must not swallow it.
    let src = "\
if gg_cpp_define_some then
    local t = { a = 1, a = 2 }
end
";
    let (doc, uri, mut agg) = setup_single_file(src, "guard_other.lua");
    let diag_config = DiagnosticsConfig::default();
    let diags = diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        &diag_config,
        doc.line_index(),
    );
    assert!(
        diags
            .iter()
            .any(|d| d.message.starts_with("Duplicate table key")),
        "unrelated codes must survive, got: {:#?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}
