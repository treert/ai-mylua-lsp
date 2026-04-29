mod test_helpers;

use test_helpers::*;
use mylua_lsp::config::GotoStrategy;
use mylua_lsp::goto;

#[test]
fn goto_unresolved_dotted_field_does_not_fallback_to_bare_global_name() {
    let main_src = "local XX = UE4.Class()";
    let other_src = r#"function foo()
    local Class = Actor:GetClass()
    Class = Actor.ParentClass
end"#;
    let (docs, mut agg, _) = setup_workspace(&[
        ("main.lua", main_src),
        ("other.lua", other_src),
    ]);
    let main_uri = make_uri("main.lua");
    let main_doc = docs.get(&main_uri).expect("main doc");

    let result = goto::goto_definition(
        main_doc,
        &main_uri,
        pos(0, 15),
        &mut agg,
        &GotoStrategy::Auto,
        &docs,
    );

    assert!(
        result.is_none(),
        "unresolved `UE4.Class` field must not fall back to unrelated bare `Class` globals: {:?}",
        result,
    );
}

#[test]
fn goto_unresolved_dotted_field_does_not_fallback_to_visible_local_name() {
    let src = r#"local Class = 1
local XX = UE4.Class()"#;
    let (doc, uri, mut agg) = setup_single_file(src, "local_name_collision.lua");

    let result = goto::goto_definition(
        &doc,
        &uri,
        pos(1, 15),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );

    assert!(
        result.is_none(),
        "unresolved `UE4.Class` field must not fall back to visible local `Class`: {:?}",
        result,
    );
}

#[test]
fn local_reassignment_is_not_indexed_as_global_contribution() {
    let src = r#"function foo()
    local Class = Actor:GetClass()
    Class = Actor.ParentClass
end"#;
    let (_doc, _uri, agg) = setup_single_file(src, "local_reassign.lua");

    assert!(
        agg.global_shard.get("Class").is_none(),
        "assignment to visible local `Class` must not be indexed as a global",
    );
}

#[test]
fn class_assignment_does_not_bind_preexisting_local() {
    let src = r#"local Foo
---@class Foo
Foo = class()
function Foo:bar()
end"#;
    let (_doc, uri, agg) = setup_single_file(src, "local_class_assignment.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let class = summary
        .type_definitions
        .iter()
        .find(|td| td.name == "Foo")
        .expect("Foo class");

    assert!(
        class.fields.iter().all(|field| field.name != "bar"),
        "`---@class Foo` should not bind to a local declared before the comment block",
    );
    assert!(
        agg.global_shard.get("Foo").is_none(),
        "assignment to existing local `Foo` must not be indexed as a global",
    );
    assert!(
        agg.global_shard.get("Foo.bar").is_none(),
        "method on local-bound class `Foo` must not be indexed as a global",
    );
}

#[test]
fn class_binds_immediately_following_local_declaration_with_value() {
    let src = r#"---@class Foo
local M = {}
function M:bar()
end"#;
    let (_doc, uri, agg) = setup_single_file(src, "immediate_local_class.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let class = summary
        .type_definitions
        .iter()
        .find(|td| td.name == "Foo")
        .expect("Foo class");

    assert!(
        class.fields.iter().any(|field| field.name == "bar"),
        "`---@class Foo` should bind to the immediately following `local M = ...`",
    );
    assert!(
        class.anchor_shape_id.is_some(),
        "`---@class Foo` bound to `local M = {{}}` should use M's table shape as anchor",
    );
}

#[test]
fn class_binds_immediately_following_local_without_value() {
    let src = r#"---@class Foo
local M
function M:bar()
end"#;
    let (_doc, uri, agg) = setup_single_file(src, "local_class_without_value.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let class = summary
        .type_definitions
        .iter()
        .find(|td| td.name == "Foo")
        .expect("Foo class");

    assert!(
        class.fields.iter().any(|field| field.name == "bar"),
        "`---@class Foo` should bind to the immediately following `local M` even without an initializer",
    );
    assert!(
        class.anchor_shape_id.is_none(),
        "`local M` without an initializer has no table shape anchor",
    );
}

#[test]
fn bare_function_declaration_assigning_visible_local_is_not_global() {
    let src = r#"local make
function make()
end"#;
    let (_doc, _uri, agg) = setup_single_file(src, "local_function_assignment.lua");

    assert!(
        agg.global_shard.get("make").is_none(),
        "`function make()` should assign the visible local, not create a global",
    );
}

#[test]
fn function_declaration_inside_local_initializer_does_not_see_local_name() {
    let src = r#"local make = function()
    function make()
    end
end"#;
    let (_doc, _uri, agg) = setup_single_file(src, "initializer_visibility_function.lua");

    assert!(
        agg.global_shard.get("make").is_some(),
        "`function make()` inside local make's initializer should assign global make",
    );
}

#[test]
fn assignment_inside_local_initializer_does_not_see_local_name() {
    let src = r#"local Foo = function()
Foo = class()
end"#;
    let (_doc, _uri, agg) = setup_single_file(src, "initializer_visibility_assignment.lua");

    assert!(
        agg.global_shard.get("Foo").is_some(),
        "`Foo = class()` inside local Foo's initializer should assign global Foo",
    );
}

#[test]
fn class_anchor_shape_backfill_uses_bound_local_not_same_name_shadow() {
    let src = r#"---@class Foo
Foo = {}
do
    local Foo = { shadow_only = 1 }
end"#;
    let (_doc, uri, agg) = setup_single_file(src, "class_anchor_shadow.lua");
    let summary = agg.summaries.get(&uri).expect("summary");
    let class = summary
        .type_definitions
        .iter()
        .find(|td| td.name == "Foo")
        .expect("Foo class");

    assert!(
        class.anchor_shape_id.is_none(),
        "global class Foo must not borrow a same-named local table shape as its anchor",
    );
}

#[test]
fn goto_nested_chained_field_jumps_to_assignment_site() {
    // P2 / future-work §0 (AST chained assign): after `a.b.c = 1`
    // registers `c` on the inner `a.b` shape (AST-driven, not splitn),
    // goto on `.c` in a subsequent read site must reach the assignment
    // line. Previously this would fall back to None because
    // `resolve_field_chain` had no URI context for the per-file Table
    // shape id and silently returned Unknown.
    let src = r#"local a = { b = { c = 0 } }
a.b.c = 1
print(a.b.c)
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "nested_goto.lua");

    // Line 2 `print(a.b.c)` — p=0 r=1 i=2 n=3 t=4 (=5 a=6 .=7 b=8 .=9 c=10
    let result = goto::goto_definition(&doc, &uri, pos(2, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(
        result.is_some(),
        "goto on chained .c should jump to the assignment site, got None",
    );
}

#[test]
fn goto_local_variable_definition() {
    let src = r#"local myVar = 42
print(myVar)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // `myVar` on line 1, col 6
    let result = goto::goto_definition(&doc, &uri, pos(1, 6), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result.is_some(), "goto should find definition of `myVar`");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 0, "myVar defined on line 0");
    }
}

#[test]
fn goto_local_function_definition() {
    let src = r#"local function foo()
    return 1
end
local x = foo()"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // `foo` on line 3, col 10
    let result = goto::goto_definition(&doc, &uri, pos(3, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result.is_some(), "goto should find definition of `foo`");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 0, "foo defined on line 0");
    }
}

#[test]
fn goto_parameter() {
    let src = r#"function bar(param1, param2)
    print(param1)
end"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // `param1` on line 1, col 10
    let result = goto::goto_definition(&doc, &uri, pos(1, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result.is_some(), "goto should find definition of parameter `param1`");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 0, "param1 defined on line 0");
    }
}

#[test]
fn goto_for_variable() {
    let src = r#"for i = 1, 10 do
    print(i)
end"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // `i` on line 1, col 10
    let result = goto::goto_definition(&doc, &uri, pos(1, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result.is_some(), "goto should find for-variable `i`");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 0, "for-variable i defined on line 0");
    }
}

#[test]
fn goto_no_result_for_undefined() {
    let src = "print(totally_undefined_name_xyz)";
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // on `totally_undefined_name_xyz`
    let result = goto::goto_definition(&doc, &uri, pos(0, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    // May or may not find something (globals index etc.), but should not panic
    let _ = result;
}

#[test]
fn goto_require_jumps_to_module_return() {
    use mylua_lsp::{document::Document, summary_builder};
    use mylua_lsp::util::LuaSource;

    let mut parser = new_parser();

    let mod_src = "local M = {}\nM.x = 1\nreturn M";
    let mod_uri = make_uri("mymod.lua");
    let mod_tree = parser.parse(mod_src.as_bytes(), None).unwrap();
    let mod_lua_source = LuaSource::new(mod_src.to_string());
    let (mod_summary, mod_scope) = summary_builder::build_file_analysis(&mod_uri, &mod_tree, mod_lua_source.source(), mod_lua_source.line_index());
    let _mod_doc = Document { lua_source: mod_lua_source, tree: mod_tree, scope_tree: mod_scope };

    let caller_src = "local m = require(\"mymod\")\nprint(m)";
    let caller_uri = make_uri("caller.lua");
    let caller_tree = parser.parse(caller_src.as_bytes(), None).unwrap();
    let caller_lua_source = LuaSource::new(caller_src.to_string());
    let (caller_summary, caller_scope) = summary_builder::build_file_analysis(&caller_uri, &caller_tree, caller_lua_source.source(), caller_lua_source.line_index());
    let caller_doc = Document { lua_source: caller_lua_source, tree: caller_tree, scope_tree: caller_scope };

    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    agg.set_require_mapping("mymod".to_string(), mod_uri.clone());
    agg.upsert_summary(mod_summary);
    agg.upsert_summary(caller_summary);

    // Click on `m` (line 0 col 6) in caller.lua — should jump to mymod.lua's
    // `return M` (line 2, column 0).
    let result = mylua_lsp::goto::goto_definition(
        &caller_doc, &caller_uri, pos(0, 6), &mut agg, &GotoStrategy::Auto, &empty_docs(),
    )
    .expect("require goto should resolve");

    if let tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc) = &result {
        assert_eq!(loc.uri, mod_uri, "should target mymod.lua");
        assert_eq!(
            loc.range.start.line, 2,
            "should land on the `return M` statement (line 2), got: {:?}",
            loc.range,
        );
    } else {
        panic!("expected scalar goto response, got {:?}", result);
    }
}

#[test]
fn goto_require_with_attribute_before_target() {
    // Regression: `attribute_name_list` interleaves identifier and
    // attribute children, so `local x <const>, y = require(...)` used
    // to pick `values.named_child(2)` for `y` (off-by-attribute) and
    // miss the require goto entirely. After fix, clicking `y` must
    // still jump to the required module.
    use mylua_lsp::{document::Document, summary_builder};
    use mylua_lsp::util::LuaSource;

    let mut parser = new_parser();

    let mod_src = "return { z = 1 }";
    let mod_uri = make_uri("attr_mod.lua");
    let mod_tree = parser.parse(mod_src.as_bytes(), None).unwrap();
    let mod_lua_source = LuaSource::new(mod_src.to_string());
    let mod_summary = summary_builder::build_file_analysis(&mod_uri, &mod_tree, mod_lua_source.source(), mod_lua_source.line_index()).0;

    // `y` is the *second* identifier in the names list but it corresponds
    // to `values.named_child(1)` (index 1 among expression values), not
    // index 2 (which is where `<const>` pushed it structurally).
    let caller_src = "local x <const>, y = 1, require(\"attr_mod\")";
    let caller_uri = make_uri("attr_caller.lua");
    let caller_tree = parser.parse(caller_src.as_bytes(), None).unwrap();
    let caller_lua_source = LuaSource::new(caller_src.to_string());
    let (caller_summary, caller_scope) =
        summary_builder::build_file_analysis(&caller_uri, &caller_tree, caller_lua_source.source(), caller_lua_source.line_index());
    let caller_doc = Document {
        lua_source: caller_lua_source,
        tree: caller_tree,
        scope_tree: caller_scope,
    };

    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    agg.set_require_mapping("attr_mod".to_string(), mod_uri.clone());
    agg.upsert_summary(mod_summary);
    agg.upsert_summary(caller_summary);

    // `y` is at column 17 in `local x <const>, y = ...`
    let result =
        goto::goto_definition(&caller_doc, &caller_uri, pos(0, 17), &mut agg, &GotoStrategy::Auto, &empty_docs())
            .expect("goto on `y` should resolve");

    if let tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc) = &result {
        assert_eq!(
            loc.uri, mod_uri,
            "y's require should target attr_mod.lua even with <const> attribute on x",
        );
        assert_eq!(
            loc.range.start.line, 0,
            "should land on `return ...` (line 0), got: {:?}",
            loc.range,
        );
    } else {
        panic!("expected scalar goto response, got {:?}", result);
    }
}

#[test]
fn goto_position_with_chinese_comment_on_same_line() {
    // Regression: LSP Position.character is UTF-16 code units. When a line
    // contains non-ASCII characters (e.g. Chinese) before the identifier,
    // the client sends a UTF-16 column that must be converted to the
    // correct byte offset internally, AND the returned Range.start.character
    // must also be in UTF-16 units.
    //
    // Line 0: `local 中文名 = 42  -- LHS identifier after `local ` and non-ASCII
    //   Unfortunately Lua identifiers are ASCII; use a comment instead:
    // Line 0: `-- 中文注释`
    // Line 1: `local myVar = 42`
    // Line 2: `print(myVar)  -- 后面的 myVar`
    //
    // The trailing `-- 后面的 myVar` contains non-ASCII BEFORE a word
    // literally spelled "myVar" but in a comment, so only tests the range
    // encoding. The real alignment test: hover on line 2 col 6 (utf-16)
    // which should be the exact start of `myVar` in `print(myVar)`.
    let src = "-- 中文注释\nlocal myVar = 42\nprint(myVar)";
    let (doc, uri, mut agg) = setup_single_file(src, "utf16.lua");

    // `myVar` in print(myVar) at line 2 col 6 (ASCII line, utf-16 == byte)
    let result = goto::goto_definition(&doc, &uri, pos(2, 6), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result.is_some(), "goto should resolve myVar");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 1, "myVar declared on line 1");
        // selection_range is the identifier `myVar` on line 1, starting at
        // column 6 in UTF-16 units (line is pure ASCII).
        assert_eq!(
            loc.range.start.character, 6,
            "selection range should be UTF-16 column 6, got {:?}",
            loc.range,
        );
    }
}

#[test]
fn semantic_token_columns_are_utf16_units() {
    // Ensure that when a line contains a non-ASCII character before a token,
    // the emitted semantic-token column counts UTF-16 code units, not bytes.
    use mylua_lsp::semantic_tokens;

    // Line 0: `local 中x = 1` — identifier `中x` starts at col 6 (after "local ").
    // In UTF-8 bytes, `中` is 3 bytes so `x` starts at byte col 9.
    // In UTF-16 code units, `中` is 1 unit so `x` starts at utf16 col 7.
    // tree-sitter emits the full identifier `中x` as one token (length 2 utf-16 units).
    //
    // Since Lua identifiers are officially ASCII, let's make the test with
    // a comment before an ASCII identifier on the SAME line:
    //   `a = 1 --中 b` where `b` is on a following line — not useful.
    // Instead, put an emoji in a string after a local declaration:
    let src = "local x = 1\nlocal y = \"👋\" local z = 2";
    let (doc, _uri, _agg) = setup_single_file(src, "sem.lua");
    let tokens = semantic_tokens::collect_semantic_tokens(
        doc.tree.root_node(),
        doc.source(),
        &doc.scope_tree,
        doc.line_index(),
    );
    // `z` on line 1 is AFTER `"👋"` (which is 4 UTF-8 bytes but 2 UTF-16 units).
    // Compute expected utf16 column for `z`: prefix on line 1 is
    // `local y = "👋" local z`, `z` is at byte col 21, utf-16 col 19.
    // We find the token whose length is 1 and assert its utf-16 column.
    let z_token = tokens
        .iter()
        .enumerate()
        .find(|(i, t)| {
            // `z` is the 3rd identifier token overall: x, y, z
            *i == 2 && t.length == 1
        })
        .map(|(_, t)| t.clone());
    assert!(
        z_token.is_some(),
        "expected a 1-char token for z; got tokens: {:?}",
        tokens,
    );
    // Sanity: sum delta_lines and delta_starts to reconstruct absolute column.
    let mut line = 0u32;
    let mut col = 0u32;
    for t in &tokens {
        if t.delta_line == 0 {
            col += t.delta_start;
        } else {
            line += t.delta_line;
            col = t.delta_start;
        }
        if t.length == 1 && line == 1 && col > 10 {
            // This is `z`. Expected UTF-16 column: prefix on line 1 is
            // `local y = "👋" local ` — the emoji is 2 UTF-16 code units,
            // so `z` sits at utf-16 column 21. If the server had emitted
            // byte columns instead, it would land at 23.
            assert_eq!(
                col, 21,
                "z should be at utf-16 col 21 on line 1, got {} (tokens: {:?})",
                col, tokens,
            );
            return;
        }
    }
    panic!("never saw z token; tokens={:?}", tokens);
}

#[test]
fn goto_nested_scope() {
    let src = r#"local outer = 1
do
    local inner = 2
    print(inner)
    print(outer)
end"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // `inner` at line 3, col 10 -> should go to line 2
    let result = goto::goto_definition(&doc, &uri, pos(3, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result.is_some(), "goto should find `inner` in nested scope");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 2, "inner defined on line 2");
    }

    // `outer` at line 4, col 10 -> should go to line 0
    let result2 = goto::goto_definition(&doc, &uri, pos(4, 10), &mut agg, &GotoStrategy::Auto, &empty_docs());
    assert!(result2.is_some(), "goto should find `outer` from parent scope");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result2 {
        assert_eq!(loc.range.start.line, 0, "outer defined on line 0");
    }
}

/// When a module does `return Player` (a global table with methods),
/// goto on `Player.new` in the caller should jump to the function
/// definition in the module file via `resolve_require_global_name`.
#[test]
fn goto_require_returning_global_table_method() {
    use mylua_lsp::{document::Document, summary_builder};
    use mylua_lsp::util::LuaSource;

    let mut parser = new_parser();

    // player.lua: global Player table with Player.new function
    let mod_src = r#"Player = {}

function Player.new(name)
    return { name = name }
end

return Player"#;
    let mod_uri = make_uri("player.lua");
    let mod_tree = parser.parse(mod_src.as_bytes(), None).unwrap();
    let mod_lua_source = LuaSource::new(mod_src.to_string());
    let (mod_summary, mod_scope) = summary_builder::build_file_analysis(&mod_uri, &mod_tree, mod_lua_source.source(), mod_lua_source.line_index());
    let _mod_doc = Document { lua_source: mod_lua_source, tree: mod_tree, scope_tree: mod_scope };

    // main.lua: require("player") and call Player.new("Alice")
    let caller_src = r#"local Player = require("player")
local hero = Player.new("Alice")"#;
    let caller_uri = make_uri("main.lua");
    let caller_tree = parser.parse(caller_src.as_bytes(), None).unwrap();
    let caller_lua_source = LuaSource::new(caller_src.to_string());
    let caller_summary = summary_builder::build_file_analysis(&caller_uri, &caller_tree, caller_lua_source.source(), caller_lua_source.line_index()).0;
    let (_, caller_scope) = summary_builder::build_file_analysis(&caller_uri, &caller_tree, caller_lua_source.source(), caller_lua_source.line_index());
    let caller_doc = Document {
        lua_source: caller_lua_source,
        tree: caller_tree,
        scope_tree: caller_scope,
    };

    let mut agg = mylua_lsp::aggregation::WorkspaceAggregation::new();
    agg.set_require_mapping("player".to_string(), mod_uri.clone());
    agg.upsert_summary(mod_summary);
    agg.upsert_summary(caller_summary);

    // Click on `new` in `Player.new("Alice")` — line 1, col 20
    let result = goto::goto_definition(
        &caller_doc, &caller_uri, pos(1, 20), &mut agg, &GotoStrategy::Auto, &empty_docs(),
    );
    assert!(
        result.is_some(),
        "goto on `new` in `Player.new(\"Alice\")` should resolve \
         (require returning global table)"
    );
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(
            loc.uri, mod_uri,
            "should jump to player.lua"
        );
        assert_eq!(
            loc.range.start.line, 2,
            "should land on `function Player.new(name)` (line 2), got: {:?}",
            loc.range,
        );
    }
}

#[test]
fn goto_emmy_comment_parent_type_name() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class ClassA1:BaseCls
ClassA1 = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_goto.lua");

    // Click `BaseCls` in `---@class ClassA1:BaseCls`.
    let result = goto::goto_definition(
        &doc,
        &uri,
        pos(3, 20),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    )
    .expect("goto on Emmy parent type should resolve");

    if let tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc) = result {
        assert_eq!(loc.range.start.line, 1, "should jump to BaseCls anchor");
    } else {
        panic!("expected scalar goto response");
    }
}

#[test]
fn goto_emmy_comment_description_word_does_not_resolve_as_type() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class ClassA1 @ BaseCls appears in docs only
ClassA1 = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_desc_goto.lua");

    // The description word `BaseCls` is not part of the type expression.
    let result = goto::goto_definition(
        &doc,
        &uri,
        pos(3, 20),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        result.is_none(),
        "description words in Emmy comments must not act as type references"
    );
}

#[test]
fn goto_emmy_comment_unmarked_description_words_do_not_resolve_as_types() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class OtherCls
OtherCls = {}

---@class Child:BaseCls OtherCls appears in docs only
Child = {}

---@class Holder
---@field x string BaseCls appears in docs only
Holder = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_unmarked_desc_goto.lua");

    let class_desc = goto::goto_definition(
        &doc,
        &uri,
        pos(6, 25),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        class_desc.is_none(),
        "unmarked @class description words must not act as type references"
    );

    let field_desc = goto::goto_definition(
        &doc,
        &uri,
        pos(10, 20),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        field_desc.is_none(),
        "unmarked @field description words must not act as type references"
    );
}

#[test]
fn goto_emmy_comment_skips_type_expression_names_that_are_not_types() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@class OtherCls
OtherCls = {}

---@overload fun(BaseCls: OtherCls): OtherCls
function f() end

---@class Holder
---@field [BaseCls] OtherCls
Holder = {}
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_type_expr_goto.lua");

    let param_name = goto::goto_definition(
        &doc,
        &uri,
        pos(6, 17),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        param_name.is_none(),
        "function type parameter names must not act as type references"
    );

    let key_type = goto::goto_definition(
        &doc,
        &uri,
        pos(10, 12),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    )
    .expect("bracket field key type should resolve");
    if let tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc) = key_type {
        assert_eq!(loc.range.start.line, 1, "should jump to BaseCls anchor");
    } else {
        panic!("expected scalar goto response");
    }
}

#[test]
fn goto_emmy_comment_string_literal_words_do_not_resolve_as_types() {
    let src = r#"---@class BaseCls
BaseCls = {}

---@type "BaseCls"
local literal = nil

---@type {["BaseCls"]: string}
local keyed = {}

---@type "\"BaseCls"
local escaped = nil
"#;
    let (doc, uri, mut agg) = setup_single_file(src, "emmy_comment_string_literal_goto.lua");

    let literal_word = goto::goto_definition(
        &doc,
        &uri,
        pos(3, 12),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        literal_word.is_none(),
        "string literal contents must not act as type references"
    );

    let key_word = goto::goto_definition(
        &doc,
        &uri,
        pos(6, 12),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        key_word.is_none(),
        "string table keys must not act as type references"
    );

    let escaped_word = goto::goto_definition(
        &doc,
        &uri,
        pos(9, 12),
        &mut agg,
        &GotoStrategy::Auto,
        &empty_docs(),
    );
    assert!(
        escaped_word.is_none(),
        "escaped quotes must not expose string literal contents as type references"
    );
}
