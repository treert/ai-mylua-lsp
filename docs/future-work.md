# Future Work — 后续可选增强

> 本文档收录 **目前稳定可用之上的后续增强项**。这些不是阻塞项——核心 LSP 能力已在生产水准，本清单面向的是 "有空时继续打磨" 的方向。
>
> 已完成项的完整历史见 git log（commit 消息按 P0/P1/P2 编号归类），或直接查阅 [`../ai-readme.md`](../ai-readme.md) 的「已实现 LSP 能力」章节。

---

## 0. 下一步待做（已确定设计、就差实现）

### [ ] AST 化链式字段赋值与读取

**背景**：当前 `summary_builder.rs::visit_assignment` 用 `full_text.splitn(2, '.')` 切割 LHS 文本来识别 `a.b = expr`，对 `a.b.c = expr` 会把 `"b.c"` 整个当作字段名错误地写到 `a` 的 shape 上（下游被 "找不到 → Unknown" 语义掩盖，表现上没 bug，但数据不对）。同时读取侧 `hover::infer_node_type` 只处理纯点号链，遇到 `foo().c` / `a[1].c` / `a:m().c` / `a.b:f().g().c` 就直接 `_ => Unknown`，链中间一旦出现 call/subscript，后续字段类型查不到。

**设计（已确认）**：

**写入侧（LHS shape 注册）** — 严格 bail：
- 沿 `object`/`field` 链走 AST，只支持**纯点号**链（任意深度：`a.b`、`a.b.c`、`a.b.c.d` …）
- 到达最内层 bare identifier → 如果是 local 且有 Table shape：
  - **按需创建** 中间缺失的嵌套 shape（`a.b.c = 1` 当 `a` 有 shape 但 `a.b` 还没 → 自动为 `a.b` 分配一个新 shape id）
  - 在最后一个字段设类型
- 中间遇到 **`function_call` / `subscript_expression` / `parenthesized_expression` / 任何非 `variable` 节点** → bail（不写入任何 shape）。这是**静态分析上正确的 no-op**：`a.b:f(arg)` 每次返回不同的临时值，给它的 `.c` 赋值并不归属于任何能通过名字再找到的 shape
- `function Foo:m() end` colon 方法声明不走 `visit_assignment`，不受影响

**读取侧（hover/goto/completion 查询）** — 全链支持：
- `hover::infer_node_type` 新增 `function_call` 分支：构造 `SymbolicStub::CallReturn { base, func_name }` 让 resolver 追踪返回值类型。可直接复用或抽取 `summary_builder::infer_call_return_type` 的 stub 构造逻辑
- `hover::infer_node_type` 新增 `variable` 的 subscript 分支（`object + index`）：返回 shape 的 `array_element_type`，或 Unknown
- 这样 `a.b:f(arg).g().c` 在 hover / goto / completion 里能正确追踪到 `.c` 的类型（只要中间每环的返回值类型能静态解出，比如 Emmy `@return` 声明、FunctionSummary、或已知的 class/shape）

**锚点**：
- 写入侧：`lsp/crates/mylua-lsp/src/summary_builder.rs::visit_assignment` 的 `"field_expression" | "variable"` 分支（`full_text.splitn(2, '.')` 处）
- 读取侧：`lsp/crates/mylua-lsp/src/hover.rs::infer_node_type`（`_ => TypeFact::Unknown` 默认分支）
- 参考：`summary_builder.rs::infer_call_return_type`（已有 method/dot call → CallReturn stub 的构造；可复用）

**测试思路**：
- `tests/test_hover.rs` / `tests/test_completion.rs` / `tests/test_goto.rs` 新增 fixture：
  - `a.b.c = 1` 写入 + `print(a.b.c)` 读取 → 正确类型（写入侧修完即命中）
  - `a.b.c.d = 1` 嵌套按需创建 → 读取 `a.b.c.d` 命中
  - `---@return Foo function make() end; local x = make().field` → 通过 CallReturn 链读取
  - `a.b:m().c.d`（带 class + `@field`/method 声明）→ 全链读取命中
  - 负面：`foo().c = 1` 不应该 pollute 任何 shape；`a.b[1].c = 1` 也不应 pollute
- `tests/test_diagnostics.rs`：写入侧 bail 后，原 `no_unknown_field_on_chained_lhs_assignment` 继续通过（仍不误报）
- 不回归：原 `a.b = expr` 单层 shape 字段设置仍正确

**关联历史**：
- 本次会话 commit `234b8df`（refactor: 死代码清理 + walk_ancestors）
- 上游审计报告见会话结尾附近"审计 #2：字符串切分讨巧"
- 设计对话：用户确认"写入侧 bail / 读取侧全链支持" 是正确方向（`a.b:f().c = expr` 运行时修改的是临时值，不归属任何持久 shape；但读取侧应能追踪）

**当前状态**：285 条测试全绿，无其他 pending 修改。下次会话开始时：
1. 读 `ai-readme.md` + 本节 + `docs/index-architecture.md` §3 建立上下文
2. 锚点文件列表：
   - `lsp/crates/mylua-lsp/src/summary_builder.rs`（写入侧 + `infer_call_return_type`）
   - `lsp/crates/mylua-lsp/src/hover.rs`（读取侧 `infer_node_type`）
   - `lsp/crates/mylua-lsp/src/resolver.rs`（`resolve_field_chain` / `resolve_stub` 处理 CallReturn）
   - `lsp/crates/mylua-lsp/src/table_shape.rs`（shape 数据结构 + `array_element_type`）
3. 先写失败测试（`a.b.c = 1` 后 hover `a.b.c` 期望命中），再改写入侧通过；然后读取侧同步

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
