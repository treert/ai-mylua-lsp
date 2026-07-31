mod test_helpers;

use mylua_lsp::document_link;
use mylua_lsp::uri_id::intern_uri;
use test_helpers::*;

#[test]
fn document_link_resolves_require_paren_form() {
    let (docs, agg, _parser) = setup_workspace(&[
        ("main.lua", "local u = require(\"util\")\n"),
        ("util.lua", "return { x = 1 }\n"),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert_eq!(links.len(), 1, "exactly one require link, got: {:?}", links);
    let link = &links[0];
    assert!(link.target.is_some(), "link must have a target URI");
    let target = link.target.as_ref().unwrap().to_string();
    assert!(
        target.ends_with("util.lua"),
        "target should point at util.lua, got: {}",
        target,
    );
    // Link range should span the string content (inside quotes).
    // Source: `local u = require("util")` — "util" starts after the
    // quote. We only assert width here since column math is UTF-16.
    assert_eq!(
        link.range.end.character - link.range.start.character,
        4,
        "link range should span 'util' (4 chars), got: {:?}",
        link.range,
    );
}

#[test]
fn document_link_resolves_from_module_map_before_summary_exists() {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, "local u = require(\"util\")\n");
    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    let util_uri = make_uri("util.lua");
    let util_uri_id = intern_uri(&util_uri);
    agg.set_require_mapping("util".to_string(), util_uri_id);

    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );

    assert_eq!(
        links.len(),
        1,
        "module-map-only require should yield a link"
    );
    assert_eq!(links[0].target.as_ref(), Some(&util_uri));
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
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert_eq!(
        links.len(),
        1,
        "short-call `require \"util\"` should still yield a link, got: {:?}",
        links,
    );
}

#[test]
fn document_link_ignores_unresolved_module() {
    // `require("no_such_module")` has no workspace target — suppress
    // the link rather than emit a dangling one.
    let (docs, agg, _parser) = setup_workspace(&[("main.lua", "require(\"no_such_module\")\n")]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert!(
        links.is_empty(),
        "unresolved modules must not produce links, got: {:?}",
        links,
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
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert!(
        links.is_empty(),
        "only `require(...)` calls should produce links, got: {:?}",
        links,
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
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert!(
        links.is_empty(),
        "aliased require calls are not followed, got: {:?}",
        links,
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
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert_eq!(
        links.len(),
        2,
        "two distinct require calls → two links, got: {:?}",
        links
    );
}

#[test]
fn document_link_custom_require_literal_replace() {
    // @customrequire with gsub transform: "mgr_abc.abc_mgr" → "module_abc.abc_mgr"
    let (docs, agg, _parser) = setup_workspace(&[
        ("module_abc/abc_mgr.lua", "return { x = 1 }\n"),
        (
            "main.lua",
            "---@customrequire param=module_name mgr_abc module_abc\n\
             function custom_require(module_name)\n\
                 return require(module_name)\n\
             end\n\
             local a = custom_require(\"mgr_abc.abc_mgr\")\n",
        ),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert_eq!(
        links.len(),
        1,
        "custom_require call should produce 1 link, got: {:?}",
        links,
    );
    let target = links[0].target.as_ref().expect("link must have target");
    let target_str = target.to_string();
    assert!(
        target_str.ends_with("module_abc/abc_mgr.lua"),
        "target should point at module_abc/abc_mgr.lua (after transform), got: {}",
        target_str,
    );
}

#[test]
fn document_link_custom_require_no_transform() {
    // @customrequire without transform: arg used directly as module path
    let (docs, agg, _parser) = setup_workspace(&[
        ("module_abc/abc_mgr.lua", "return { x = 1 }\n"),
        (
            "main.lua",
            "---@customrequire param=module_name\n\
             function direct_require(module_name)\n\
                 return require(module_name)\n\
             end\n\
             local m = direct_require(\"module_abc.abc_mgr\")\n",
        ),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert_eq!(
        links.len(),
        1,
        "direct_require call should produce 1 link, got: {:?}",
        links,
    );
    let target = links[0].target.as_ref().expect("link must have target");
    let target_str = target.as_str();
    assert!(
        target_str.ends_with("module_abc/abc_mgr.lua"),
        "target should point at module_abc/abc_mgr.lua, got: {:?}",
        target,
    );
}

#[test]
fn document_link_custom_require_ignores_non_string_arg() {
    // Variable arg: no string literal → no link
    let (docs, agg, _parser) = setup_workspace(&[
        ("module_abc/abc_mgr.lua", "return { x = 1 }\n"),
        (
            "main.lua",
            "---@customrequire param=module_name mgr_abc module_abc\n\
             function custom_require(module_name)\n\
                 return require(module_name)\n\
             end\n\
             local prefix = \"mgr_abc.abc_mgr\"\n\
             local a = custom_require(prefix)\n",
        ),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert!(
        links.is_empty(),
        "non-string arg should not produce a link, got: {:?}",
        links,
    );
}

#[test]
fn document_link_custom_require_ignores_unresolved_module() {
    // Transform produces a path that doesn't resolve → no link
    let (docs, agg, _parser) = setup_workspace(&[
        (
            "main.lua",
            "---@customrequire param=module_name mgr_abc module_abc\n\
             function custom_require(module_name)\n\
                 return require(module_name)\n\
             end\n\
             local a = custom_require(\"mgr_abc.nonexistent\")\n",
        ),
    ]);
    let uri = make_uri("main.lua");
    let doc = docs.get(&intern_uri(&uri)).expect("main.lua opened");
    let links = document_link::document_links(
        doc.root_node().unwrap(),
        doc.source(),
        &agg,
        doc.line_index(),
    );
    assert!(
        links.is_empty(),
        "unresolved module after transform should not produce a link, got: {:?}",
        links,
    );
}
