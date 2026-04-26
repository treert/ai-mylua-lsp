# Future Work — 后续待办

> 只保留**真正还没做**的方向；已实现的不再列在这里。
>
> 关联文档：[index-architecture.md](index-architecture.md)、[performance-analysis.md](performance-analysis.md)

---

## 1. 聚合层（`aggregation.rs`）

### 1.1 [P1] `signature_fingerprint` 粒度过粗

- **问题**：文件级单一 hash，任何一个对外 API 变动都让整个下游链路失效。对"挂了几十个 global 的 `Mgr.lua`"影响尤为明显。
- **方案**：改为 **per-name fingerprint**（`HashMap<String, u64>`），按名字逐个 diff，只标脏变化的名字。文件级 hash 保留作 quick check。
- **验收**：改一个 class 的单个 field，其他 class 的 cache 不被标脏。

### 1.2 [P2] `uri_priority_key` 的 `"annotation"` 误判

- **问题**：路径**子串**包含 `"annotation"` 就被提权（如 `my-annotation-helper/`），应改为**路径段**匹配。
- **方案**：按 `/` 切段匹配，或走配置项 `workspace.annotationDirs`。
- **验收**：路径含 `annotation` 子串但不是独立目录段时不提权。

### 1.3 [P2] `type_dependants` 线性查重 + 全桶 retain

- **问题**：写入侧 `iter().any()` 线性查重；删除侧对每个 bucket 全量 `retain`，复杂度 `O(Σ buckets)`。
- **方案**：value 改为 `HashSet<Uri>`；引入 `per_uri_contributions` 反查表，删除时只动相关桶。
- **验收**：10,000 文件工作区，upsert 耗时从 O(all buckets) 降到 O(本文件贡献数)。

### 1.4 [P2] `collect_affected_names` 漏收 type_fact 引用的外部名字

- **问题**：只收本文件**定义**的名字，不收 `type_fact` 里**引用**的外部类型名。导致 `resolution_cache` 漏标脏。
- **方案**：扩展收集 `type_fact` 中的 `GlobalRef` / `TypeRef` 名字。随 1.1 一起做。
- **验收**：field 类型从 `OldT` 改为 `NewT`，相关 cache 都被标脏。

### 1.5 [P3] `TypeCandidate` 只存剪影 → 消费方二次线扫

- **问题**：不含 fields / parents，消费方需回查 `summaries[uri].type_definitions` 做 `find()` 线扫。
- **方案**：`type_definitions` 改为 `HashMap<String, TypeDefinition>`，O(1) 查询。注意同文件多同名 class 的去重。
- **验收**：hover 热路径的"候选 → 详情"查找耗时下降。

### 1.6 [P3] `invalidate_dependants_targeted` 命名/契约模糊

- **问题**：函数名暗示"一键级联失效"，实际只改 `resolution_cache` dirty 位。
- **方案**：重命名为 `mark_resolution_cache_dirty_for`，或提供 `cascade_invalidate` facade 收敛职责。

### 1.7 [P2] 函数签名 ID 化——对齐 `TableShape` 的间接引用模式

**✅ 已完成于 2026-04-26**

- **问题**：`TableShape` 通过 `TableShapeId(u32)` 唯一定位，`TypeFact::Known(Table(id))` 指向 `summary.table_shapes[id]`。但函数签名直接内联在 `TypeFact::Known(Function(FunctionSignature))` 中，没有 ID 回查机制。导致：
  - 同一个函数被多处引用时，签名数据重复存储
  - 无法通过 `TypeFact` 回查函数的完整元信息（overload、Emmy 注解、定义位置）
  - local function 赋值给 table field / Emmy class member 时（如 `M.process = helper`），外部只能拿到内联签名副本，丢失了与原始 `FunctionSummary` 的关联
  - 外部消费方（hover、signature_help、completion、call_hierarchy、resolver）被迫用函数名字符串直接查 `function_summaries`，绕过了类型系统的间接引用
- **方案**：引入 `FunctionSummaryId(u32)`，对齐 `TableShapeId` 模式：
  - `function_summaries` key 从函数名字符串改为 `FunctionSummaryId`，成为纯粹的签名仓库（与 `table_shapes` 对称）
  - `TypeFact` 新增 `Known(FunctionRef(FunctionSummaryId))` variant，`GlobalContribution.type_fact` 和 `TypeFieldDef.type_fact` 等处通过 ID 引用函数签名
  - summary_builder 为每个函数分配递增 ID
  - 消费端通过 `def_uri + FunctionSummaryId` 查完整签名，**不再需要按名字直接访问 `function_summaries`**
  - 保留内联 `Known(Function(FunctionSignature))` 作为轻量 variant（Emmy 注解直接声明的函数类型无文件归属，不走 ID）
- **影响范围**：`type_system.rs`（TypeFact variant）、`summary.rs`（字段类型）、`summary_builder/`（ID 分配 + 写入）、`resolver.rs`（间接查找）、`hover.rs`、`signature_help.rs`、`completion.rs`、`call_hierarchy.rs`（消费端全部改为通过 ID 查签名，消除按名字直接访问 `function_summaries` 的模式）
- **依赖**：无前置依赖
- **验收**：`local function helper(); M.process = helper`，外部 hover `obj.process` 能查到 `helper` 的完整签名和 overload；`function_summaries` key 为 `FunctionSummaryId`，无名字冲突；外部消费方不再按名字直接查 `function_summaries`

---

## 2. 泛型支持缺口

### 2.1 [P2] `is_named_type_compatible` 忽略泛型实参

- **问题**：`List<string>` 和 `List<number>` 被当成同一类型，不报诊断。
- **方案**：按 params 递归比较；允许无实参侧作为"any"宽松兜底。默认 severity 建议 `Warning` 或 `default off`。
- **验收**：`List<string>` 赋值 `List<number>` 触发 `emmyTypeMismatch`。

### 2.2 [P3] 泛型上界约束（`@generic T : Foo`）未解析

- **问题**：`generic_params` 只存名字不存 bound，违反约束的用法无法诊断。
- **方案**：升级为 `Vec<GenericParam { name, bound }>` + 解析 + 校验。
- **验收**：约束违反 / 满足两类用例。

### 2.3 [P3] 泛型实参数量不校验

- **问题**：`Foo<T, U>` 用 `Foo<string>`（少一个）静默兜底不报错。
- **方案**：对比 `generic_params.len()` 与实参数量，不等报 `genericArityMismatch`。

### 2.4 [P3] 递归泛型栈溢出风险

- **问题**：`substitute_in_fact` 无深度保护，病态递归输入可能栈溢出。
- **方案**：加深度计数器，超阈值（如 32）停止递归返回原 fact。

### 2.5 [P3] `type_dependants` 泛型参数过滤是全局并集

- **问题**：class A 的泛型参数 `T` 会把 class B 里引用真类 `T` 的边也过滤掉。
- **方案**：改为作用域感知，只排除当前 class 自身的 generic_params。

---

## 3. 内存优化

### 3.1 [P2] 语法树 / 文件文本 LRU 缓存

- **问题**：`documents` 为每个文件常驻 `text + tree + scope`，大型项目内存线性增长，但大部分语法树不被频繁访问。
- **核心思路**：**Summary 常驻，AST 按需缓存。** `Document` 改为 `LruCache<Uri, Document>`（容量 10~20），cache miss 时从磁盘读 + 全量解析。
- **关键点**：
  - 用户正在编辑的文件不会被淘汰（最近使用）
  - 被淘汰文件丢失旧 tree，只能全量解析（但全量也很快，毫秒级）
  - Find References / Rename 等全局操作临时解析的文件可不放入 LRU
- **验收**：500+ 文件工作区，LRU 容量 15，全流程无功能回归；内存 RSS 对比有明显下降。

---

## 4. 推荐落地顺序

1. **1.2** `uri_priority_key` 路径段匹配 — 工作量小，修正隐藏偏差
2. **2.1** 泛型 variance 诊断 — 收益明显，默认 off 降低风险
3. **1.1** per-name fingerprint — 改动较大，对大型工作区 cache 命中率有实质提升
4. **1.7** 函数签名 ID 化 — 对齐 TableShape 模式，消除名字冲突与按名直接访问
5. **1.3** 反向图查重数据结构 — 规模到 1 万+ 文件前不紧迫
6. **1.4** `collect_affected_names` 扩展 — 正确性修复，随 1.1 一起做
7. **3.1** 语法树 LRU 缓存 — 内存优化，实现成本中等
8. 其余 P3 项按需补做

---

## 5. 维护约定

- 落地后在对应条目补 "**✅ 已完成于 \<日期\>**"，随后迁移到已完成区或删除。
- 涉及 `DocumentSummary` / `TypeCandidate` 等可序列化结构变化时，bump `CACHE_SCHEMA_VERSION`。
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
- **文档同步**：对外能力变动同步 [`../ai-readme.md`](../ai-readme.md)「已实现 LSP 能力」章节；架构/数据流变动同步 [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md)

新发现的方向追加到本文时请按以下模板：

```markdown
### <简短标题>

- **动机**：为什么要做
- **影响范围**：涉及的模块 / 数据结构 / 对外能力
- **验收**：什么条件下认为做完
- **风险 / 默认开关**：是否需要 opt-in、对既有行为的影响
```

同时在 [`README.md`](README.md) 的文档索引行同步一句话描述。