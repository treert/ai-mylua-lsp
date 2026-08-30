# Future Work — 后续待办

> **本文件只保留尚未实现的方向。** 已完成的条目直接删除；如果完成项涉及架构或数据结构变更，需在同一次提交中同步更新 [`index-architecture.md`](index-architecture.md)、[`architecture.md`](architecture.md) 等相关文档。
>
> 关联文档：[index-architecture.md](index-architecture.md)、[performance-analysis.md](performance-analysis.md)、[lsp-semantic-spec.md](lsp-semantic-spec.md)

---

## 1. 索引与聚合层

### 1.1 [P1] `signature_fingerprint` 粒度过粗

- **问题**：文件级单一 hash（`DocumentSummary.signature_fingerprint`），任何一个对外 API 变动都让整个下游链路失效。对"挂了几十个 global 的 `Mgr.lua`"影响尤为明显。
- **方案**：改为 **per-name fingerprint**（`HashMap<String, u64>`），按名字逐个 diff，只标脏变化的名字。文件级 hash 保留作 quick check。
- **验收**：改一个 class 的单个 field，其他 class 的下游文件不被标脏。

### 1.2 [P3] `TypeCandidate` 只存剪影 → 消费方二次线扫

- **问题**：不含 fields / parents，消费方需回查 `summaries[uri].type_definitions` 做 `find()` 线扫（该字段仍是 `Vec<TypeDefinition>`）。
- **方案**：`type_definitions` 改为 `HashMap<String, TypeDefinition>`，O(1) 查询。注意同文件多同名 class 的去重。
- **验收**：hover 热路径的"候选 → 详情"查找耗时下降。

### 1.3 [P3] `FieldInfo.assignment_count` 名不副实

- **问题**：`table_shape.rs` 的 `FieldInfo.assignment_count` 注释写着"同一字段多次赋值时累加（union）"，但全仓 4 处构造点全部硬编码 `1`，从未累加，也无任何消费方。同一结构体的 `def_range` 有类似问题：注释说"首次定义位置"，而 `set_field` 是覆盖写入，实际存的是**最后一次**（`diagnostics/env_field.rs` 因此自建写点表，不用该字段，见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §1.3.1）。
- **方案**：二选一——要么实现累加并接上 union 推断，要么删除该字段并修正 `def_range` 的注释使其与实现相符。倾向后者：当前没有消费方，留着只会误导下一个读代码的人。
- **验收**：字段注释与实现一致；若保留则有测试覆盖累加行为。

---

## 2. 泛型支持缺口

### 2.1 [P3] 泛型实参未从调用点回填

- **问题**：`resolver.rs` 能对已知实参做替换，但调用点的实参类型没有传进来——`setmetatable({}, …)` 的 `call_arg_types` 是 `[Unknown, Unknown]`，其 `---@generic T … @return T` 因此解析为**未替换**的 `EmmyType("T")` 而非 `{}` 的 shape。
- **耦合警告**：`diagnostics/env_field.rs` 的静默契约目前**部分依赖**这个缺口。实现回填后，`_ENV = setmetatable({}, {__index = _G})` 会解析出确定的 table shape，若不同时保证 metatable 追踪生效，沙箱内所有名字会爆红。该模块已按"装了 metatable 即失去穷尽性"独立处理此事，并由 `test_global_env.rs::setmetatable_env_reports_nothing` 锁定——**实现本条时该测试必须保持绿**。
- **方案**：将调用点实参类型传入 `substitute_generic_params`，用 `unify_one` 的现有绑定机制回填。
- **验收**：`local t = setmetatable({a=1}, {})` 的 `t.a` 可解析；且上述 `setmetatable_env_reports_nothing` 不变红。

### 2.2 [P3] 泛型上界约束（`@generic T : Foo`）未校验

- **问题**：Emmy 注解解析层已能读出 constraint（`GenericParam.constraint`），但 `FunctionSummary.generic_params` / `TypeDefinition` 只保存泛型名，违反约束的用法无法诊断。
- **方案**：将 bound 传播到 `FunctionSummary` / `TypeDefinition`，并在泛型实例化与调用诊断中校验。
- **验收**：约束违反 / 满足两类用例。

### 2.3 [P3] 泛型实参数量不校验

- **问题**：`Foo<T, U>` 用 `Foo<string>`（少一个）静默兜底不报错。
- **方案**：对比 `generic_params.len()` 与实参数量，不等报 `genericArityMismatch`。

### 2.4 [P3] 递归泛型栈溢出风险

- **问题**：`resolver.rs::substitute_in_fact` 无深度保护（递归调用不带 depth 参数），病态递归输入可能栈溢出。
- **方案**：加深度计数器，超阈值（如 32，与 `MAX_RESOLVE_DEPTH` 一致）停止递归返回原 fact。

---

## 3. `_ENV` 沙箱能力缺口

> 背景与已实现部分见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §1.3 / §1.3.1 / §1.6。
> 已支持：goto / hover / references / `envUnknownField` 诊断。

### 3.1 [P3] 沙箱内自由名不提供补全

- **问题**：`_ENV` 被重定向后，该作用域内的自由名不进入补全列表——既不污染、也不误报，但也不提供能力。当前是文档明确的既定立场，非疏漏。
- **待定的语义问题**（动手前须先定）：补全候选应当是什么？
  - 仅新环境表已知字段（最保守，但对 `setmetatable({}, {__index=_G})` 这类沙箱会漏掉绝大部分可用名字）；
  - 还是叠加全局命名空间（对纯 `_ENV = {}` 会给出运行时为 nil 的候选，与 `envUnknownField` 诊断自相矛盾）。
  - 两者取舍取决于是否追踪 `__index`——这与 2.1 的 metatable 处理是同一个问题。
- **方案**：语义定下后，在 `completion.rs` 接入 `name_resolution` 的 `EnvField` 分支。
- **验收**：`local _ENV = {allowed=1}` 下补全含 `allowed`；纯 `_ENV = {}` 下的行为符合上面选定的语义，且与 `envUnknownField` 的判断不冲突。

### 3.2 [P3] `document_highlight` 不区分环境边界

- **问题**：`g = 1; _ENV = {}; g = 2` 中两个 `g` 运行时是不同变量，goto / references 已能区分，但同文件高亮仍把四处一起点亮。
- **原因**：`document_highlight.rs` 按设计只做「文本匹配 + 作用域 decl_byte 过滤」，不查索引，因此对**所有**全局名都不做语义区分——这不是 `_ENV` 特有的缺口。
- **方案**：若要修，需让它对非 local 的名字也走 `name_resolution`（§1.6 的公共层），代价是每次高亮请求都要做索引查询。需先评估该请求的调用频次（编辑器在光标移动时会频繁触发）是否承受得起。
- **风险**：这是性能与精确性的取舍，不是纯 bug 修复。改动前应先量测。

---

## 4. EmmyLua 注解

### 4.1 [P3] `emmy_type_name_at_byte` 无 AST 上下文

- **问题**：`emmy.rs::emmy_type_name_at_byte` 用纯字节扫描判定光标是否在 `---@...` 行的结构区。多行字符串/长注释里出现 emmy 样式的文本（例如 `[[\n  local x = ---@type Foo\n]]`）会被误识别为真正的类型引用，导致 hover/goto/references 出现错误命中。
- **方案**：三处调用入口（`hover.rs::hover` / `goto.rs::goto_definition` / `references.rs::identify_at_cursor`）改用 AST 先把光标定位到节点，仅当祖先链含 `emmy_comment` / `comment` 时再调 `emmy_type_name_at_byte`。
- **验收**：用户构造的"长字符串内含 emmy-like 行"用例不再触发类型 ref 误命中；既有 trailing/leading emmy 行的 goto/hover 行为保持不变。
- **风险**：触发条件极冷门，目前为已知限制（该函数 doc 注释指回本文档）。

---

## 5. 推荐落地顺序

1. **1.1** per-name fingerprint — 改动较大，可显著缩小大型工作区的级联重算范围
2. **1.3** `assignment_count` / `def_range` 注释与实现对齐 — 改动极小，且消除一处持续误导
3. **2.1** 泛型实参回填 — 能力提升明显，但需连带确认 metatable 追踪（见条目内耦合警告）
4. **1.2** `type_definitions` O(1) 详情索引 — 规模到 1 万+ 文件前不紧迫
5. **3.1** 沙箱补全 — 须先定语义，与 2.1 同批做更省事
6. 其余 P3 项按需补做

---

## 6. 维护约定

- 已完成的条目直接从本文件删除；如涉及架构变更，同一次提交更新相关文档（`index-architecture.md`、`architecture.md` 等）。
- 新增条目模板：

```markdown
### [Px] <标题>

- **问题**：为什么要做（现状是什么、错在哪）
- **方案**：怎么做
- **验收**：什么条件下认为做完
- **风险**：（可选）对既有行为的影响、是否需要 opt-in、与其它条目的耦合
```

---

## 7. 新增能力时的维护清单

- **新增诊断类别**：在 `DiagnosticsConfig` 加字段 + 默认 severity + `vscode-extension/package.json` 配置声明 + `diagnostics/suppression.rs` 的抑制码；默认开启时需在 fixture 上跑一遍确认不会在真实项目上产生大量噪声
- **新增 LSP capability**：在 `lib.rs::initialize` 的 `ServerCapabilities` 声明 + async handler；独立的 `src/<feature>.rs` 模块 + 对应集成测试文件
- **涉及裸标识符解析**：只改 `name_resolution.rs`（§1.6 的单点实现），不要在单个能力里内联解析顺序
- **代码修改后**：按 [`../.cursor/rules/coding-discipline.mdc`](../.cursor/rules/coding-discipline.mdc) 的纪律执行（尤其「Rust 禁止格式化」与「最小改动」），跑全量测试
- **文档同步**：对外能力变动同步 [`lsp-capabilities.md`](lsp-capabilities.md)；架构/数据流变动同步 [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md)；语义规则变动同步 [`lsp-semantic-spec.md`](lsp-semantic-spec.md)；用户可见变动追加 `vscode-extension/CHANGELOG.md` 的 `[Unreleased]`
