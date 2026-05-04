mod test_helpers;

use mylua_lsp::config::GotoStrategy;
use mylua_lsp::goto;
use mylua_lsp::type_system::{KnownType, TypeFact};
use mylua_lsp::uri_id::intern_uri;
use test_helpers::*;
use tower_lsp_server::ls_types::GotoDefinitionResponse;

fn single_loc(resp: &GotoDefinitionResponse) -> &tower_lsp_server::ls_types::Location {
    match resp {
        GotoDefinitionResponse::Scalar(loc) => loc,
        GotoDefinitionResponse::Array(v) if !v.is_empty() => &v[0],
        _ => panic!("expected at least one location, got {:?}", resp),
    }
}

#[test]
fn type_definition_local_annotated_with_at_type() {
    // `---@type Foo local x = nil` — typeDefinition on `x` should jump
    // to the `@class Foo` definition (stored as the range of the
    // next non-comment statement that "owns" the class). The key
    // assertion is that it jumps to the Foo DECLARATION, not the `f`
    // local declaration.
    let src = r#"---@class Foo
---@field x number
Foo = {}

---@type Foo
local f = nil
print(f)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "td1.lua");

    // Click `f` on the `print(f)` line — line 6, col 6
    let r = goto::goto_type_definition(&doc, intern_uri(&uri), pos(6, 6), &mut agg, &GotoStrategy::Auto)
        .expect("type definition should resolve");
    let loc = single_loc(&r);
    // The Foo class definition anchor is at line 2 (`Foo = {}`).
    // The critical guarantee: NOT line 5 (`local f`) — typeDefinition
    // must diverge from goto_definition here.
    assert_ne!(loc.range.start.line, 5, "must not land on `local f = nil`");
    assert_eq!(loc.range.start.line, 2, "should anchor on Foo's class definition line");
}

#[test]
fn type_definition_clicked_on_type_name_itself() {
    // Click on `Foo` in `---@type Foo` — the identifier IS a type
    // name, typeDefinition should land on Foo's class definition.
    let src = r#"---@class Foo
Foo = {}

---@type Foo
local f = nil
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "td2.lua");

    // `Foo` in the `---@type Foo` comment on line 3, col 11.
    let r = goto::goto_type_definition(&doc, intern_uri(&uri), pos(3, 11), &mut agg, &GotoStrategy::Auto)
        .expect("type definition should resolve");
    let loc = single_loc(&r);
    // Foo's anchor is line 1 (`Foo = {}` — line 0 is the `@class`).
    assert_eq!(loc.range.start.line, 1);
}

#[test]
fn type_definition_falls_back_to_definition_for_primitive() {
    // `local n = 1` — `n` has no Emmy-named type. typeDefinition
    // should fall back to the plain definition (line 0).
    let src = "local n = 1\nprint(n)\n";
    let (doc, uri, mut agg) = setup_single_file(src, "td3.lua");

    let r = goto::goto_type_definition(&doc, intern_uri(&uri), pos(1, 6), &mut agg, &GotoStrategy::Auto)
        .expect("should fall back to goto_definition");
    let loc = single_loc(&r);
    assert_eq!(loc.range.start.line, 0);
}

#[test]
fn type_definition_emmy_generic() {
    // `---@class Box<T>` + `---@type Box<number> local b` — clicking
    // `b` should still jump to `@class Box`.
    let src = r#"---@class Box<T>
---@field value T
Box = {}

---@type Box<number>
local b = nil
print(b)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "td_generic.lua");

    let r = goto::goto_type_definition(&doc, intern_uri(&uri), pos(6, 6), &mut agg, &GotoStrategy::Auto)
        .expect("should resolve EmmyGeneric base type");
    let loc = single_loc(&r);
    // Box's anchor statement is `Box = {}` on line 2.
    assert_ne!(loc.range.start.line, 5, "must not land on `local b = nil`");
    assert_eq!(loc.range.start.line, 2);
}

#[test]
fn type_definition_follows_call_return_stub_to_emmy_type() {
    // `local x = MakeFoo()` — indirect Emmy type via function return.
    // The resolver must chase the CallReturn stub through
    // `function_summaries["MakeFoo"].returns[0]` and land on `Foo`.
    let src = r#"---@class Foo
Foo = {}

---@return Foo
function MakeFoo() end

local x = MakeFoo()
print(x)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "td_call.lua");

    // Click `x` in `print(x)` — line 7, col 6
    let r = goto::goto_type_definition(&doc, intern_uri(&uri), pos(7, 6), &mut agg, &GotoStrategy::Auto)
        .expect("typeDefinition should chase CallReturn → EmmyType");
    let loc = single_loc(&r);
    // Foo's anchor is `Foo = {}` on line 1.
    assert_eq!(loc.range.start.line, 1, "should jump to Foo class anchor, got {:?}", loc);
}

#[test]
fn type_definition_unknown_returns_none_after_fallback_also_fails() {
    // Clicking in empty whitespace should return None (no identifier
    // → no plain definition either).
    let (doc, uri, mut agg) = setup_single_file("\n\n", "td_empty.lua");
    let r = goto::goto_type_definition(&doc, intern_uri(&uri), pos(0, 0), &mut agg, &GotoStrategy::Auto);
    assert!(r.is_none());
}

#[test]
fn class_annotation_does_not_bind_across_blank_line_and_plain_doc_comment() {
    let src = r#"---@class _ClassLala
---@field lala string

--- utils lala
utils = {}
"#;
    let (_doc, uri, agg) = setup_single_file(src, "td_plain_doc_gap.lua");
    let summary = summary_by_uri(&agg, &uri).expect("summary");
    let contrib = summary
        .global_contributions
        .iter()
        .find(|c| c.name == "utils")
        .expect("utils contribution");

    assert!(
        !matches!(
            &contrib.type_fact,
            TypeFact::Known(KnownType::EmmyType(name)) if name == "_ClassLala"
        ),
        "plain doc comments after a blank line must not carry @class binding"
    );
}
