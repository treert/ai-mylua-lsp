mod test_helpers;

use test_helpers::*;
use mylua_lsp::document_link;

#[test]
fn document_link_resolves_require_paren_form() {
    let (docs, agg, _parser) = setup_workspace(&[
        ("main.lua", "local u = require(\"util\")\n"),
        ("util.lua", "return { x = 1 }\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&uri).expect("main.lua opened");
    let links = document_link::document_links(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &agg,
        &doc.line_index,
    );
    assert_eq!(links.len(), 1, "exactly one require link, got: {:?}", links);
    let link = &links[0];
    assert!(link.target.is_some(), "link must have a target URI");
    let target = link.target.as_ref().unwrap().to_string();
    assert!(
        target.ends_with("util.lua"),
        "target should point at util.lua, got: {}", target,
    );
    // Link range should span the string content (inside quotes).
    // Source: `local u = require("util")` — "util" starts after the
    // quote. We only assert width here since column math is UTF-16.
    assert_eq!(
        link.range.end.character - link.range.start.character,
        4,
        "link range should span 'util' (4 chars), got: {:?}", link.range,
    );
}

#[test]
fn document_link_resolves_require_short_call() {
    // `require "util"` (no parens) — the grammar's `arguments` is the
    // string node directly.
    let (docs, agg, _parser) = setup_workspace(&[
        ("main.lua", "local u = require \"util\"\n"),
        ("util.lua", "return 1\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&uri).expect("main.lua opened");
    let links = document_link::document_links(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &agg,
        &doc.line_index,
    );
    assert_eq!(
        links.len(), 1,
        "short-call `require \"util\"` should still yield a link, got: {:?}", links,
    );
}

#[test]
fn document_link_ignores_unresolved_module() {
    // `require("no_such_module")` has no workspace target — suppress
    // the link rather than emit a dangling one.
    let (docs, agg, _parser) = setup_workspace(&[
        ("main.lua", "require(\"no_such_module\")\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&uri).expect("main.lua opened");
    let links = document_link::document_links(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &agg,
        &doc.line_index,
    );
    assert!(
        links.is_empty(),
        "unresolved modules must not produce links, got: {:?}", links,
    );
}

#[test]
fn document_link_ignores_non_require_calls() {
    // Other single-string calls must not be treated as require.
    let (docs, agg, _parser) = setup_workspace(&[
        ("main.lua", "print(\"hello\")\nerror(\"oops\")\n"),
        ("hello.lua", "return 1\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&uri).expect("main.lua opened");
    let links = document_link::document_links(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &agg,
        &doc.line_index,
    );
    assert!(
        links.is_empty(),
        "only `require(...)` calls should produce links, got: {:?}", links,
    );
}

#[test]
fn document_link_rejects_aliased_require() {
    // `m = require; m("util")` — callee is `m`, not `require`. Even
    // though the runtime behavior equals `require("util")`, we
    // deliberately don't follow it to avoid false positives where
    // the user has an unrelated `m` callable.
    let (docs, agg, _parser) = setup_workspace(&[
        ("main.lua", "local m = require\nm(\"util\")\n"),
        ("util.lua", "return 1\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&uri).expect("main.lua opened");
    let links = document_link::document_links(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &agg,
        &doc.line_index,
    );
    assert!(
        links.is_empty(),
        "aliased require calls are not followed, got: {:?}", links,
    );
}

#[test]
fn document_link_multi_require_each_get_link() {
    let (docs, agg, _parser) = setup_workspace(&[
        (
            "main.lua",
            "local a = require(\"util\")\nlocal b = require(\"helper\")\n",
        ),
        ("util.lua", "return 1\n"),
        ("helper.lua", "return 2\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&uri).expect("main.lua opened");
    let links = document_link::document_links(
        doc.tree.root_node(),
        doc.text.as_bytes(),
        &agg,
        &doc.line_index,
    );
    assert_eq!(links.len(), 2, "two distinct require calls → two links, got: {:?}", links);
}
