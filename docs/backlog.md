# Backlog — 待办与实现指南

> 面向 **AI 新会话** 的可独立消费任务清单。每条都带 **目标 / 锚点代码位置 / 实现提示 / 陷阱 / 测试思路**，挑一条就能直接上手，无需再做整体代码走查。
>
> **使用方式**：
> 1. 先读 [`../ai-readme.md`](../ai-readme.md) 和 [`README.md`](README.md) 建立上下文；
> 2. 在下方挑一项标 `[ ]` 的 item，按其"锚点"打开对应文件；
> 3. 完成后把 `[ ]` 改成 `[x]`，更新 `ai-readme.md` 的"已实现 LSP 能力 / 测试矩阵"。
>
> **约定**：每项应至少带 1 条集成测试（放 `lsp/crates/mylua-lsp/tests/test_*.rs`），构建零新增 warning，之后按 `.cursor/rules/code-review-after-changes.mdc` 跑 code-reviewer。

## 项目当前状态（截至 commit `388bba0`）

- LSP 端支持：initialize/sync (Incremental + tree-sitter 增量 reparse) / 文档同步 / publishDiagnostics (syntax + 4 类 semantic) / documentSymbol / definition / hover / completion (含 Emmy tag & require path 补全) / references (含 Emmy 注解引用) / rename (带标识符校验) / workspace/symbol / semanticTokens (full) / **signatureHelp (含 @overload)**
- 位置编码：UTF-16（tree-sitter 边界转换）
- 并发：per-URI tokio mutex 串行化 did_open/did_change
- 测试：176 条全绿
- 文档同步：ai-readme.md 已反映所有已实现能力

---

## P0 残余 — 两轮 review 后的非 BLOCKING SUGGESTION

### [x] P0-R1 — `lookup_function_signatures_by_field` 跨文件 class 声明/实现分离时兜底漏 overload

**背景**：第二轮 reviewer 的 n1。当 `@class Foo` + `@field init fun(...)` 在 `a.lua`，而 `function Foo:init() end ---@overload ...` 实际在 `b.lua`，正常路径的 `resolved.def_uri = a.lua`，读 `summaries[a.lua].function_summaries["Foo:init"]` 会命中失败 → 只返回 `vec![sig]`，丢 `b.lua` 的 overloads。

**锚点**：`lsp/crates/mylua-lsp/src/signature_help.rs::lookup_function_signatures_by_field`（正常路径的 `for sep in [":", "."]` 循环之后、`return vec![sig.clone()]` 之前）。

**实现提示**：把"Unknown 分支走 global_shard"的逻辑也加到正常路径末尾：即便 resolver 已经给出 `Function(sig)`，若 owner_class 已知且 `{class}:{field}` 在 `global_shard` 的源 URI 不同，仍按 global_shard 定位到"真实实现文件"，从那里的 `function_summaries[qualified]` 拉 overloads。建议抽一个私有 `fn lookup_overloads_via_global_shard(cls, field, index) -> Option<Vec<FunctionSignature>>`，两个分支都调用。

**测试思路**（`tests/test_signature_help.rs`）：
```lua
-- a.lua
---@class Foo
---@field init fun(name: string): Foo
Foo = {}

-- b.lua
---@overload fun(n: number): Foo
function Foo:init() end

-- caller.lua
---@type Foo
local f = nil
f:init()
```
期望 signatures 里同时出现 `fun(name: string): Foo` 和 `fun(n: number): Foo`。

---

### [x] P0-R2 — `compute_active_parameter` 未终止的 `--[[` 多跑一轮

**背景**：第二轮 reviewer 的 n2。如果用户输入一半 `foo(a, --[[noteb)`，`--[[` 找不到 `]]`，`j` 停在 `slice.len()-1`，continue 后还会跑一次循环末 `i += 1`。没有死循环，仅多一轮 match。

**锚点**：`lsp/crates/mylua-lsp/src/signature_help.rs::compute_active_parameter` 的 `b'-'` 分支（`rest.starts_with(b"[[")` 里）。

**实现提示**：找不到 `]]` 时直接 `i = slice.len(); break;`。

**测试思路**：边界测试，`foo(a, --[[unclosed, b)` 位置在末尾，期望不 panic、active_parameter 合理（保持最后一次已数的 commas）。

---

### [x] P0-R3 — `lookup_function_signatures_by_field` 移除不安全的 bare `function_summaries.get(field_name)` 兜底

**背景**：第二轮 reviewer 的 n3。base 是 shape table（没有 emmy class），owner_class = None，只能走 bare `summary.function_summaries.get(field_name)`，同文件中两个 shape 都有同名方法会产生歧义。

**锚点**：同上文件。

**实现提示**：TableShape 里挂 owner name（需扩展 `DocumentSummary::table_shapes` 条目）。短期内可以先记录在 `summary_builder.rs::visit_assignment` / `visit_local_declaration` 的绑定名上，然后从 base_fact 的 `LocalTypeFact.source` 反查。优先级低，不常见。

---

### [ ] P0-R4 — `apply_text_edit` 早期 review 的 S6 剩余：`unwrap_or(text.len())` 语义更保守

**背景**：第 2 次 bug 批（commit `1ccb01e`）复审里我们的 n6：`position_to_byte_offset.unwrap_or(text.len())` 是更合适的默认，已经做了 clamp 到 EOF，功能 OK。无需改。**本条勾掉**。

---

## P1 新功能 — 常规 LSP 能力，用户能直接感知

### [x] P1-1 — `textDocument/foldingRange`

**目标**：代码折叠（`do ... end` / `function ... end` / `if ... end` / `for ... end` / `while ... end` / `repeat ... until` / `--[[ ... ]]` 长注释块 / 连续的 `---` 块）。

**锚点**：
- 新建 `lsp/crates/mylua-lsp/src/folding_range.rs`
- `lsp/crates/mylua-lsp/src/lib.rs`：`ServerCapabilities.folding_range_provider = Some(FoldingRangeProviderCapability::Simple(true))`；加 `async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>>`

**实现提示**：纯 Tree-sitter walk 即可。对每种 block 节点发 `FoldingRange { start_line, end_line, kind: Region }`。连续 `---` 注释聚合为一个 fold（kind: Comment）。注意行号要用 tree-sitter 的 row（0-indexed），LSP FoldingRange 也是 0-indexed line。

**测试思路**：
- `test_folding_range.rs`：fixture `function f() if x then for i=1,10 do end end end` 应该有 3 个嵌套区间；`---\n---\n---` 连续 3 行应产生 1 个 Comment fold；空文件返回空 Vec。

---

### [x] P1-2 — `textDocument/documentHighlight`

**目标**：光标悬停/放在某 identifier 时高亮当前文件里所有同义使用（read/write 区分可选）。

**锚点**：
- 新建 `lsp/crates/mylua-lsp/src/document_highlight.rs`
- `lib.rs`：`document_highlight_provider = Some(OneOf::Left(true))` + `async fn document_highlight(...)`

**实现提示**：复用 `references::find_references(..., include_declaration=true)` 但 **只取当前文件的结果**、转为 `DocumentHighlight { range, kind: Some(Read|Write) }`。Read/Write 判定：identifier 是否在 `assignment_statement.left` 子树里（类似 `diagnostics::is_assignment_target`），是则 Write 否则 Read。

**测试思路**：`local x = 1; x = 2; print(x)` 光标在任一 `x` 上应返回 3 个高亮，其中定义+第二次赋值为 Write、print 调用里为 Read。

---

### [ ] P1-3 — `textDocument/typeDefinition`

**目标**：LSP 里有 `definition`（跳到声明）和 `typeDefinition`（跳到其**类型**的声明）两个独立请求。对 `---@type Foo local x = ...` 里的 `x`，goto-definition 跳到 `local x`，但 goto-type-definition 应跳到 `---@class Foo` 处。

**锚点**：
- `lsp/crates/mylua-lsp/src/goto.rs`：抽出 `pub fn goto_type_definition(...)`
- `lib.rs`：`type_definition_provider = Some(TypeDefinitionProviderCapability::Simple(true))` + `async fn goto_type_definition(...)`

**实现提示**：先 `scope_tree.resolve_decl` 拿到 local 的 `decl_byte`，再从 `summary.local_type_facts` 取 `TypeFact`；如果是 `Known(EmmyType(name))` / `Known(EmmyGeneric(name, _))` / `Stub(TypeRef { name })`，就到 `index.type_shard[name]` 拿 range；否则 fall back 到 definition。

**测试思路**：一份文件有 `---@class Foo` + `---@type Foo local x`，typeDefinition on `x` 跳到 `@class Foo` 行。

---

### [ ] P1-4 — `documentSymbol` 展开 @class 层级

**目标**：VS Code outline 里看到 `@class Foo` 作为一个 CLASS 节点，`@field bar: integer` 与 `function Foo:baz()` 作为其子节点。

**锚点**：`lsp/crates/mylua-lsp/src/symbols.rs::collect_document_symbols`

**实现提示**：
1. 遍历 `emmy_comment` 提取 `@class` 及其后的 `@field` 聚合为 CLASS；从 summary（build 后）拿更准确。用 `DocumentSummary.type_definitions` 更直接——把 `TypeDefinition { name, kind=Class, fields, range }` 作 CLASS，每个 field 作 children。
2. 对 `function_declaration` 的 name 包含 `.` / `:` 时（如 `Foo.bar` / `Foo:baz`），挂到对应 class 的 children 下（若 class 存在）。
3. 去掉目前 `assignment_statement` 每条都变 symbol 的噪声（特别是 `x.foo = 1` 这种 LHS 带 `.` 的）；保留纯顶级全局赋值 `G = ...`。

**陷阱**：`DocumentSymbol.children: Option<Vec<DocumentSymbol>>` 需要嵌套；VS Code outline 按 `range` 排序展示。别忘了 selection_range 必须在 range 内。

**测试思路**：带 `@class Foo` + `@field x integer` + `function Foo:m()` 的 fixture，期望顶层有 1 个 CLASS `Foo`，其 children 含 `x`（FIELD）和 `m`（METHOD）。

---

### [ ] P1-5 — `workspace/symbol` 含 @class 成员

**目标**：全工作区模糊搜索里能搜到 `bar`（某 class 的 field）/ `baz`（某 class 的 method），container_name 填 class 名。

**锚点**：`lsp/crates/mylua-lsp/src/workspace_symbol.rs::search_workspace_symbols`

**实现提示**：在现有 `global_shard` + `type_shard` 扫描后，再额外遍历 `index.summaries.values()`，对每个 `TypeDefinition` 的 `fields` 发一条 `SymbolInformation { kind: FIELD, container_name: Some(class_name), ... }`。`function Foo:m()` 已在 global_shard 里名为 `"Foo:m"`——可把 `container_name` 置为 `Foo`、去掉前缀仅保留 `m` 显示。

**测试思路**：两个文件各有一个 class，搜 `ba` 能同时列出两个 class 的同名 `bar` 字段，`container_name` 分别为两个 class。

---

### [ ] P1-6 — rename 覆盖 Emmy 类型名 / 类成员（跨文件）

**目标**：rename `Foo` 时，把所有 `---@class Foo` / `---@type Foo` / `---@param x Foo` / `---@class Bar : Foo` 等注解里的 `Foo` 一起改。类成员（field / method）rename 同理。

**锚点**：`lsp/crates/mylua-lsp/src/rename.rs::rename`、`src/references.rs::find_references`（已支持 Emmy 类型引用扫描）

**实现提示**：rename 内部已经用 `references::find_references(..., include_declaration=true, strategy=Merge)`。再 P0.4 已让 references 返回注解里的引用位置。现在只需让这条链路确实触发：验证 rename 时 `name` 在 `type_shard`，走 references 的 Emmy 注解路径，产生跨文件 WorkspaceEdit。**可能已经工作**——但需要测试覆盖：把 rename Emmy 类型作为 explicit 测试。

**陷阱**：当前 references 的 word 提取假设 identifier 是 ASCII。rename 如果传入的 `new_name` 是合法 Lua 标识符（已校验），OK。注解里的 Emmy 类型引用位置是 UTF-16 Range，rename 反应到 TextEdit 也是 UTF-16，一致。

**测试思路**：`tests/test_rename.rs`（新建）— fixture 两个文件，rename `Foo` → `Bar`，期望两处文件的 WorkspaceEdit 里同时包含 `---@class`、`---@type`、`---@param x`、`---@class Bar : <here>` 等全部替换。

---

### [ ] P1-7 — 反向类型依赖图 + 级联诊断重算

**目标**：修改 `a.lua` 里 `@class Foo` 的字段后，所有在注解里引用 `Foo`（即便不 require a.lua）的 `b.lua` 的语义诊断能自动重算。当前 `require_by_return` 只覆盖 require 依赖。

**锚点**：
- 数据结构：`lsp/crates/mylua-lsp/src/aggregation.rs::WorkspaceAggregation` 新增 `type_dependants: HashMap<String, Vec<Uri>>`（类型名 → 引用它的文件列表）
- 填充：`upsert_summary` 里扫 `summary.local_type_facts` / `type_definitions.parents` / `type_definitions.fields` / `function_summaries.params/returns` 里出现的 `EmmyType(name)`、`TypeRef { name }` 全部反写到 `type_dependants[name]`
- 消费：`lsp/crates/mylua-lsp/src/lib.rs::collect_dependant_uris` 同时看 `require_by_return` 和 `type_dependants[name]`（name 取自 `summary.type_definitions`）

**陷阱**：要 dedup、要在 `remove_contributions` 里也把这个 URI 从所有 `type_dependants` value 里移除（类似 `require_map.retain` 的 handling）。

**测试思路**：`tests/test_diagnostics.rs` + `tests/test_workspace.rs`：
- a.lua 定义 `@class Foo` + `@field x integer`
- b.lua `---@type Foo local f = ... print(f.x)`，初始干净
- 修改 a.lua 把 `x` 改成 `y`（upsert_summary），b.lua 对 `f.x` 的诊断应被标脏

（注：如果 b.lua 未打开，只校验 `type_dependants` 含 b.uri 即可——真的重算诊断需要在 lib.rs 里走 schedule_semantic_diagnostics，这需要 LSP runtime，纯测试可能只能测数据结构层）

---

### [x] P1-8 — 匿名 / local function 签名推断

**目标**：`local f = function(a, b) return a + b end` 之后 `f(|)` 的 signatureHelp 应显示参数 `a, b`；hover 显示完整签名。

**锚点**：`lsp/crates/mylua-lsp/src/summary_builder.rs::infer_expression_type` 里 `"function_definition"` 分支（目前返回空 FunctionSignature）。

**实现提示**：
- 抽取 `function_definition` 的 `parameters` 子节点，用类似 `extract_ast_params` 的逻辑收集 `ParamInfo`
- 若周围有 `---@param`/`---@return` emmy 注解（相对于 `local f = function ...` 语句），合并类型信息
- 返回 `TypeFact::Known(KnownType::Function(FunctionSignature { params, returns }))`

**陷阱**：当 `function_definition` 出现在 RHS 时，emmy 注解在 `local f` 前面的行里，需要在 `visit_local_declaration` 里把 pending emmy 转给这个函数。现有 `build_function_summary` 只处理 `function_declaration` / `local_function_declaration`；匿名 function 不经过它。

**测试思路**：`tests/test_signature_help.rs` + `tests/test_hover.rs`：
- `---@param a number\nlocal f = function(a, b) end\nf(|)` — signatureHelp 应显示 `a: number, b`
- hover on `f` 应显示 `fun(a: number, b): ...`

---

### [x] P1-9 — `local a, b = foo()` 多返回值分派类型

**目标**：`local a, b = f()` 里 `a` 拿 `f` 的第 1 个返回值类型、`b` 拿第 2 个。

**锚点**：`lsp/crates/mylua-lsp/src/summary_builder.rs::visit_local_declaration`

**实现提示**：当 `name_count > 1` 且 values 只有 1 个表达式 + 是 `function_call` 时，按其 `FunctionSignature.returns[i]` 分别赋给 `ctx.local_type_facts[names[i]]`。对 multi-return 推断不准确时退回 Unknown。

**测试思路**：`test_hover.rs`：`function f() ---@return number, string\nreturn 1, "x" end\nlocal a, b = f()` 悬停 `a` 显示 number、`b` 显示 string。

---

## P2 UX 提升 — 优先级低，边际收益小

### ~~P2-1 — Semantic tokens 细分 token type~~（**WONTFIX — 故意不做**）

**结论**：本项目 **刻意不做** semantic tokens 的 token type 细分。semantic tokens 定位为 **TextMate 基色的最小补充**，只发 TextMate 静态无法判断的语义（全局 `defaultLibrary`、全局/局部区分、Emmy 类型名等）；`keyword` / `number` / `string` / 注释等基色一律由 TextMate 负责。

**理由**：

1. TextMate 在无语义信息下已能稳定着色，semantic tokens 再细分**收益边际、维护成本高**
2. 细分会放大 TextMate 与 LSP **作用域命名不一致** 导致的"跳色"问题
3. 5 万文件级工作区下 semantic tokens 的计算/传输成本应保持最低
4. `keyword` / `number` / `string` 等在 LSP ready 之前就已由 TextMate 着色，细分反而会在打开瞬间出现"闪烁"

**设计依据**：见 [`requirements.md`](requirements.md) §3.1（已更新）和 [`architecture.md`](architecture.md) §3.1 / §4 能力表。

---

### [ ] P2-2 — Semantic tokens range provider

**目标**：大文件打开时客户端只要视口里的部分。

**锚点**：`ServerCapabilities.semantic_tokens_provider.range = Some(true)` + `async fn semantic_tokens_range(...)`。

**实现提示**：调用已有的 `collect_semantic_tokens`，返回前按 `range` 过滤 token。

---

### [ ] P2-3 — 更多诊断类别

- 函数调用参数**个数**不匹配（`FunctionSummary.signature.params.len()` vs 实参数）
- 函数调用参数**类型**不匹配（emmy `@param` vs 实参推断类型）
- `@return` 与实际 `return` 语句数量/类型不匹配
- 表字面量里的**重复 key**
- **未使用 local**（warning，可配置）
- `---@type` 声明和 **后续赋值**类型不匹配（目前只 check 最初字面量，见 `diagnostics::find_local_rhs_type`）

**锚点**：`lsp/crates/mylua-lsp/src/diagnostics.rs`、`src/config.rs::DiagnosticsConfig`（每类加配置项）。

**实现提示**：每新增一类应有配置项 + 默认 severity。`config.rs` 里的 `DiagnosticsConfig` 照 `emmy_type_mismatch` 的模式扩。

---

### [ ] P2-4 — `textDocument/inlayHint`

**目标**：调用参数内联显示形参名（`foo(▸x= 1, ▸y= 2)`），局部变量类型内联显示（`local x: number = 1`）。

**锚点**：新建 `src/inlay_hint.rs` + `lib.rs::inlay_hint_provider = Some(OneOf::Left(true))` + `async fn inlay_hint(...)`。

**实现提示**：复用 `FunctionSummary` 形参名 / `LocalTypeFact`。要给用户配置开关（`mylua.inlayHint.enable`、`parameterNames`、`variableTypes` 等）。

---

### [ ] P2-5 — `runtime.version` 真正生效

**目标**：`config.rs::RuntimeConfig.version` 现在是死字段。应根据 `"5.3"` / `"5.4"` / `"5.1"` / `"luajit"` 切换内置 builtin 列表（bit32 / goto label 语义 / etc）、影响 `diagnostics::LUA_BUILTINS`。

**锚点**：`src/config.rs::RuntimeConfig`、`src/diagnostics.rs::LUA_BUILTINS` + `src/semantic_tokens.rs::LUA_BUILTINS`。

**实现提示**：抽 `fn builtins_for(version: &str) -> &'static [&'static str]`，两处调用都读 config。

---

### [ ] P2-6 — `textDocument/selectionRange`

**目标**：VS Code "智能扩展选区" 快捷键（macOS `⌃⇧→`）。一个 AST 自底向上串。

**锚点**：新建 `src/selection_range.rs` + `lib.rs::selection_range_provider`。

**实现提示**：每个 position，从最深 descendant 向 root 构造 `SelectionRange { range, parent: Some(Box<SelectionRange>) }` 链。

---

### [ ] P2-7 — `textDocument/declaration`

Lua 里 declaration ≡ definition。最简实现：alias 到现有 `goto_definition`。

**锚点**：`lib.rs::declaration_provider`。

---

### [ ] P2-8 — `completionItem/resolve`

**目标**：懒加载 completion item 的 `documentation` / `detail`，减少初始 payload。

**锚点**：`ServerCapabilities.completion_provider.resolve_provider = Some(true)` + `async fn completion_resolve(item) -> Result<CompletionItem>`。

**实现提示**：Completion 第一次返回的 items 只填 label+kind+detail；resolve 阶段才去 hover 拿 markdown 文档填 `documentation`。

---

## 附录：锚点文件速查

| 关键能力 | 主文件 | 相关模块 |
|---|---|---|
| 入口 / 派发 / capabilities | `lsp/crates/mylua-lsp/src/lib.rs` | |
| AST 单文件推断 → DocumentSummary | `src/summary_builder.rs` | `src/emmy.rs`（注解解析）|
| 跨文件聚合 + 缓存 | `src/aggregation.rs` | `src/resolver.rs`（链式解析）|
| 作用域树 | `src/scope.rs` | |
| Goto / Hover / Completion / References / Rename / Signature | 各同名 `src/*.rs` | |
| Diagnostics (syntax + semantic) | `src/diagnostics.rs` | |
| Workspace scan / file filter | `src/workspace_scanner.rs` | |
| Summary 持久化缓存 | `src/summary_cache.rs` | |
| Tree-sitter bridge | `crates/tree-sitter-mylua/` | |
| 集成测试 | `crates/mylua-lsp/tests/test_*.rs` | |
| 测试 fixture（手动 E2E） | `tests/lua-root/`、`tests/lua-root2/` | |
| 测试 fixture（Rust 集成测试用）| `tests/hover/`、`tests/complete/`、`tests/define/`、`tests/parse/`、`tests/project/` | |

## 开发工作流速查

```bash
# 构建 LSP
cd lsp && cargo build -p mylua-lsp

# 跑全部测试
cd lsp && cargo test --tests -p mylua-lsp

# 构建 + 启动 VSCode 扩展开发宿主
.cursor/scripts/test-extension.sh       # macOS / Linux
.cursor/scripts/test-extension.ps1      # Windows

# 独立 target 目录（避免 VSCode 扩展锁住二进制）
$env:CARGO_TARGET_DIR="target-test"; cargo test --tests   # PowerShell
CARGO_TARGET_DIR=target-test cargo test --tests           # bash/zsh
```

## Code review 流程提醒

依 `.cursor/rules/code-review-after-changes.mdc`：每组功能性改动后：
1. 运行 `cargo build -p mylua-lsp`，确保零新增 error/warning
2. 运行 `cargo test --tests -p mylua-lsp`，确保全绿
3. 调用 `code-reviewer` subagent（见 `.cursor/agents/code-reviewer.md`），传入变更文件清单和目标
4. 处理 BLOCKING → 重跑 review，直至 APPROVED
5. 最后同步 `ai-readme.md`（能力表 / 测试矩阵 / 索引架构描述）
