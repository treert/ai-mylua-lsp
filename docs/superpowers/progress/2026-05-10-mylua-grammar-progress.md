# MyLua 语法支持 — 进度记录

**日期**: 2026-05-10  
**状态**: P0/P1/P2 完成；P3 safe access/call 已收尾（含 standalone safe call statement）；top_keyword corpus/default 差异已收尾；下一步处理 named/spread args 或 safe access 链式增强

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
- standalone `obj?()` / `obj?:method(1)` function call statement
- safe `?.` / `?:` unknown-field diagnostics 降噪；普通 `.` / `:` 仍按原逻辑报告

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
npx tree-sitter test --file-name statements.txt
npx tree-sitter test --file-name col0_error_recovery.txt
npx tree-sitter test --file-name top_level_keyword_split.txt
npx tree-sitter test
```

结果：

- `npx tree-sitter test --file-name statements.txt`: 20/20 passed
- `npx tree-sitter test --file-name col0_error_recovery.txt`: 4/4 passed
- `npx tree-sitter test --file-name top_level_keyword_split.txt`: 11/11 passed
- `npx tree-sitter test`: 51/51 passed
- 本轮仅调整 corpus 源输入/期望，未修改 parser/Rust 源码；上一轮 `npx tree-sitter generate` 与 `cargo test` 结果仍保持记录为 passed

## 当前工作区变更

```text
 M docs/superpowers/progress/2026-05-10-mylua-grammar-progress.md
 M grammar/test/corpus/col0_error_recovery.txt
 M grammar/test/corpus/statements.txt
 M grammar/test/corpus/top_level_keyword_split.txt
```

注意：`docs/README.md` 不记录 `docs/superpowers` 内容，已保持不修改。

## 下次继续建议

优先继续 P2/P3 中尚未完成的 parser 子任务，建议顺序：

1. named/spread args
   - `f(a=1)`
   - `f(*args)`
   - 启用 `parse_mylua_named_args_fixture`
2. safe access/call 可选增强
   - 可选：再支持链式 `obj?.field?.nested`
3. keyword-as-name 完整范围
   - 当前仅为 `continue` label/goto 做了最小适配
4. `$function`
5. `$string` scanner mode

## 重要注意事项

- `parse_mylua_named_args_fixture` 和 `parse_mylua_dollar_extensions_fixture` 当前是 `#[ignore]`，后续实现对应语法时再启用。
- `t[]` 当前 AST 没有显式 `empty_index` 节点，只是 `variable` 缺少 `index` field；Rust analyzer 后续读取 index 时必须允许 `None`。
- `dollar_string` 第一阶段不要接入 `extract_string_literal` / require / document link。
- 不要运行全仓库 `cargo fmt`，会产生大量无关格式化 diff；如需格式化，限制在本次修改文件。
