# MyLua 语法支持 — 进度记录

**日期**: 2026-05-10  
**状态**: P0/P1/P2 完成；P3 safe access/call 已收尾（含 standalone safe call statement 与链式 safe field access）；P3 named/spread args 第一阶段已收尾；top_keyword corpus/default 差异已收尾；下一步处理 keyword-as-name 完整范围或 `$function`

## 入口文档

- Spec: `docs/mylua-spec.md`
- Plan: `docs/superpowers/plans/2026-05-10-mylua-implementation-plan.md`

## 本轮已完成

### P0：测试闭环

- 新增 `grammar/test/corpus/mylua.txt`
  - 当前覆盖并通过：`continue`、`array_constructor`、`t[] = value`、safe access/call、链式 safe access、named/spread call arguments
- 新增 `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
  - 已启用：低风险语法、`continue.mylua`、`array.mylua`、safe access/call、safe access/call 作为 prefix expression 的组合、named/spread inline smoke test
  - 已保留但 `#[ignore]`：`func-named-args.mylua` fixture（仍包含 P5 `$string`）、`$string` / `$function` fixture

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

### P3：safe access/call 与 named/spread args 第一阶段

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

### top_keyword corpus/default 差异收尾

已改：

- `grammar/test/corpus/statements.txt`
  - 普通 statements corpus 改为匹配 scanner 默认 `top_keyword_disabled = true` 的 `word_*` 期望
- `grammar/test/corpus/col0_error_recovery.txt`
- `grammar/test/corpus/top_level_keyword_split.txt`
  - 显式加入 `---#enable top_keyword`，保留顶层关键字错误前置恢复测试语义

待继续：

- 可选：再支持链式 `obj?.field?.nested`

## 当前验证结果

最近一次验证命令：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/grammar
npx tree-sitter test --file-name mylua.txt   # TDD RED：实现前按预期失败
npx tree-sitter generate
npx tree-sitter test --file-name mylua.txt
npx tree-sitter test

cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test parse_mylua_safe_access_and_call_syntax --test test_mylua_parse -- --nocapture
cargo test plain_field_access_with_question_comment_still_reports_unknown_field --test test_diagnostics -- --nocapture   # TDD RED：修复前按预期失败，修复后通过
cargo test plain_method_call_with_question_comment_still_reports_unknown_field --test test_diagnostics -- --nocapture  # TDD RED：修复前按预期失败，修复后通过
cargo test safe_field_access_suppresses_unknown_field_diagnostic --test test_diagnostics -- --nocapture
cargo test safe_method_call_suppresses_unknown_field_diagnostic --test test_diagnostics -- --nocapture
cargo test --test test_mylua_parse --test test_diagnostics
```

结果：

- TDD RED：`npx tree-sitter test --file-name mylua.txt` 在实现前因 `obj?.field?.nested` 产生 `ERROR`，符合预期
- TDD RED：两个 `question_comment` diagnostics 回归测试在修复前均因 0 个 `Unknown field` 失败，符合预期；修复后均通过
- `npx tree-sitter generate`: passed
- `npx tree-sitter test --file-name mylua.txt`: 5/5 passed
- `npx tree-sitter test`: 52/52 passed（仅保留既有 external scanner non-static warning）
- `cargo test parse_mylua_safe_access_and_call_syntax --test test_mylua_parse -- --nocapture`: passed
- `cargo test safe_field_access_suppresses_unknown_field_diagnostic --test test_diagnostics -- --nocapture`: passed
- `cargo test safe_method_call_suppresses_unknown_field_diagnostic --test test_diagnostics -- --nocapture`: passed
- `cargo test --test test_mylua_parse --test test_diagnostics`: passed（`test_diagnostics`: 92/92 passed；`test_mylua_parse`: 5/5 passed, 2 ignored）
- 备注：最终完整 `cargo test` 曾尝试执行，但被当前执行环境跳过；本轮相关 Rust 集成测试已完成验证

## 当前工作区变更

```text
 M docs/superpowers/progress/2026-05-10-mylua-grammar-progress.md
 M grammar/grammar.js
 M grammar/test/corpus/mylua.txt
 M lsp/crates/mylua-lsp/src/diagnostics/field_access.rs
 M lsp/crates/mylua-lsp/tests/test_diagnostics.rs
 M lsp/crates/mylua-lsp/tests/test_mylua_parse.rs
```

注意：`docs/README.md` 不记录 `docs/superpowers` 内容，已保持不修改。

## 下次继续建议

优先继续 P3/P4/P5 中尚未完成的 parser 子任务，建议顺序：

1. keyword-as-name 完整范围
   - 当前仅为 `continue` label/goto 做了最小适配
2. `$function`
3. `$string` scanner mode
   - 完成后可启用 `parse_mylua_named_args_fixture`（该 fixture 当前仍包含 `$"..."`）
4. named/spread 后续语义增强
   - argument count/type diagnostics 按 named 参数名匹配
   - signature help / inlay hints 对 named/spread 做精确定位

## 重要注意事项

- `parse_mylua_named_args_fixture` 当前仍是 `#[ignore]`，原因是 fixture 包含 P5 `$string`；`parse_mylua_dollar_extensions_fixture` 也保持 `#[ignore]`。
- `t[]` 当前 AST 没有显式 `empty_index` 节点，只是 `variable` 缺少 `index` field；Rust analyzer 后续读取 index 时必须允许 `None`。
- `dollar_string` 第一阶段不要接入 `extract_string_literal` / require / document link。
- named/spread 第一阶段只做 parser + 保守 diagnostics；后续如要精确语义，需按参数名重做 call args/signature help/inlay hints 匹配。
- 不要运行全仓库 `cargo fmt`，会产生大量无关格式化 diff；如需格式化，限制在本次修改文件。
