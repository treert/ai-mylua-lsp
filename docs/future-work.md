# Future Work — 后续可选增强

> 本文档收录 **目前稳定可用之上的后续增强项**。这些不是阻塞项——核心 LSP 能力已在生产水准，本清单面向的是 "有空时继续打磨" 的方向。
>
> 已完成项的完整历史见 git log（commit 消息按 P0/P1/P2 编号归类），或直接查阅 [`../ai-readme.md`](../ai-readme.md) 的「已实现 LSP 能力」章节。

---

## 1. 诊断扩展（P2-3 剩余子项）

**全部完成。** `duplicateTableKey` / `unusedLocal` / `argumentCountMismatch` / `argumentTypeMismatch` / `returnMismatch` 以及 `emmyTypeMismatch` 的 reassignment 扩展均已实现，每个都有独立的 `DiagnosticsConfig` 开关。

后三项默认 Off —— 第一次开启会在老代码上产生大量告警，按需 opt-in。启用路径：

```json
{
  "mylua.diagnostics.argumentCountMismatch": "warning",
  "mylua.diagnostics.argumentTypeMismatch": "warning",
  "mylua.diagnostics.returnMismatch": "warning"
}
```

实现要点见 `ai-readme.md` 的「语义诊断」条目和 `tests/test_diagnostics.rs`（32 条集成测试）。

---

## 2. selection_range / symbols 精细化

**全部完成。** `TypeDefinition.name_range` / `TypeFieldDef.name_range` 已加入 summary，并在 `symbols.rs` / `workspace_symbol.rs` 中替换 `selection_range` / `location.range`。`@field private name T` 里跳过 visibility 关键字后定位 name token。

---

## 3. signature_help 继续打磨

### [ ] `lookup_function_signatures_by_field` shape table 同名方法消歧

**背景**：P0-R3 已经通过移除 bare fallback 消除了"误拿同名 top-level 函数"的风险，但对于一个文件里两个 shape table 都有同名方法的场景（`{ m = function() end }` + `{ m = function() end }`），没有 owner_class 上下文时我们只能依赖 resolver 的 def_uri 区分。长期解决方案：`TableShape` 挂 owner 绑定名，从 `base_fact` 的 `LocalTypeFact.source` 反查。

**锚点**：`summary.rs::TableShape` / `summary_builder.rs::visit_assignment` / `visit_local_declaration`。

---

## 4. 其他低优先项

- `textDocument/prepareCallHierarchy` / `callHierarchy/incomingCalls` / `outgoingCalls`：从 `FunctionSummary` + `global_shard` 构造函数调用图
- ~~`textDocument/documentLink`：识别 `require("mod")` 里的 module path 作为可跳转链接~~ ✅ 已完成（paren + short-call 两种形态，别名调用 `m = require; m("x")` 不跟随）
- ~~`textDocument/foldingRange` 的 `elseif` / `else` 分支独立折叠~~ ✅ 已完成（外层 + if-branch + 每个 elseif/else 各一个 fold）
- 语义 tokens delta provider（当前只支持 full + range，delta 可进一步减小流量）
- `---@meta` 元文件支持（Lua-LS 习惯的 stub 文件约定）
- EmmyLua 类型表达式扩展：`fun(...)` 返回多值、`self` 泛型绑定（`---@diagnostic disable-next-line` 等已完成 ✅）

---

## 维护提示

- 任何新增诊断类别 → 在 `DiagnosticsConfig` 加字段 + 默认 severity；默认开启时需在 fixture 上跑一遍确认不产生大量噪声
- 任何新增 LSP capability → 在 `lib.rs::initialize` 的 `ServerCapabilities` 声明 + async handler；独立的 `src/<feature>.rs` 模块 + 集成测试
- 代码修改后按 `.cursor/rules/code-review-after-changes.mdc` 跑 code-reviewer
