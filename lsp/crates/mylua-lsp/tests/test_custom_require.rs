mod test_helpers;

use mylua_lsp::config::DiagnosticsConfig;
use mylua_lsp::diagnostics;
use mylua_lsp::document::DocumentStoreView;
use mylua_lsp::hover;
use test_helpers::*;
use tower_lsp_server::ls_types::HoverContents;

/// 模块 abc_mgr 的 Lua 源码（被所有测试共享）。
const ABC_MGR_SRC: &str = r#"local abc_mgr = {}

abc_mgr.name = "abc_mgr"
abc_mgr.version = "1.0.0"

function abc_mgr.init()
    print("init")
end

function abc_mgr.update()
    print("update")
end

function abc_mgr.get_name()
    return abc_mgr.name
end

function abc_mgr.test_print(...)
    print("abc_mgr", ...)
end

return abc_mgr
"#;

/// hover 单个位置，返回 markdown 文本（或空串）。
fn hover_markdown(
    docs: &std::collections::HashMap<UriId, mylua_lsp::document::Document>,
    agg: &mut mylua_lsp::aggregation::WorkspaceAggregation,
    uri: &tower_lsp_server::ls_types::Uri,
    line: u32,
    character: u32,
) -> String {
    let uri_id = intern_uri(uri);
    let doc = docs.get(&uri_id).expect("doc should exist");
    let store = DocumentStoreView::new(docs);
    hover::hover(doc, uri_id, pos(line, character), agg, &store)
        .map(|h| match h.contents {
            HoverContents::Markup(md) => md.value,
            HoverContents::Scalar(s) => match s {
                tower_lsp_server::ls_types::MarkedString::String(s) => s,
                tower_lsp_server::ls_types::MarkedString::LanguageString(ls) => ls.value,
            },
            HoverContents::Array(arr) => arr
                .into_iter()
                .map(|s| match s {
                    tower_lsp_server::ls_types::MarkedString::String(s) => s,
                    tower_lsp_server::ls_types::MarkedString::LanguageString(ls) => ls.value,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        })
        .unwrap_or_default()
}

fn assert_hover_contains(
    docs: &std::collections::HashMap<UriId, mylua_lsp::document::Document>,
    agg: &mut mylua_lsp::aggregation::WorkspaceAggregation,
    uri: &tower_lsp_server::ls_types::Uri,
    line: u32,
    character: u32,
    needle: &str,
) {
    let md = hover_markdown(docs, agg, uri, line, character);
    assert!(
        md.contains(needle),
        "hover at {}:{} expected to contain {:?}, got: {:?}",
        line,
        character,
        needle,
        md
    );
}

#[test]
fn custom_require_no_transform_resolves_module() {
    // 形态1：无变换规则，直接当 require 路径
    let main_src = r#"--- @customrequire param=module_name
function direct_require(module_name)
    return require(module_name)
end

local m = direct_require("module_abc.abc_mgr")
m.version
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // hover `m` 在 line 5, col 6 — 应解析为 abc_mgr 表类型
    assert_hover_contains(&docs, &mut agg, &main_uri, 5, 6, "table");
}

#[test]
fn custom_require_literal_replace_resolves_module() {
    // 形态2：字面量替换（核心样例）
    let main_src = r#"--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    local module = require(module_path)
    return module
end

local a = custom_require("mgr_abc.abc_mgr")
a.version
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // hover `a` 在 line 7, col 6 — 应解析为 abc_mgr 表类型（transform 后）
    assert_hover_contains(&docs, &mut agg, &main_uri, 7, 6, "table");
}

#[test]
fn custom_require_pattern_only_empty_template() {
    // 形态4：删除前缀。template 为空串。
    let main_src = r#"--- @customrequire param=module_name ^mgr_\.
function strip_prefix(module_name)
    return require(string.gsub(module_name, "^mgr_%.", ""))
end

local s = strip_prefix("mgr_.module_abc.abc_mgr")
s.version
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // hover `s` 在 line 5, col 6 — 应解析为 abc_mgr 表类型
    assert_hover_contains(&docs, &mut agg, &main_uri, 5, 6, "table");
}

#[test]
fn custom_require_silent_fallback_on_non_string_arg() {
    // 用例5：静默降级（实参不是字符串字面量）
    let main_src = r#"--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    return require(module_name)
end

local prefix = "mgr_abc.abc_mgr"
local b = custom_require(prefix)
b.nonexistent
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // `custom_require(prefix)` 中 prefix 是变量 → 降级 Unknown
    // hover `b`（line 7, col 6）应不包含 version（类型是 Unknown）
    let md = hover_markdown(&docs, &mut agg, &main_uri, 7, 6);
    assert!(
        !md.contains("version"),
        "non-string arg should fall back to Unknown, got: {:?}",
        md
    );
}

#[test]
fn custom_require_regex_compile_failure_silent() {
    // 用例6：regex 编译失败（[unclosed(）
    let main_src = r#"--- @customrequire param=module_name [unclosed(
function bad_regex(module_name)
    return require(module_name)
end

local x = bad_regex("anything")
x.foo
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // regex 编译失败 → 整个 custom_require 降级，hover 不 panic
    let _md = hover_markdown(&docs, &mut agg, &main_uri, 5, 6);
    // 不 panic 即通过
}

/// 收集单文件的 semantic diagnostics（用于诊断测试）。
fn collect_diags(src: &str, filename: &str) -> Vec<tower_lsp_server::ls_types::Diagnostic> {
    let (doc, uri, mut agg) = setup_single_file(src, filename);
    let diag_config = DiagnosticsConfig::default();
    diagnostics::collect_semantic_diagnostics_id(
        doc.root_node().unwrap(),
        src.as_bytes(),
        summary_id_by_uri(&agg, &uri),
        &mut agg,
        &doc.scope_tree,
        &diag_config,
        doc.line_index(),
    )
}

#[test]
fn custom_require_warning_on_regex_compile_failure() {
    // A: regex 编译失败 → Warning
    let src = r#"--- @customrequire param=module_name [unclosed(
function bad_regex(module_name)
    return require(module_name)
end
"#;
    let diags = collect_diags(src, "bad_regex.lua");
    let regex_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("regex pattern") && d.message.contains("failed to compile"))
        .collect();
    assert_eq!(
        regex_diags.len(),
        1,
        "expected 1 regex-compile-failure warning, got {:?}",
        diags
    );
    assert_eq!(
        regex_diags[0].severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::WARNING)
    );
}

#[test]
fn custom_require_warning_on_unknown_param_name() {
    // B: param_name 在参数列表找不到 → Warning
    let src = r#"--- @customrequire param=foo mgr_abc module_abc
function custom_require(module_name)
    return require(module_name)
end
"#;
    let diags = collect_diags(src, "unknown_param.lua");
    let param_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("@customrequire param 'foo'")
            && d.message.contains("does not match"))
        .collect();
    assert_eq!(
        param_diags.len(),
        1,
        "expected 1 param-mismatch warning, got {:?}",
        diags
    );
    assert_eq!(
        param_diags[0].severity,
        Some(tower_lsp_server::ls_types::DiagnosticSeverity::WARNING)
    );
}

#[test]
fn custom_require_no_warning_on_valid_annotation() {
    // 合法注解：param_name 匹配 + regex 合法 → 无 custom require 相关诊断
    let src = r#"--- @customrequire param=module_name ^mgr_abc module_abc
function custom_require(module_name)
    return require(module_name)
end
"#;
    let diags = collect_diags(src, "valid.lua");
    let cr_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("@customrequire"))
        .collect();
    assert!(
        cr_diags.is_empty(),
        "valid @customrequire should not produce diagnostics, got {:?}",
        cr_diags
    );
}

#[test]
fn custom_require_no_warning_when_no_transform() {
    // 无变换规则：无 regex，不应有 regex 失败诊断
    let src = r#"--- @customrequire param=module_name
function direct_require(module_name)
    return require(module_name)
end
"#;
    let diags = collect_diags(src, "no_transform.lua");
    let cr_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("@customrequire"))
        .collect();
    assert!(
        cr_diags.is_empty(),
        "no-transform @customrequire should not produce diagnostics, got {:?}",
        cr_diags
    );
}

