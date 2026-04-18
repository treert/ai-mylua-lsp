# Future Work — 后续可选增强

> 本文档收录 **目前稳定可用之上的后续增强项**。这些不是阻塞项——核心 LSP 能力已在生产水准，本清单面向的是 "有空时继续打磨" 的方向。
>
> 已完成项的完整历史见 git log（commit 消息按 P0/P1/P2 编号归类），或直接查阅 [`../ai-readme.md`](../ai-readme.md) 的「已实现 LSP 能力」章节。

---

## 1. 诊断扩展（P2-3 剩余子项）

P2-3 中 `duplicateTableKey` + `unusedLocal` 两项已完成；以下诊断尚未实现，每类都应有独立的 `DiagnosticsConfig` 开关（参考 `emmy_type_mismatch` 的模式）。

### [ ] 函数调用参数个数不匹配

**锚点**：`lsp/crates/mylua-lsp/src/diagnostics.rs`、`src/config.rs::DiagnosticsConfig`

**目标**：对 `foo(a, b)` 调用，若静态解析得到 `FunctionSummary.signature.params.len()` 确定（非 `vararg`），且实参数量 `!=` 形参数量 → 报告。`---@overload` 存在时允许多个重载之一匹配。

**陷阱**：
- `foo(t, ...)` 里 vararg 形参吸收任意多实参
- `foo()` 调用 `function foo(a, b) end` 时，`nil` 默认值在 Lua 里合法，行为是 warning 而非 error
- method call `obj:m(x)` 时 `self` 已被隐式传递，不应计入实参数

### [ ] 函数调用参数类型不匹配

**锚点**：同上

**目标**：emmy `@param x number` 声明下，实参静态类型可推断时（来自 `local_type_facts` / literal）若不兼容则报告。复用 `check_type_mismatch_diagnostics` 里的 `is_type_compatible`。

### [ ] `@return` 与实际 `return` 语句不匹配

**目标**：`---@return number` 声明下，函数体内 `return "x"` 报告类型不匹配；`---@return number, string` 声明下 `return 1` 报告数量不匹配。

**陷阱**：`return` 可以出现在嵌套 `if/do/while` 里，需要遍历整个 function_body 而非只看末尾。

### [ ] `---@type` 声明与后续赋值类型不匹配

**目标**：当前只 check `local` 首次字面量赋值（见 `diagnostics::find_local_rhs_type`）；`x = "new value"` 后续赋值若类型冲突也应报告。

**陷阱**：需要扩展 `diagnostics.rs` 遍历 `assignment_statement` 的 LHS 是否指向有 emmy 类型声明的 local。

---

## 2. selection_range / symbols 精细化

### [ ] `TypeDefinition` 增加 `name_range` 字段

**背景**：当前 `TypeDefinition.range` 指向 `@class Foo` 下一条 statement（`Foo = {}` 整行）；documentSymbol 的 CLASS 节点 `selection_range` 跟着粗粒度化，客户端点击 outline 会高亮整条语句而非 `Foo` 本身。

**实现提示**：`summary_builder.rs::visit_emmy_comment` 里解析 `@class <name>` 时记录 name token 的 range 到 `TypeDefinition.name_range`，`symbols.rs` 与 `workspace_symbol.rs` 的 Class 节点用 `name_range` 作 `selection_range`。

### [ ] `TypeFieldDef` 同名 range 精细化

与上一项同构：`@field x integer` 里 `x` 的 range，当前复用整条 emmy_line 的 range，可拆出精准列范围。

---

## 3. signature_help 继续打磨

### [ ] `lookup_function_signatures_by_field` shape table 同名方法消歧

**背景**：P0-R3 已经通过移除 bare fallback 消除了"误拿同名 top-level 函数"的风险，但对于一个文件里两个 shape table 都有同名方法的场景（`{ m = function() end }` + `{ m = function() end }`），没有 owner_class 上下文时我们只能依赖 resolver 的 def_uri 区分。长期解决方案：`TableShape` 挂 owner 绑定名，从 `base_fact` 的 `LocalTypeFact.source` 反查。

**锚点**：`summary.rs::TableShape` / `summary_builder.rs::visit_assignment` / `visit_local_declaration`。

---

## 4. 其他低优先项

- `textDocument/prepareCallHierarchy` / `callHierarchy/incomingCalls` / `outgoingCalls`：从 `FunctionSummary` + `global_shard` 构造函数调用图
- `textDocument/documentLink`：识别 `require("mod")` 里的 module path 作为可跳转链接
- `textDocument/foldingRange` 的 `elseif` / `else` 分支独立折叠
- 语义 tokens delta provider（当前只支持 full + range，delta 可进一步减小流量）
- `---@meta` 元文件支持（Lua-LS 习惯的 stub 文件约定）
- EmmyLua 类型表达式扩展：`fun(...)` 返回多值、`self` 泛型绑定、`---@diagnostic disable-next-line` 等

---

## 维护提示

- 任何新增诊断类别 → 在 `DiagnosticsConfig` 加字段 + 默认 severity；默认开启时需在 fixture 上跑一遍确认不产生大量噪声
- 任何新增 LSP capability → 在 `lib.rs::initialize` 的 `ServerCapabilities` 声明 + async handler；独立的 `src/<feature>.rs` 模块 + 集成测试
- 代码修改后按 `.cursor/rules/code-review-after-changes.mdc` 跑 code-reviewer
