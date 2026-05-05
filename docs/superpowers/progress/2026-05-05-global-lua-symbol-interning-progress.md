# Global LuaSymbol Interning — 进度记录

**日期**: 2026-05-05
**状态**: Task 1 完成并已提交；Task 2+ 待拆分执行

## 当前目标

用全局字符串驻留池降低大型工作区内存占用。方案是引入 `LuaSymbol(Spur)`，内部封装全局 `lasso::ThreadedRodeo`，然后逐步把常驻结构里的重复 `String` 替换为 `LuaSymbol`。

只优化长期驻留内存的数据结构：

- `WorkspaceAggregation` / `GlobalShard`
- `DocumentSummary`
- `ScopeTree`
- `TypeFact`
- `TableShape`

不优化请求级临时字符串：

- hover markdown
- diagnostic message
- completion label/detail
- signature help label
- index status message
- CLI 参数和 config 字符串

## 关键文档

- Plan: `docs/superpowers/plans/2026-05-05-global-lua-symbol-interning.md`

## 已完成

### Task 1: Add LuaSymbol Infrastructure

提交：

```text
cf7fefb refactor: add LuaSymbol infrastructure
```

改动：

- `lsp/crates/mylua-lsp/Cargo.toml` 添加 `lasso = { version = "0.7.3", features = ["multi-threaded"] }`
- `lsp/Cargo.lock` 更新依赖锁定
- `lsp/crates/mylua-lsp/src/lua_symbol.rs` 新增：
  - `LuaSymbol(Spur)` newtype
  - private `OnceLock<ThreadedRodeo>`
  - `intern_lua_symbol(&str) -> LuaSymbol`
  - `resolve_lua_symbol(LuaSymbol) -> &'static str`
  - `LuaSymbol::as_str()`
  - `Debug`, `Display`, `From<&str>`, `Serialize`
  - 单元测试覆盖 interning、resolve、Display、Debug、From、JSON 序列化
- `lsp/crates/mylua-lsp/src/lib.rs` 导出 `pub mod lua_symbol;`

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test lua_symbol
cargo build
```

结果：

- `cargo test lua_symbol`: 7 passed
- `cargo build`: passed
- `ReadLints`: no linter errors
- `code-reviewer`: no issues found

## 当前仓库状态

换会话前检查：

```text
git status --short
git log -1 --oneline
```

最新提交：

```text
cf7fefb refactor: add LuaSymbol infrastructure
```

## 下一步建议

不要直接一次性执行原计划里的 Task 2，因为它同时覆盖 `TypeFact`、`TableShape`、`DocumentSummary`、`summary_builder`，替换量大。建议把 Task 2 拆成更小的可验证提交：

1. `TypeFact` / `FunctionSignature` / `ParamInfo` 里的长期字符串改为 `LuaSymbol`
2. `TableShape` / `FieldInfo` 字段名和 owner 改为 `LuaSymbol`
3. `DocumentSummary` 名称字段和 `function_name_index` 改为 `LuaSymbol`
4. `summary_builder` 构造边界统一 intern
5. `lua_perf --summary` 验证 JSON 仍输出字符串

每个小任务都应保持：

- 不运行 `cargo fmt` / `rustfmt`
- 只迁移常驻结构，不迁移 UI 临时字符串
- LSP 输出边界仍输出 `String`
- 验证至少包含 `cargo test --tests` 和 `cargo build`
- 完成后调用 code-reviewer

## 新会话起步提示

新会话建议先读：

1. `ai-readme.md`
2. `docs/README.md`
3. `docs/superpowers/plans/2026-05-05-global-lua-symbol-interning.md`
4. 本文件

然后从“拆分 Task 2”开始讨论，不要直接派发完整 Task 2。

