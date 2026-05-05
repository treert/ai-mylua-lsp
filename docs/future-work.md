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

### 1.3 [P4] 诊断路径请求级局部缓存（resolve_type local_cache）

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

---

## 3. 内存优化

### 3.1 [P1] 全局字符串驻留池

- **问题**：2 万文件工作区源文件总大小约 200M，但 LSP RSS 可达 ~6G。主要瓶颈不是 AST LRU，而是索引 / summary / scope 等常驻结构中大量重复 `String` 复制（全局名、类型名、字段名、函数名、模块名、局部变量名等）。
- **方案**：引入类似 `UriId` 的 `LuaSymbol` newtype，内部封装全局 `lasso::ThreadedRodeo`，外部只能通过 `LuaSymbol` 间接 intern / resolve，不直接访问 rodeo。将长期驻留内存的数据结构从 `String` 改为 `LuaSymbol`，重点覆盖 `WorkspaceAggregation` / `GlobalShard`、`DocumentSummary`、`ScopeTree`、`TypeFact` / `TableShape` 等索引热数据；hover、诊断 message、completion label 等请求级临时拼装字符串不纳入本优化。
- **JSON 输出**：summary 的 JSON 仅用于 `lua_perf` 调试查看，无反序列化兼容要求。`LuaSymbol` 自定义 `Serialize`，序列化时通过全局 interner 输出原始字符串，而不是输出底层整数。
- **验收**：2 万文件工作区 RSS 显著下降；索引构建和 HashMap 查找因整数 key 受益；`lua_perf` summary JSON 仍输出可读字符串；LSP 功能测试无回归。

---

## 4. 推荐落地顺序

1. **2.1** 泛型 variance 诊断 — 收益明显，默认 off 降低风险
2. **1.1** per-name fingerprint — 改动较大，可显著缩小大型工作区的级联重算范围
3. **1.3** 反向图查重数据结构 — 规模到 1 万+ 文件前不紧迫
4. **3.1** 全局字符串驻留池 — 优先处理常驻结构里的重复 `String`，预计对 2 万文件工作区 RSS 收益最大
5. 其余 P3 项按需补做

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