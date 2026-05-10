mod test_helpers;

use std::fs;
use std::path::PathBuf;

use test_helpers::new_parser;

fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn assert_source_parses(name: &str, source: &str) {
    let mut parser = new_parser();
    let tree = parser
        .parse(source.as_bytes(), None)
        .unwrap_or_else(|| panic!("parser returned None for {name}"));
    let root = tree.root_node();

    assert_eq!(root.kind(), "source_file");
    assert!(
        !root.has_error(),
        "{name} should parse without syntax errors:\n{}",
        root.to_sexp(),
    );
}

fn assert_fixture_parses(relative_path: &str) {
    let path = repo_root().join(relative_path);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    assert_source_parses(&path.display().to_string(), &source);
}

fn assert_source_has_error(name: &str, source: &str) {
    let mut parser = new_parser();
    let tree = parser
        .parse(source.as_bytes(), None)
        .unwrap_or_else(|| panic!("parser returned None for {name}"));
    let root = tree.root_node();

    assert_eq!(root.kind(), "source_file");
    assert!(
        root.has_error(),
        "{name} should keep rejecting keyword in ambiguous name positions:\n{}",
        root.to_sexp(),
    );
}

#[test]
fn parse_mylua_low_risk_syntax() {
    assert_source_parses(
        "low-risk MyLua syntax",
        r#"
function f(a, b,) end
f(1, 2,)
local n = 1_000_000
local x = value ?? default_value
assert((false ?? 1) == false)
"#,
    );
}

#[test]
fn parse_mylua_continue_fixture() {
    assert_fixture_parses("tests/lua-root/mylua/continue.mylua");
}

#[test]
fn parse_mylua_array_fixture() {
    assert_fixture_parses("tests/lua-root/mylua/array.mylua");
}

#[test]
fn parse_mylua_safe_access_and_call_syntax() {
    assert_source_parses(
        "safe access/call MyLua syntax",
        r#"
local field_value = obj?.field
local chained_value = obj?.field?.nested
local index_value = obj?["key"]
local call_value = obj?()
local method_value = obj?:method(1)
local field_call = obj?.field()
local safe_field_call = obj?.field?()
local call_field = obj?().field
local safe_index_chain = obj?["key"]?.nested
local combined = obj?.field ?? default_value
obj?()
obj?:method(1)
"#,
    );
}

#[test]
fn parse_mylua_named_and_spread_args_syntax() {
    assert_source_parses(
        "named/spread argument MyLua syntax",
        r#"
local function f(a, b, c) return a end
local args = {a = 1, b = 2, c = 3}
f(a=1, c=3, b=2)
f(11, c=33, *args)
f(b=11, *args, a=11, g())
f(*[100], 13)
"#,
    );
}

#[test]
fn parse_mylua_keyword_as_name_syntax() {
    assert_source_parses(
        "keyword-as-name MyLua syntax",
        r#"
local tb = { local = 1, end = 2 }
tb.end = 3
local x = tb.for
local y = tb?.return
tb:for()
tb?:return()
function tb:for() end
function tb.while() end
goto end
::end::
"#,
    );
}

#[test]
fn parse_mylua_rejects_keywords_in_ambiguous_name_positions() {
    for (name, source) in [
        ("local declaration name", "local end = 1"),
        ("function parameter", "function f(local) end"),
        ("numeric for variable", "for end = 1, 3 do end"),
        ("local function name", "local function while() end"),
    ] {
        assert_source_has_error(name, source);
    }
}

#[test]
#[ignore = "fixture still contains P5 dollar string/function syntax"]
fn parse_mylua_named_args_fixture() {
    assert_fixture_parses("tests/lua-root/mylua/func-named-args.mylua");
}

#[test]
#[ignore = "requires P5 dollar string/function grammar"]
fn parse_mylua_dollar_extensions_fixture() {
    assert_fixture_parses("tests/lua-root/mylua/dollarext.mylua");
}
