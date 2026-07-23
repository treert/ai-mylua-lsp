# Future Work — 后续待办

> **本文件只保留尚未实现的方向。** 已完成的条目应从本文件中删除；如果完成项涉及架构或数据结构变更，需在同一次提交中同步更新 [`index-architecture.md`](index-architecture.md)、[`architecture.md`](architecture.md) 等相关文档。
>
> 关联文档：[index-architecture.md](index-architecture.md)、[performance-analysis.md](performance-analysis.md)

---

## 1. 聚合层（`aggregation.rs`）

### 1.1 [P1] `signature_fingerprint` 粒度过粗

- **问题**：文件级单一 hash，任何一个对外 API 变动都让整个下游链路失效。对"挂了几十个 global 的 `Mgr.lua`"影响尤为明显。
- **方案**：改为 **per-name fingerprint**（`HashMap<String, u64>`），按名字逐个 diff，只标脏变化的名字。文件级 hash 保留作 quick check。
- **验收**：改一个 class 的单个 field，其他 class 的下游文件不被标脏。

### 1.3 [P3] `TypeCandidate` 只存剪影 → 消费方二次线扫

- **问题**：不含 fields / parents，消费方需回查 `summaries[uri].type_definitions` 做 `find()` 线扫。
- **方案**：`type_definitions` 改为 `HashMap<String, TypeDefinition>`，O(1) 查询。注意同文件多同名 class 的去重。
- **验收**：hover 热路径的"候选 → 详情"查找耗时下降。

---

## 2. 泛型支持缺口

### 2.1 [P3] 泛型上界约束（`@generic T : Foo`）未校验

- **问题**：Emmy 注解解析层已能读出 constraint，但 summary 仍只保存泛型名，违反约束的用法无法诊断。
- **方案**：将 bound 传播到 `FunctionSummary` / `TypeDefinition`，并在泛型实例化与调用诊断中校验。
- **验收**：约束违反 / 满足两类用例。

### 2.2 [P3] 泛型实参数量不校验

- **问题**：`Foo<T, U>` 用 `Foo<string>`（少一个）静默兜底不报错。
- **方案**：对比 `generic_params.len()` 与实参数量，不等报 `genericArityMismatch`。

### 2.3 [P3] 递归泛型栈溢出风险

- **问题**：`substitute_in_fact` 无深度保护，病态递归输入可能栈溢出。
- **方案**：加深度计数器，超阈值（如 32）停止递归返回原 fact。

---

## 3. EmmyLua 注解

### 3.1 [P3] `emmy_type_name_at_byte` 无 AST 上下文

- **问题**：`lsp/crates/mylua-lsp/src/emmy.rs::emmy_type_name_at_byte` 用纯字节扫描判定光标是否在 `---@...` 行的结构区。多行字符串/长注释里出现 emmy 样式的文本（例如 `[[\n  local x = ---@type Foo\n]]`）会被误识别为真正的类型引用，导致 hover/goto/references 出现错误命中。
- **方案**：调用入口（`hover.rs::hover` / `goto.rs::goto_definition` / `references.rs`）改用 AST 先把光标定位到节点，仅当祖先链含 `emmy_comment` / `comment` 时再调 `emmy_type_name_at_byte`。
- **验收**：用户构造的"长字符串内含 emmy-like 行"用例不再触发类型 ref 误命中；既有 trailing/leading emmy 行的 goto/hover 行为保持不变。
- **风险**：触发条件极冷门，目前为已知限制（见函数 doc）。

---

## 4. 推荐落地顺序

1. **1.1** per-name fingerprint — 改动较大，可显著缩小大型工作区的级联重算范围
2. **1.3** `type_definitions` O(1) 详情索引 — 规模到 1 万+ 文件前不紧迫
3. 其余 P3 项按需补做

---

## 5. 维护约定

- 已完成的条目直接从本文件删除；如涉及架构变更，同一次提交更新相关文档（`index-architecture.md`、`architecture.md` 等）。
- 新增条目模板：

```markdown
### [Px] <标题>

- **问题**：为什么要做
- **方案**：怎么做
- **验收**：什么条件下认为做完
```

---

## 6. 新增能力时的维护清单

- **新增诊断类别**：在 `DiagnosticsConfig` 加字段 + 默认 severity；默认开启时需在 fixture 上跑一遍确认不会在真实项目上产生大量噪声
- **新增 LSP capability**：在 `lib.rs::initialize` 的 `ServerCapabilities` 声明 + async handler；独立的 `src/<feature>.rs` 模块 + 对应集成测试文件
- **代码修改后**：按 [`../.cursor/rules/code-review-after-changes.mdc`](../.cursor/rules/code-review-after-changes.mdc) 跑构建验证 + code-reviewer
- **文档同步**：对外能力变动同步 [`../AGENTS.md`](../AGENTS.md)「已实现 LSP 能力」章节；架构/数据流变动同步 [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md)

新发现的方向追加到本文时请按以下模板：

```markdown
### <简短标题>

- **动机**：为什么要做
- **影响范围**：涉及的模块 / 数据结构 / 对外能力
- **验收**：什么条件下认为做完
- **风险 / 默认开关**：是否需要 opt-in、对既有行为的影响
```

同时在 [`README.md`](README.md) 的文档索引行同步一句话描述。