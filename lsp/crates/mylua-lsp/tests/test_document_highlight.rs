mod test_helpers;

use mylua_lsp::aggregation::WorkspaceAggregation;
use mylua_lsp::document::Document;
use mylua_lsp::document_highlight::document_highlight;
use test_helpers::*;
use tower_lsp_server::ls_types::{DocumentHighlight, DocumentHighlightKind, Position, Uri};

/// Shortcut: collect (start_line, start_char, kind) triples for easy
/// assertion without hand-writing `DocumentHighlight { ... }` structs.
fn triples(
    highlights: &[tower_lsp_server::ls_types::DocumentHighlight],
) -> Vec<(u32, u32, DocumentHighlightKind)> {
    highlights
        .iter()
        .map(|h| {
            (
                h.range.start.line,
                h.range.start.character,
                h.kind.unwrap_or(DocumentHighlightKind::TEXT),
            )
        })
        .collect()
}

/// `document_highlight` on a single-file workspace.
///
/// The `uri_id` / aggregation arguments exist because a name that is *not* a
/// visible local is resolved through `name_resolution` (§1.6), which needs the
/// index to tell a sandboxed free name from the same-named global.
fn highlights(
    doc: &Document,
    uri: &Uri,
    agg: &WorkspaceAggregation,
    at: Position,
) -> Option<Vec<DocumentHighlight>> {
    document_highlight(doc, intern_uri(uri), at, agg)
}

#[test]
fn document_highlight_local_read_and_write() {
    // local x = 1; x = 2; print(x)
    // expected: line 0 col 6 Write, line 1 col 0 Write, line 2 col 6 Read
    let src = "local x = 1\nx = 2\nprint(x)\n";
    let (doc, uri, agg) = setup_single_file(src, "rw.lua");

    // Click on the first `x`
    let h = highlights(&doc, &uri, &agg, pos(0, 6)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 6, DocumentHighlightKind::WRITE),
            (1, 0, DocumentHighlightKind::WRITE),
            (2, 6, DocumentHighlightKind::READ),
        ],
        "local decl + assignment + usage, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_parameter_is_write() {
    // function f(x) return x + 1 end
    let src = "function f(x) return x + 1 end\n";
    let (doc, uri, agg) = setup_single_file(src, "param.lua");

    // Click on the parameter declaration `x` at col 11
    let h = highlights(&doc, &uri, &agg, pos(0, 11)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 11, DocumentHighlightKind::WRITE),
            (0, 21, DocumentHighlightKind::READ),
        ],
        "parameter decl is Write, usage is Read, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_for_numeric_loop_var() {
    let src = "for i = 1, 10 do\n  print(i)\nend\n";
    let (doc, uri, agg) = setup_single_file(src, "forn.lua");

    let h = highlights(&doc, &uri, &agg, pos(0, 4)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 4, DocumentHighlightKind::WRITE),
            (1, 8, DocumentHighlightKind::READ),
        ],
    );
}

#[test]
fn document_highlight_for_generic_loop_vars() {
    let src = "for k, v in pairs(t) do\n  print(k, v)\nend\n";
    let (doc, uri, agg) = setup_single_file(src, "forg.lua");

    let h = highlights(&doc, &uri, &agg, pos(0, 4)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 4, DocumentHighlightKind::WRITE),
            (1, 8, DocumentHighlightKind::READ),
        ],
    );
}

#[test]
fn document_highlight_function_declaration_name() {
    let src = "local function foo() return 1 end\nprint(foo())\n";
    let (doc, uri, agg) = setup_single_file(src, "fndecl.lua");

    let h = highlights(&doc, &uri, &agg, pos(0, 15)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 15, DocumentHighlightKind::WRITE),
            (1, 6, DocumentHighlightKind::READ),
        ],
    );
}

#[test]
fn document_highlight_shadowing_respects_scope() {
    let src = "local x = 1\ndo\n  local x = 2\n  print(x)\nend\nprint(x)\n";
    let (doc, uri, agg) = setup_single_file(src, "shadow.lua");

    // Click on OUTER `x` at line 0, col 6. Expected highlights: outer
    // decl + outer usage on line 5 only; NOT the inner ones.
    let h = highlights(&doc, &uri, &agg, pos(0, 6)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 6, DocumentHighlightKind::WRITE),
            (5, 6, DocumentHighlightKind::READ),
        ],
        "shadowed inner `x` must not appear, got: {:?}",
        t,
    );

    // Click on INNER `x` at line 2, col 8. Expected: inner decl +
    // inner usage only.
    let h = highlights(&doc, &uri, &agg, pos(2, 8)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (2, 8, DocumentHighlightKind::WRITE),
            (3, 8, DocumentHighlightKind::READ),
        ],
        "outer `x` must not appear when clicking inner, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_global_variable() {
    // Global `G` — no scope decl, and the file never redirects `_ENV`, so this
    // stays on the plain text-matching path.
    let src = "G = 1\nG = G + 1\nprint(G)\n";
    let (doc, uri, agg) = setup_single_file(src, "global.lua");

    let h = highlights(&doc, &uri, &agg, pos(0, 0)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 0, DocumentHighlightKind::WRITE),
            (1, 0, DocumentHighlightKind::WRITE),
            (1, 4, DocumentHighlightKind::READ),
            (2, 6, DocumentHighlightKind::READ),
        ],
    );
}

#[test]
fn document_highlight_local_x_equals_x_rhs_not_new_local() {
    // `local x = x + 1` — RHS `x` refers to the outer `x`, not the
    // newly declared one. When we click on the new local (LHS),
    // highlights should NOT include the RHS `x`.
    let src = "local x = 10\nlocal x = x + 1\nprint(x)\n";
    let (doc, uri, agg) = setup_single_file(src, "self_ref.lua");

    // Click on the NEW (inner) `x` at line 1 col 6
    let h = highlights(&doc, &uri, &agg, pos(1, 6)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (1, 6, DocumentHighlightKind::WRITE),
            (2, 6, DocumentHighlightKind::READ),
        ],
        "RHS `x` refers to outer, must not be highlighted, got: {:?}",
        t,
    );

    // Click on the OUTER `x` at line 0 col 6
    let h = highlights(&doc, &uri, &agg, pos(0, 6)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 6, DocumentHighlightKind::WRITE),
            (1, 10, DocumentHighlightKind::READ),
        ],
        "outer `x` has decl + RHS reference; inner shadows rest, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_indexed_assignment_base_is_read() {
    // `t.x = 1` — the `t` is READ for indexing, only the slot `t.x`
    // is actually written. Analogous for `t[k] = v`.
    let src = "t = {}\nt.x = 1\nt[42] = 2\nprint(t)\n";
    let (doc, uri, agg) = setup_single_file(src, "indexed.lua");

    // Click on the global `t` declaration (line 0 col 0)
    let h = highlights(&doc, &uri, &agg, pos(0, 0)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 0, DocumentHighlightKind::WRITE), // t = {}
            (1, 0, DocumentHighlightKind::READ),  // t.x = 1  — base is read
            (2, 0, DocumentHighlightKind::READ),  // t[42] = 2 — base is read
            (3, 6, DocumentHighlightKind::READ),  // print(t)
        ],
        "indexed-assignment base must classify as READ, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_no_match_on_nothing() {
    // Empty file / cursor on whitespace — return None rather than
    // panicking.
    let (doc, uri, agg) = setup_single_file("", "empty.lua");
    assert!(highlights(&doc, &uri, &agg, pos(0, 0)).is_none());
}

// ---------------------------------------------------------------------------
// Environment boundaries
// ---------------------------------------------------------------------------
//
// A free name is `_ENV.name`, so two occurrences of the same text on either
// side of a `_ENV` redirect are different variables at run time. goto /
// references have distinguished them for a long time (§1.3); highlight was the
// last capability still matching pure text, so clicking any one of them lit up
// all of them.
//
// The semantic pass is confined to names that are **not** visible locals, and
// only runs at all when the file redirects `_ENV` somewhere — an ordinary file
// keeps the original text-matching cost.

#[test]
fn document_highlight_separates_the_two_sides_of_an_env_redirect() {
    // `tests/lua-root/test_env.lua` §4: the first `g` is a field of the real
    // global environment, the last two are fields of the sandbox table.
    let src = "g = 1\n_ENV = {}\ng = 2\nprint(g)\n";
    let (doc, uri, agg) = setup_single_file(src, "env_boundary_highlight.lua");

    let h = highlights(&doc, &uri, &agg, pos(0, 0)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![(0, 0, DocumentHighlightKind::WRITE)],
        "the pre-redirect `g` is the real global and stands alone, got: {:?}",
        t,
    );

    let h = highlights(&doc, &uri, &agg, pos(2, 0)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (2, 0, DocumentHighlightKind::WRITE),
            (3, 6, DocumentHighlightKind::READ),
        ],
        "the post-redirect `g` covers only the sandbox's own write and read, \
         got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_groups_an_env_field_with_its_definition() {
    // The field's definition site is a table-constructor key, which is not a
    // *reference* position — it is matched against the resolved definition
    // range instead, so clicking the use still highlights where it came from.
    let src = "local _ENV = { own = 1 }\nprint(own)\n";
    let (doc, uri, agg) = setup_single_file(src, "env_field_def_highlight.lua");

    let h = highlights(&doc, &uri, &agg, pos(1, 6)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 15, DocumentHighlightKind::READ),
            (1, 6, DocumentHighlightKind::READ),
        ],
        "the env field's own definition must stay grouped with its use, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_keeps_a_sandbox_fallthrough_name_with_its_global() {
    // SYMMETRY GUARD, mirroring `references::verify_global`. A
    // `{ __index = _G }` sandbox does not describe its field set, so a name it
    // does not have really *is* the global — one symbol, both occurrences.
    let src = "Reachable = 1\n\
               local _ENV = setmetatable({}, { __index = _G })\n\
               local x = Reachable\n";
    let (doc, uri, agg) = setup_single_file(src, "sandbox_fallthrough_highlight.lua");

    let h = highlights(&doc, &uri, &agg, pos(0, 0)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 0, DocumentHighlightKind::WRITE),
            (2, 10, DocumentHighlightKind::READ),
        ],
        "a name reachable through `__index` must stay one symbol with the \
         global, got: {:?}",
        t,
    );
}

#[test]
fn document_highlight_keeps_locals_out_of_a_sandboxed_name() {
    // Lexical names are not environment fields. A local that happens to share
    // its spelling with a sandbox field is a different symbol.
    let src = "local _ENV = { shared = 1 }\n\
               print(shared)\n\
               local function f() local shared = 2 return shared end\n";
    let (doc, uri, agg) = setup_single_file(src, "sandbox_vs_local_highlight.lua");

    let h = highlights(&doc, &uri, &agg, pos(1, 6)).expect("Some");
    let t = triples(&h);
    assert_eq!(
        t,
        vec![
            (0, 15, DocumentHighlightKind::READ),
            (1, 6, DocumentHighlightKind::READ),
        ],
        "the local `shared` inside `f` must not be highlighted, got: {:?}",
        t,
    );
}
