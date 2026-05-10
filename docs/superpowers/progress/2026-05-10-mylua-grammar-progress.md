# MyLua 语法支持 — 进度记录

**日期**: 2026-05-10  
**状态**: P0/P1/P2 完成；P3 safe access/call 表达式形态已接入；standalone safe call statement 待继续

## 入口文档

- Spec: `docs/mylua-spec.md`
- Plan: `docs/superpowers/plans/2026-05-10-mylua-implementation-plan.md`

## 本轮已完成

### P0：测试闭环

- 新增 `grammar/test/corpus/mylua.txt`
  - 当前覆盖并通过：`continue`、`array_constructor`、`t[] = value`
- 新增 `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
  - 已启用：低风险语法、`continue.mylua`、`array.mylua`
  - 已保留但 `#[ignore]`：named/spread args、`$string` / `$function` fixture

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

### P3：safe access/call 表达式形态

已改：

- `grammar/grammar.js`
- `grammar/test/corpus/mylua.txt`
- `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/field_access.rs`
- `lsp/crates/mylua-lsp/tests/test_diagnostics.rs`

已支持：

- `obj?.field`
- `obj?["key"]`
- `local v = obj?()`
- `local v = obj?:method(1)`
- safe `?.` / `?:` unknown-field diagnostics 降噪；普通 `.` / `:` 仍按原逻辑报告

待继续：

- standalone safe call statement：`obj?()` / `obj?:method()` 直接作为语句时，会影响 `top_level_keyword_split` 错误恢复 corpus，需单独处理。

## 当前验证结果

最近一次验证命令：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/grammar
npx tree-sitter generate
npx tree-sitter test --file-name mylua.txt
npx tree-sitter test

cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test
cargo test -p mylua-lsp --test test_mylua_parse parse_mylua_safe_access_and_call_syntax
cargo test -p mylua-lsp --test test_diagnostics safe_field_access_suppresses_unknown_field_diagnostic
cargo test -p mylua-lsp --test test_diagnostics safe_method_call_suppresses_unknown_field_diagnostic
```

结果：

- `npx tree-sitter generate`: passed
- `npx tree-sitter test --file-name mylua.txt`: 3/3 passed
- `cargo test`: passed
- `parse_mylua_safe_access_and_call_syntax`: passed
- `safe_field_access_suppresses_unknown_field_diagnostic`: passed
- `safe_method_call_suppresses_unknown_field_diagnostic`: passed
- `npx tree-sitter test`: failed，当前失败集中在 `top_level_keyword_split` 的错误恢复期望（引入 standalone safe call statement 时更明显；当前仍需继续排查 safe primary 对错误恢复成本的影响）
- 相关 `read_lints`: no new errors；`grammar/grammar.js` 仅有既有 hint（`EMMY_PREC` unused / CommonJS module）

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

优先继续 P2/P3 中尚未完成的 parser 子任务，建议顺序：

1. safe access/call 收尾
   - 排查 `npx tree-sitter test` 中 `top_level_keyword_split` 错误恢复回归
   - 在不破坏错误恢复的前提下支持 standalone `obj?()` / `obj?:method()` 语句
   - 可选：再支持链式 `obj?.field?.nested`
2. named/spread args
   - `f(a=1)`
   - `f(*args)`
   - 启用 `parse_mylua_named_args_fixture`
3. keyword-as-name 完整范围
   - 当前仅为 `continue` label/goto 做了最小适配
4. `$function`
5. `$string` scanner mode

## 重要注意事项

- `parse_mylua_named_args_fixture` 和 `parse_mylua_dollar_extensions_fixture` 当前是 `#[ignore]`，后续实现对应语法时再启用。
- `t[]` 当前 AST 没有显式 `empty_index` 节点，只是 `variable` 缺少 `index` field；Rust analyzer 后续读取 index 时必须允许 `None`。
- `dollar_string` 第一阶段不要接入 `extract_string_literal` / require / document link。
- 不要运行全仓库 `cargo fmt`，会产生大量无关格式化 diff；如需格式化，限制在本次修改文件。
