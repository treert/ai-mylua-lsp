# Global LuaSymbol Interning — 进度记录

**日期**: 2026-05-05
**状态**: 代码迁移与验证已完成；大工作区 RSS 实测尚未执行

## 目标

用全局字符串驻留池降低大型工作区内存占用。实现方式是引入 `LuaSymbol(Spur)`，把长期驻留索引结构里的重复 Lua 名称字符串替换为 compact symbol。

迁移范围：

- `WorkspaceAggregation` / `GlobalShard`
- `DocumentSummary`
- `ScopeTree`
- `TypeFact`
- `TableShape`

不迁移请求级临时文本，例如 hover markdown、diagnostic message、completion label/detail、signature help label、index status message、CLI/config 字符串。

计划文档：`docs/superpowers/plans/2026-05-05-global-lua-symbol-interning.md`

## 已完成摘要

已提交：

```text
cf7fefb refactor: add LuaSymbol infrastructure
9a7bbd0 refactor: intern core type and table names
9e1d1af refactor: intern summary names
3793623 refactor: intern scope and aggregation names
1832ba6 refactor: tighten summary builder symbol boundaries
d00e9f2 refactor: avoid interning query misses
```

完成内容：

- 新增 `LuaSymbol` 基础设施，统一通过全局 `ThreadedRodeo` intern/resolve。
- 将 `TypeFact`、`TableShape`、`DocumentSummary`、`ScopeTree`、`WorkspaceAggregation` 等长期驻留结构的 Lua 名称字段迁移为 `LuaSymbol`。
- 保持 LSP/UI/debug JSON 边界仍输出普通字符串。
- 收紧 summary builder 构造边界，减少先 resolve 回 `String` 再重新 intern 的路径。
- 增加非插入式查询边界：`get_lua_symbol`、`TableShape::get_field`，避免 lookup miss 把请求级临时字符串写入全局 interner。
- 补充回归测试覆盖 JSON 字符串输出、长期字段使用 `LuaSymbol`、lookup miss 不污染 symbol pool。

## 已验证

最近一次完整验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo run --bin lua-perf -- --summary-stdout /Users/zhuguosen/MyGit/ai-mylua-lsp/tests/lua-root/main.lua
cargo test --tests
cargo build
```

结果：

- `lua-perf --summary-stdout`: passed，summary JSON 中名称字段仍输出字符串
- `cargo test --tests`: 587 passed
- `cargo build`: passed，无 warning
- `ReadLints`: no linter errors
- `code-reviewer`: no blocking issues found

## 未完成 / 可选后续

原计划 Task 5 的大工作区 RSS 对比尚未执行：

- file count
- source bytes
- peak RSS after initial index Ready
- initial index wall time

如果有 2w 文件级 workspace，再采集 before/after 数据；只有拿到真实数据时才更新 `docs/performance-analysis.md`，不写推测数字。

