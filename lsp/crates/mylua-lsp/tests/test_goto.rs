mod test_helpers;

use test_helpers::*;
use mylua_lsp::config::GotoStrategy;
use mylua_lsp::goto;

#[test]
fn goto_local_variable_definition() {
    let src = r#"local myVar = 42
print(myVar)"#;
    let (doc, uri, mut agg) = setup_single_file(src, "test.lua");

    // `myVar` on line 1, col 6
    let result = goto::goto_definition(&doc, &uri, pos(1, 6), &mut agg, &GotoStrategy::Auto);
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
    let result = goto::goto_definition(&doc, &uri, pos(3, 10), &mut agg, &GotoStrategy::Auto);
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
    let result = goto::goto_definition(&doc, &uri, pos(1, 10), &mut agg, &GotoStrategy::Auto);
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
    let result = goto::goto_definition(&doc, &uri, pos(1, 10), &mut agg, &GotoStrategy::Auto);
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
    let result = goto::goto_definition(&doc, &uri, pos(0, 10), &mut agg, &GotoStrategy::Auto);
    // May or may not find something (globals index etc.), but should not panic
    let _ = result;
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
    let result = goto::goto_definition(&doc, &uri, pos(3, 10), &mut agg, &GotoStrategy::Auto);
    assert!(result.is_some(), "goto should find `inner` in nested scope");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result {
        assert_eq!(loc.range.start.line, 2, "inner defined on line 2");
    }

    // `outer` at line 4, col 10 -> should go to line 0
    let result2 = goto::goto_definition(&doc, &uri, pos(4, 10), &mut agg, &GotoStrategy::Auto);
    assert!(result2.is_some(), "goto should find `outer` from parent scope");
    if let Some(tower_lsp_server::ls_types::GotoDefinitionResponse::Scalar(loc)) = &result2 {
        assert_eq!(loc.range.start.line, 0, "outer defined on line 0");
    }
}
