# MyLua 语法支持 — 进度记录

**日期**: 2026-05-10  
**状态**: P0/P1/P2 完成；P3 safe access/call 已收尾（含 standalone safe call statement 与链式 safe field access）；P3 named/spread args 第一阶段已收尾；P3 keyword-as-name 完整范围已收尾；top_keyword corpus/default 差异已收尾；P4 `$function` 已收尾；P5 `$string` parser/scanner 第一阶段已收尾；P6 LSP 基础能力稳定第一批已收尾（require/module completion、document link、signature help 的 `$string` 关键路径），第二批已收尾（hover/goto/references 的 AST 锚定 Emmy 解析与 semantic tokens `$string` smoke 覆盖）；下一步继续 P6 signature help/completion 细分场景或进入 P7/P8 前置 diagnostics

## 入口文档

- Spec: `docs/mylua-spec.md`
- Plan: `docs/superpowers/plans/2026-05-10-mylua-implementation-plan.md`

## 本轮已完成

### P0：测试闭环

- 新增 `grammar/test/corpus/mylua.txt`
  - 当前覆盖并通过：`continue`、`array_constructor`、`t[] = value`、safe access/call、链式 safe access、named/spread call arguments、keyword-as-name contexts（含 method/safe method call）、`$function`、`$string`
- 新增 `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
  - 已启用：低风险语法、`continue.mylua`、`array.mylua`、`func-named-args.mylua`、`dollarext.mylua`、safe access/call、safe access/call 作为 prefix expression 的组合、named/spread inline smoke test、keyword-as-name inline smoke test、`$function` / `$string` inline smoke test、keyword 在有歧义 name 位置的负向回归测试
  - 当前无 `#[ignore]` 的 MyLua parser fixture 测试

### P1：`.mylua` 后缀接入

已改：

- `vscode-extension/package.json`
  - 注册 `mylua` language id 与 `.mylua` 后缀
  - `.mylua` 复用现有 Lua TextMate grammar / semantic token scope
  - `mylua.workspace.include` 默认包含 `**/*.mylua`
- `vscode-extension/src/extension.ts`
  - `documentSelector` 同时匹配 `lua` / `mylua`
  - watcher 同时监听 `**/*.lua` / `**/*.mylua`
- `lsp/crates/mylua-lsp/src/config.rs`
  - 默认 include 改为 `['**/*.lua', '**/*.mylua']`
- `lsp/crates/mylua-lsp/src/workspace_scanner.rs`
  - 新增 `is_lua_like_path`
  - workspace scan 收集 `.lua` / `.mylua`
  - `file_path_to_module_name` strip `.lua` / `.mylua`
  - `init.mylua` 与 `init.lua` 一样映射为目录模块名
- `lsp/crates/mylua-lsp/src/handlers.rs`
  - watcher create/change 使用 `is_lua_like_path`

### P2：已完成的低风险 grammar 子集

已改：

- `grammar/grammar.js`
- `grammar/src/scanner.c`
- `grammar/src/parser.c`（由 `npx tree-sitter generate` 生成）

已支持：

- `continue_statement`
- `continue` 可作为 `goto continue` / `::continue::` 名称
- `array_constructor`: `[]`, `[1, nil, 3]`
- `t[] = value` parser 接受
- 函数定义参数尾逗号：`function f(a, b,) end`
- 函数调用参数尾逗号：`f(1, 2,)`
- number literal 支持 `_`
- `??` 作为 `binary_expression` operator

### P3：safe access/call、named/spread args 第一阶段与 keyword-as-name

已改：

- `grammar/grammar.js`
- `grammar/test/corpus/mylua.txt`
- `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/field_access.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/call_args.rs`
- `lsp/crates/mylua-lsp/src/util.rs`
- `lsp/crates/mylua-lsp/src/inlay_hint.rs`
- `lsp/crates/mylua-lsp/tests/test_diagnostics.rs`
- `lsp/crates/mylua-lsp/tests/test_inlay_hint.rs`

已支持：

- `obj?.field`
- `obj?.field?.nested`
- `obj?["key"]`
- `local v = obj?()`
- `local v = obj?:method(1)`
- standalone `obj?()` / `obj?:method(1)` function call statement
- safe `?.` / `?:` unknown-field diagnostics 降噪；普通 `.` / `:` 仍按原逻辑报告
- safe diagnostics 通过 AST 直接 operator token 判定，避免注释中的 `?` 误触发降噪
- `named_argument`: `f(a=1)`、`f(1, c=3, b=2)`
- `spread_argument`: `f(*args)`、`f(*[100], 13)`
- `extract_call_arg_nodes` 对 `named_argument` / `spread_argument` 返回 `value` 表达式，named value 可参与现有类型诊断
- 含 `spread_argument` 的调用暂时跳过参数数量/类型 diagnostics，避免第一阶段误报
- keyword-as-name 完整范围：field access、safe field access、table field key、function/method name、method call、safe method call、goto label、label declaration
- keyword-as-name 在 grammar 中将普通 `word_*` keyword alias 为 `identifier`，尽量减少 Rust analyzer 侧改动；不 alias `top_word_*`，避免削弱 top_keyword 错误恢复语义
- 普通变量名、参数名、局部声明名、local function name、numeric for variable 仍拒绝 keyword，并已有负向回归测试

### top_keyword corpus/default 差异收尾

已改：

- `grammar/test/corpus/statements.txt`
  - 普通 statements corpus 改为匹配 scanner 默认 `top_keyword_disabled = true` 的 `word_*` 期望
- `grammar/test/corpus/col0_error_recovery.txt`
- `grammar/test/corpus/top_level_keyword_split.txt`
  - 显式加入 `---#enable top_keyword`，保留顶层关键字错误前置恢复测试语义

### P4：`$function`

已改：

- `grammar/grammar.js`
  - 新增 `dollar_function` / `dollar_function_body`
  - `$function` 可作为 primary expression
  - 支持无括号调用参数：`consume ${ ... }` / `consume $(x){ ... }`
- `grammar/test/corpus/mylua.txt`
  - 新增 `MyLua dollar function` corpus，覆盖 `${...}`、`$(params){...}`、`$(...){...}`、尾逗号参数与无括号调用参数
- `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
  - 新增 `parse_mylua_dollar_function_syntax`
- `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs`
- `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`
- `lsp/crates/mylua-lsp/src/summary_builder/call_sites.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/type_compat.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/return_mismatch.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/param_annotation.rs`
- `lsp/crates/mylua-lsp/src/folding_range.rs`
  - 将 `dollar_function` 按普通匿名 `function_definition` 等价纳入类型推断、函数 summary、调用层级、参数/返回诊断锚点与折叠范围

### P5：`$string`

已改：

- `grammar/grammar.js`
  - 新增 `dollar_string` / `dollar_string_content` / `dollar_escape` / `dollar_name_interpolation` / `dollar_interpolation`
  - `$"..."` / `$'...'` 可作为 primary expression
  - 支持 `$$`、`$name`、`${expr}`，其中 `${expr}` 复用普通 expression AST
  - 支持无括号调用参数：`print $"hello $name"`
- `grammar/src/scanner.c`
  - 新增 `$string` 内容 token 扫描，在引号、裸 `$`、裸换行处停下
  - 复用短字符串转义扫描，避免 `$"if local end"` 内容被拆成 keyword/identifier
- `grammar/test/corpus/mylua.txt`
  - 新增 `MyLua dollar string` corpus，覆盖内容、`$name`、`$$`、`${format("world")}` 和无括号调用参数
- `grammar/test/corpus/col0_error_recovery.txt`
- `grammar/test/corpus/top_level_keyword_split.txt`
  - 同步 `$string` 引入后 error-recovery 快照中 ERROR 内 function call 的形态变化；顶层关键字前置恢复仍保留
- `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
  - 新增 `parse_mylua_dollar_string_syntax`
  - 启用 `parse_mylua_named_args_fixture`
  - 启用 `parse_mylua_dollar_extensions_fixture`
- `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs`
- `lsp/crates/mylua-lsp/src/type_inference.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/type_compat.rs`
  - 将 `dollar_string` 第一阶段按 string 类型处理
- `lsp/crates/mylua-lsp/src/util.rs`
  - `extract_string_literal` 显式跳过 `dollar_string`，避免 require/document link/module path 静态提取误用插值字符串
- `lsp/crates/mylua-lsp/tests/test_diagnostics.rs`
  - 新增 `$string` 作为 string 参与参数类型诊断的回归测试

### P6：LSP 基础能力稳定（第一批）

已改：

- `lsp/crates/mylua-lsp/src/completion.rs`
  - `require` module completion 从“任意祖先 `require` 调用中的字符串”收紧为“直接作为 `require` 参数的普通静态 `string`”
  - 修复 `$"... ${"..."} ..."` 插值内部普通字符串误触发 require/module completion 的问题
  - 保留普通 `require("")` 与 `require ""` 的模块补全行为
- `lsp/crates/mylua-lsp/tests/test_completion.rs`
  - 新增 `complete_require_path_ignores_string_nested_in_dollar_interpolation`
- `lsp/crates/mylua-lsp/tests/test_document_link.rs`
  - 新增 `document_link_ignores_dollar_string_require_argument`
- `lsp/crates/mylua-lsp/tests/test_signature_help.rs`
  - 新增 `signature_help_dollar_string_short_call_active_param_is_zero`
- `lsp/crates/mylua-lsp/src/emmy.rs`
  - 新增 `emmy_type_name_at_byte_in_range`，在 AST comment/emmy 节点范围内锚定解析 Emmy 类型名，避免同一物理行前序 `$string` 文本中的 `---` 干扰真实 trailing Emmy 注解
- `lsp/crates/mylua-lsp/src/util.rs`
  - 新增 `emmy_context_node_at`，用 AST 上下文门禁定位具体 `emmy_line` / `emmy_comment` / 短 Emmy `comment` 节点，并过滤普通/长注释
- `lsp/crates/mylua-lsp/src/hover.rs`
- `lsp/crates/mylua-lsp/src/goto.rs`
- `lsp/crates/mylua-lsp/src/references.rs`
  - `emmy_type_name_at_byte` 仅在 `emmy_line` / `emmy_comment` / `comment` AST 上下文内调用，避免 `$string` 内容中的 `---@type Foo` 文本误触发类型 hover/goto/references
  - `references` 的 raw-word fallback 同样限制在 Emmy/comment 上下文，避免 `$string` 普通文本被当成类型/全局引用
- `lsp/crates/mylua-lsp/tests/test_hover.rs`
  - 新增 `hover_dollar_string_emmy_like_text_does_not_resolve_as_type`
- `lsp/crates/mylua-lsp/tests/test_goto.rs`
  - 新增 `goto_dollar_string_emmy_like_text_does_not_resolve_as_type`
- `lsp/crates/mylua-lsp/tests/test_references.rs`
  - 新增 `references_dollar_string_plain_text_words_do_not_resolve_as_types`
- `lsp/crates/mylua-lsp/tests/test_semantic_tokens_range.rs`
  - 新增 `semantic_tokens_dollar_string_interpolation_smoke`

本轮已继续：

- `$string` parser/scanner 第一阶段已完成，P0 中两个被 P5 阻塞的 fixture parser tests 已启用。
- P6 第一批已完成：module completion 不再把 `$string` 插值内部的普通字符串当成 require 静态路径；document link 明确忽略动态 `$string` require 参数；signature help 验证 `foo $"..."` 无括号调用保持 active parameter = 0。
- P6 第二批已完成：hover/goto/references 不再把 `$string` 内容或 `${"..."}` 内普通字符串中的 `---@type BaseCls` 文本当成 Emmy 类型注解；真实 trailing Emmy 类型注解仍可解析。
- code review 后补齐边界：同一行 `$string` 假 `---@type FakeCls` 不再遮蔽后续真实 trailing `---@type BaseCls`；references 不再收集 Emmy 描述区、普通注释和长注释中的同名伪引用。
- semantic tokens 已补 `$name` 与 `${name}` 插值 smoke 覆盖，确认 `$string` 普通内容不产 token、插值 identifier 仍产 token。
- `docs/future-work.md` 已移除完成的 `emmy_type_name_at_byte` 无 AST 上下文条目。

## 当前验证结果

最近一次验证命令：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test -p mylua-lsp --test test_completion complete_require_path_ignores_string_nested_in_dollar_interpolation -- --nocapture
cargo test -p mylua-lsp --test test_completion -- --nocapture
cargo test -p mylua-lsp --test test_document_link document_link_ignores_dollar_string_require_argument -- --nocapture
cargo test -p mylua-lsp --test test_signature_help signature_help_dollar_string_short_call_active_param_is_zero -- --nocapture
cargo test -p mylua-lsp --test test_completion --test test_document_link --test test_signature_help
rustfmt crates/mylua-lsp/src/completion.rs crates/mylua-lsp/tests/test_completion.rs crates/mylua-lsp/tests/test_document_link.rs crates/mylua-lsp/tests/test_signature_help.rs
```

结果：

- 新增 RED 验证：`complete_require_path_ignores_string_nested_in_dollar_interpolation` 初次失败，确认 `$string` 插值内部普通字符串会误触发 require module completion；修复后 passed。
- `cargo test -p mylua-lsp --test test_completion -- --nocapture`: passed（12 passed）
- `cargo test -p mylua-lsp --test test_document_link document_link_ignores_dollar_string_require_argument -- --nocapture`: passed（1 passed）
- `cargo test -p mylua-lsp --test test_signature_help signature_help_dollar_string_short_call_active_param_is_zero -- --nocapture`: passed（1 passed）
- `cargo test -p mylua-lsp --test test_completion --test test_document_link --test test_signature_help`: passed（12 + 8 + 16 passed）
- `rustfmt` 仅作用于本轮 4 个 Rust 修改文件，passed
- `read_lints`：`completion.rs`、`test_completion.rs`、`test_document_link.rs`、`test_signature_help.rs` 无诊断
- 本轮未重复运行 grammar 侧 `npx tree-sitter generate/test`；上轮记录仍为 `generate` passed、`tree-sitter test` 55/55 passed。

## 当前工作区变更

```text
 M docs/future-work.md
 M docs/superpowers/progress/2026-05-10-mylua-grammar-progress.md
 M lsp/crates/mylua-lsp/src/emmy.rs
 M lsp/crates/mylua-lsp/src/goto.rs
 M lsp/crates/mylua-lsp/src/hover.rs
 M lsp/crates/mylua-lsp/src/references.rs
 M lsp/crates/mylua-lsp/src/util.rs
 M lsp/crates/mylua-lsp/tests/test_goto.rs
 M lsp/crates/mylua-lsp/tests/test_hover.rs
 M lsp/crates/mylua-lsp/tests/test_references.rs
 M lsp/crates/mylua-lsp/tests/test_semantic_tokens_range.rs
```

注意：`docs/README.md` 不记录 `docs/superpowers` 内容，已保持不修改。

## 下次继续建议

优先继续 P6 / 后续语义稳定子任务：

1. P6 LSP 基础能力稳定
   - signature help 继续补 `${expr}` 内部普通调用、named/spread 场景的定位回归
   - completion 继续补普通 `$string` 内容区不误触发大量无关补全的策略评估；require/module completion 的 `$string` 动态路径误触发已修复
   - hover / goto / references 已加 Emmy AST 上下文门禁；后续如遇普通 string/long string 中类似误触发，可复用同一门禁策略补测试
   - document link 已确认忽略 `$string` require 参数；后续如进入 P8 常量折叠，再单独设计无插值 `$string` 的静态路径策略
2. `$string` 专属 diagnostics（P7/P8 前置）
   - `${expr}` 不允许跨物理行
   - `${expr}` 内不支持嵌套 `dollar_string`
   - `$` 后只允许 `$`、Name 或 `{`
3. named/spread 后续语义增强
   - argument count/type diagnostics 按 named 参数名匹配
   - signature help / inlay hints 对 named/spread 做精确定位

## 重要注意事项

- `parse_mylua_named_args_fixture` 与 `parse_mylua_dollar_extensions_fixture` 已启用；后续如果新增 MyLua fixture，应默认纳入 parser smoke test。
- `t[]` 当前 AST 没有显式 `empty_index` 节点，只是 `variable` 缺少 `index` field；Rust analyzer 后续读取 index 时必须允许 `None`。
- `dollar_string` 第一阶段已显式跳过 `extract_string_literal`；后续除非做 P8 常量折叠，否则不要接入 require / document link / module completion 静态路径解析。
- require/module completion 当前只允许直接作为 `require` 参数的普通静态 `string` 触发；不要重新放宽为“任意 require 祖先下的字符串”，否则 `$"... ${"..."} ..."` 会误触发。
- `$name` 插值当前复用外部 `identifier` token；scanner 会在裸 `$` 处停下，非法 `$` 形态留给后续 diagnostics。
- hover/goto/references 不应直接调用纯字节级 `emmy_type_name_at_byte`；入口应先用 `emmy_context_node_at` 定位 AST Emmy 节点，再用 `emmy_type_name_at_byte_in_range` 按节点范围锚定解析。
- references 的 raw-word fallback 只能用于 Emmy/comment 上下文；references 收集侧也要用结构化 Emmy 解析确认候选，避免描述区、普通注释、长注释伪引用。
- named/spread 第一阶段只做 parser + 保守 diagnostics；后续如要精确语义，需按参数名重做 call args/signature help/inlay hints 匹配。
- 不要运行全仓库 `cargo fmt`，会产生大量无关格式化 diff；如需格式化，限制在本次修改文件。
