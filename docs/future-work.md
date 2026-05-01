# Future Work — 后续待办

> **本文件只保留尚未实现的方向。** 已完成的条目应从本文件中删除；如果完成项涉及架构或数据结构变更，需在同一次提交中同步更新 [`index-architecture.md`](index-architecture.md)、[`architecture.md`](architecture.md) 等相关文档。
>
> 关联文档：[index-architecture.md](index-architecture.md)、[performance-analysis.md](performance-analysis.md)

---

## 1. 聚合层（`aggregation.rs`）

### 1.1 [P1] `signature_fingerprint` 粒度过粗

- **问题**：文件级单一 hash，任何一个对外 API 变动都让整个下游链路失效。对"挂了几十个 global 的 `Mgr.lua`"影响尤为明显。
- **方案**：改为 **per-name fingerprint**（`HashMap<String, u64>`），按名字逐个 diff，只标脏变化的名字。文件级 hash 保留作 quick check。
- **验收**：改一个 class 的单个 field，其他 class 的 cache 不被标脏。

### 1.3 [P2] `type_dependants` 线性查重 + 全桶 retain

- **问题**：写入侧 `iter().any()` 线性查重；删除侧对每个 bucket 全量 `retain`，复杂度 `O(Σ buckets)`。
- **方案**：value 改为 `HashSet<Uri>`；引入 `per_uri_contributions` 反查表，删除时只动相关桶。
- **验收**：10,000 文件工作区，upsert 耗时从 O(all buckets) 降到 O(本文件贡献数)。

### 1.4 [P4] 诊断路径请求级局部缓存（resolve_type local_cache）

- **问题**：全局 `resolution_cache` 已移除（因 `&mut` 约束和失效 bug），但诊断遍历同一文件时可能对相同 base 重复解析。例如 `cfg.width`、`cfg.height`、`cfg.title` 各自独立 resolve `RequireRef("config")`。
- **方案**：给 `resolve_type` 增加 `cache: Option<&mut HashMap<CacheKey, ResolvedType>>` 参数。诊断入口创建临时 HashMap，遍历完即丢弃；goto/hover 等传 `None`。无需失效逻辑——每次请求都是新 cache。
- **优先级**：低。当前解析链都是 O(1) HashMap 查表，单次微秒级。仅在大文件（数千个字段访问）出现可测量延迟时再实施。
- **验收**：大文件诊断耗时有可测量下降（profiling 对比）。

### 1.5 [P3] `TypeCandidate` 只存剪影 → 消费方二次线扫

- **问题**：不含 fields / parents，消费方需回查 `summaries[uri].type_definitions` 做 `find()` 线扫。
- **方案**：`type_definitions` 改为 `HashMap<String, TypeDefinition>`，O(1) 查询。注意同文件多同名 class 的去重。
- **验收**：hover 热路径的"候选 → 详情"查找耗时下降。

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

- **设计文档**：[`superpowers/specs/2026-05-01-ast-lru-cache-design.md`](superpowers/specs/2026-05-01-ast-lru-cache-design.md)
- **问题**：`documents` 为每个文件常驻 `LuaSource + tree + scope_tree`，大型项目内存线性增长（2 万文件工作区 ~6.5GB），但大部分语法树不被频繁访问。
- **核心思路**：拆分 `DocumentStore`——`LuaSource` 常驻，`tree + scope_tree` 走 LRU 缓存（`astCacheCapacity` 可配置）。慢文件（`slow_pinned`）和打开文件（`open_pinned`）双 pin 集合互不干扰。
- **阻塞因素**：`references.rs` 和 `rename.rs` 全量遍历所有文件 AST，summary 层无引用反向索引，无法缩小扫描范围。LRU 淘汰的文件会被频繁 `parse_temp` 重建，收益受限。**建议先实现引用反向索引，再实施 LRU。**
- **验收**：2 万文件工作区，AST 缓存容量 200，全流程无功能回归；内存 RSS 显著下降（预估释放 1.2~1.8GB）。

---

## 4. 推荐落地顺序

1. **2.1** 泛型 variance 诊断 — 收益明显，默认 off 降低风险
2. **1.1** per-name fingerprint — 改动较大，对大型工作区 cache 命中率有实质提升
3. **1.3** 反向图查重数据结构 — 规模到 1 万+ 文件前不紧迫
4. **3.1** 语法树 LRU 缓存 — 依赖引用反向索引，否则淘汰收益受限
5. 其余 P3 项按需补做

---

## 5. 维护约定

- 已完成的条目直接从本文件删除；如涉及架构变更，同一次提交更新相关文档（`index-architecture.md`、`architecture.md` 等）。
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