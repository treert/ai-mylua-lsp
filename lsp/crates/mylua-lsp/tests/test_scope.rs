mod test_helpers;

use mylua_lsp::uri_id::intern_uri;
use test_helpers::*;

fn resolve_name(src: &str, line: u32, col: u32) -> Option<String> {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let uri = make_uri("test.lua");
    let offset = doc
        .line_index()
        .position_to_byte_offset(doc.source(), pos(line, col))?;
    let ident = mylua_lsp::util::find_node_at_position(doc.root_node().unwrap(), offset)?;
    let name = mylua_lsp::util::node_text(ident, doc.source());
    doc.scope_tree
        .resolve_id(offset, name, intern_uri(&uri))
        .map(|d| format!("{}: {}", d.name, kind_str(&d.kind)))
}

fn kind_str(k: &mylua_lsp::types::DefKind) -> &'static str {
    match k {
        mylua_lsp::types::DefKind::LocalVariable => "local",
        mylua_lsp::types::DefKind::LocalFunction => "local_fn",
        mylua_lsp::types::DefKind::Parameter => "param",
        mylua_lsp::types::DefKind::ForVariable => "for_var",
        mylua_lsp::types::DefKind::GlobalVariable => "global",
        mylua_lsp::types::DefKind::GlobalFunction => "global_fn",
    }
}

#[test]
fn resolve_local_in_function_body() {
    let src = r#"local createJson = function ()
local json = {}
json.EMPTY_ARRAY = {}
end
"#;
    // "json" at line 2 col 0 (in `json.EMPTY_ARRAY`)
    let result = resolve_name(src, 2, 0);
    assert_eq!(result.as_deref(), Some("json: local"));
}

#[test]
fn resolve_local_at_declaration_site() {
    let src = r#"local createJson = function ()
local json = {}
end
"#;
    // "json" at line 1 col 6
    let result = resolve_name(src, 1, 6);
    assert_eq!(result.as_deref(), Some("json: local"));
}

#[test]
fn resolve_parameter() {
    let src = r#"local function foo(x, y)
    return x + y
end
"#;
    // "x" at line 1 col 11
    let result = resolve_name(src, 1, 11);
    assert_eq!(result.as_deref(), Some("x: param"));
}

#[test]
fn resolve_for_numeric_variable() {
    let src = r#"for i = 1, 10 do
    print(i)
end
"#;
    // "i" inside the loop body at line 1 col 10
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("i: for_var"));
}

#[test]
fn resolve_for_generic_variable() {
    let src = r#"for k, v in pairs(t) do
    print(k, v)
end
"#;
    // "k" at line 1 col 10
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("k: for_var"));
    // "v" at line 1 col 13
    let result_v = resolve_name(src, 1, 13);
    assert_eq!(result_v.as_deref(), Some("v: for_var"));
}

#[test]
fn shadowing_inner_scope() {
    let src = r#"local x = 1
do
    local x = 2
    print(x)
end
print(x)
"#;
    // "x" at line 3 col 10 → inner local (line 2)
    let inner = resolve_name(src, 3, 10);
    assert_eq!(inner.as_deref(), Some("x: local"));

    // "x" at line 5 col 6 → outer local (line 0)
    let outer = resolve_name(src, 5, 6);
    assert_eq!(outer.as_deref(), Some("x: local"));
}

#[test]
fn nested_function_body_locals() {
    let src = r#"local function outer()
    local a = 1
    local function inner()
        local b = 2
        print(a, b)
    end
end
"#;
    // "a" at line 4 col 14 → outer's local
    let result_a = resolve_name(src, 4, 14);
    assert_eq!(result_a.as_deref(), Some("a: local"));
    // "b" at line 4 col 17 → inner's local
    let result_b = resolve_name(src, 4, 17);
    assert_eq!(result_b.as_deref(), Some("b: local"));
}

#[test]
fn unresolved_global() {
    let src = r#"print(undefined_var)
"#;
    // "undefined_var" at line 0 col 6
    let result = resolve_name(src, 0, 6);
    assert_eq!(result, None);
}

#[test]
fn self_in_colon_method() {
    let src = r#"function Foo:bar()
    print(self)
end
"#;
    // "self" at line 1 col 10
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("self: param"));
}

#[test]
fn local_rhs_sees_outer_scope() {
    let src = r#"local x = 1
local x = x + 1
print(x)
"#;
    // "x" in RHS of second declaration (line 1, col 10 → the `x` in `x + 1`)
    // should resolve to the OUTER x (line 0), not the current declaration
    let result = resolve_name(src, 1, 10);
    assert_eq!(result.as_deref(), Some("x: local"));

    // After the second declaration (line 2), `x` should resolve to the inner one
    let result_after = resolve_name(src, 2, 6);
    assert_eq!(result_after.as_deref(), Some("x: local"));
}

#[test]
fn visible_locals_completeness() {
    let src = r#"local a = 1
local function foo(x)
    local b = 2
    do
        local c = 3
        print()
    end
end
"#;
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    // At line 5 col 8 (inside do block, at "print()")
    let offset = doc
        .line_index()
        .position_to_byte_offset(doc.source(), pos(5, 14))
        .unwrap();
    let locals = doc.scope_tree.visible_locals(offset);
    let names: Vec<&str> = locals.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"a"), "should see outer 'a': {:?}", names);
    assert!(names.contains(&"x"), "should see param 'x': {:?}", names);
    assert!(names.contains(&"b"), "should see 'b': {:?}", names);
    assert!(names.contains(&"c"), "should see inner 'c': {:?}", names);
    assert!(names.contains(&"foo"), "should see 'foo': {:?}", names);
}

// ---------------------------------------------------------------------------
// if / elseif / else branch isolation
//
// The three branch bodies are sibling scopes under a declaration-free
// `IfStatement` shell, so a `local` in one branch is invisible in the
// others. Conditions live in the shell, outside every branch.
// ---------------------------------------------------------------------------

#[test]
fn else_branch_cannot_see_then_branch_local() {
    let src = r#"if cond then
    local a = 1
    print(a)
else
    print(a)
end
"#;
    // Inside `then`: resolves to the branch-local `a`.
    assert_eq!(resolve_name(src, 2, 10).as_deref(), Some("a: local"));
    // Inside `else`: the `then` branch's `a` is out of scope.
    assert_eq!(resolve_name(src, 4, 10), None);
}

#[test]
fn then_branch_cannot_see_else_branch_local() {
    let src = r#"if cond then
    print(b)
else
    local b = 2
    print(b)
end
"#;
    assert_eq!(resolve_name(src, 1, 10), None);
    assert_eq!(resolve_name(src, 4, 10).as_deref(), Some("b: local"));
}

#[test]
fn elseif_branches_are_mutually_isolated() {
    let src = r#"if c1 then
    local x = 1
elseif c2 then
    print(x)
    local y = 2
elseif c3 then
    print(y)
else
    print(x)
end
"#;
    // First `elseif` cannot see the `then` branch's `x`.
    assert_eq!(resolve_name(src, 3, 10), None);
    // Second `elseif` cannot see the first `elseif`'s `y`.
    assert_eq!(resolve_name(src, 6, 10), None);
    // `else` cannot see the `then` branch's `x` either.
    assert_eq!(resolve_name(src, 8, 10), None);
}

#[test]
fn branch_local_is_invisible_after_the_if_statement() {
    let src = r#"local outer = 1
if cond then
    local inner = 2
end
print(inner)
print(outer)
"#;
    assert_eq!(resolve_name(src, 2, 10).as_deref(), Some("inner: local"));
    assert_eq!(resolve_name(src, 4, 6), None);
    // The enclosing scope is still reachable through the shell.
    assert_eq!(resolve_name(src, 5, 6).as_deref(), Some("outer: local"));
}

#[test]
fn condition_resolves_in_enclosing_scope_not_the_branch() {
    let src = r#"local guard = 1
if guard then
    local guard = 2
    print(guard)
end
"#;
    // The condition sits in the shell, so it sees the outer `guard`; the
    // shadowing declaration inside `then` must not leak backwards into it.
    assert_eq!(resolve_name(src, 1, 3).as_deref(), Some("guard: local"));
    assert_eq!(resolve_name(src, 3, 10).as_deref(), Some("guard: local"));

    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);
    let cond_offset = doc
        .line_index()
        .position_to_byte_offset(doc.source(), pos(1, 3))
        .unwrap();
    let cond_decl = doc.scope_tree.resolve_decl(cond_offset, "guard").unwrap();
    let body_offset = doc
        .line_index()
        .position_to_byte_offset(doc.source(), pos(3, 10))
        .unwrap();
    let body_decl = doc.scope_tree.resolve_decl(body_offset, "guard").unwrap();
    assert_ne!(
        cond_decl.decl_byte, body_decl.decl_byte,
        "condition must bind the outer `guard`, not the branch-local one"
    );
}

#[test]
fn nested_if_inside_branch_still_resolves_outward() {
    let src = r#"local a = 1
if c1 then
    local b = 2
    if c2 then
        local c = 3
        print(a, b, c)
    end
end
"#;
    assert_eq!(resolve_name(src, 5, 14).as_deref(), Some("a: local"));
    assert_eq!(resolve_name(src, 5, 17).as_deref(), Some("b: local"));
    assert_eq!(resolve_name(src, 5, 20).as_deref(), Some("c: local"));
}

#[test]
fn branch_local_visible_on_trailing_blank_line_of_its_branch() {
    // tree-sitter ends a clause at its last statement; the branch scope is
    // deliberately extended to the next clause / `end` so completion still
    // sees branch locals while the cursor rests on a trailing blank line.
    let src = "if cond then\n    local a = 1\n\nelse\n    local b = 2\n\nend\n";
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, src);

    let then_tail = doc
        .line_index()
        .position_to_byte_offset(doc.source(), pos(2, 0))
        .unwrap();
    let then_names: Vec<&str> = doc
        .scope_tree
        .visible_locals(then_tail)
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert!(
        then_names.contains(&"a"),
        "then-branch tail should still see 'a': {:?}",
        then_names
    );
    assert!(
        !then_names.contains(&"b"),
        "then-branch tail must not see else's 'b': {:?}",
        then_names
    );

    let else_tail = doc
        .line_index()
        .position_to_byte_offset(doc.source(), pos(5, 0))
        .unwrap();
    let else_names: Vec<&str> = doc
        .scope_tree
        .visible_locals(else_tail)
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert!(
        else_names.contains(&"b"),
        "else-branch tail should still see 'b': {:?}",
        else_names
    );
    assert!(
        !else_names.contains(&"a"),
        "else-branch tail must not see then's 'a': {:?}",
        else_names
    );
}
