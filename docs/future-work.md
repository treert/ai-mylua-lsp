# Future Work — 后续待办与维护提示

> **当前状态：无已知紧急待办。**
>
> 过往的待办项（诊断扩展、selection_range 精细化、signature_help shape-owner、callHierarchy、documentLink、foldingRange 分支折叠、semantic tokens delta、`---@meta`、`fun()` 多返回 + `self` 泛型绑定 等）已全部落地。各能力的实现细节与测试覆盖记录在 [`../ai-readme.md`](../ai-readme.md) 的「已实现 LSP 能力」章节与 `lsp/crates/mylua-lsp/tests/` 下的集成测试文件中。
>
> 本文档只保留 **真正还没做** 的方向；已实现的不再列在这里。

本文档收集 `WorkspaceAggregation`（聚合层）与相关索引数据结构 / 泛型支持的**已知坑点**和**优化方向**。
>
> **当前状态**：以下条目在现网行为可见但**暂未造成严重回归**（除冷启动依赖丢失外），大多属于性能/精度层面的改进。实际排期取决于规模增长（>1 万文件）和诊断准确性需求。
>
> **关联文档**：
> - 实现现状：[`index-architecture.md`](index-architecture.md)、[`../ai-readme.md`](../ai-readme.md) 「索引架构」章节
> - 性能相关的正交瓶颈：[`performance-analysis.md`](performance-analysis.md)

---

## 1. `WorkspaceAggregation::upsert_summary` 相关坑点

代码位置：[`lsp/crates/mylua-lsp/src/aggregation.rs`](../lsp/crates/mylua-lsp/src/aggregation.rs)。

### 1.1 [P1] 冷启动阶段 `require_by_return` 反向边丢失

- **动机**：`upsert_summary` 写 `require_by_return` 前调用 `resolve_module_to_uri(rb.module_path)`；若冷启动时被 require 的目标文件尚未建 `require_map`，返回 `None` 后这条反向依赖**永久丢失**（无幂等回填）。触发 cascade 失效时（`require_by_return` 驱动的诊断重跑）会漏掉这些文件，表现为"改了 `m.n`，引用它的 A 文件诊断没刷"。
- **影响范围**：
  - `aggregation.rs:192-202`（`upsert_summary` 的 require_bindings 分支）
  - `aggregation.rs:236-238`（`set_require_mapping` — 当前只写 map，不触发回填）
  - `workspace_scanner.rs`（冷启动文件遍历顺序）
  - 间接影响诊断调度器的 cascade 完整性
- **根因**：聚合层没有"待决反向边"队列；新 `require_map` 条目被写入时不主动扫已有 summary 补登反向边。
- **建议方案**：
  - (a) **最小改动**：在 `set_require_mapping` 里扫描 `summaries` 的 `require_bindings`，把 `module_path == 新注册路径` 的条目补登进 `require_by_return`。写操作 O(summaries × avg require_bindings)，冷启动一次性开销可接受。
  - (b) **更稳**：维护一张 `pending_reverse_edges: HashMap<String, Vec<RequireDependant>>` 暂存未解析的边；`set_require_mapping` 命中时排空对应 bucket 并提升到 `require_by_return`。
  - 无论哪种，`resolve_module_to_uri` 的 fallback 路径（`aggregation.rs:348-354`）命中时建议把结果回填进 `require_map`，避免每次 upsert 都重扫。
- **验收**：
  - 新增 `test_workspace.rs` 用例：随机顺序 upsert 一组互相 require 的文件 → 所有反向边齐全。
  - 现有 `test_type_dependants.rs::test_cascade_*` 风格的用例增加 "A require B 但 B 先扫入" 的场景。
- **风险**：`set_require_mapping` 当前是轻量操作（O(1) 写 HashMap），方案 (a) 把它变成 O(N) 线扫；若 workspace 扫描阶段大量调用它，可能明显拖慢。先压测 + 考虑批处理。

### 1.2 [P1] `signature_fingerprint` 粒度过粗导致过度失效

- **动机**：`signature_fingerprint` 是**整个文件级**单一 hash。文件里任何一个对外 API 变了（加个 global、改个 `@field`），`affected.global_names ∪ type_names ∪ module_paths` 的全部缓存条目都会被标脏。对"挂了几十个 global 的 `Mgr.lua`"，一次局部改动就让整个下游链路失效。
- **影响范围**：
  - `aggregation.rs:215-219`（触发点）
  - `aggregation.rs:269-304`（`collect_affected_names` — 当前是"全家桶"）
  - `summary.rs` 的 `signature_fingerprint` 计算（散落在 `summary_builder.rs`）
- **建议方案**：
  - 改为 **per-name fingerprint**：`global_fingerprints: HashMap<String, u64>` / `type_fingerprints: HashMap<String, u64>`，每个 `GlobalContribution` / `TypeDefinition` 各自 hash 自己的 signature（对类型来说是 fields 集合 + parents + generic_params）。
  - `collect_affected_names` 按名字逐个 diff：只把"fingerprint 不一致"的名字放进 `affected`。
  - 文件级 `signature_fingerprint` 保留作为 quick check（若相等直接跳过名字级 diff）。
- **验收**：
  - `test_type_dependants.rs` 加 "改一个 class 的单个 field，其他 class 的 cache 不被标脏" 的断言。
  - 压测冷启动 + 频繁编辑下 `resolution_cache` 的 dirty 命中率。
- **风险**：per-name hash 会增加 summary 体积（每名字多 8 bytes），对磁盘 cache 影响可控；实现复杂度中等。

### 1.3 [P2] `uri_priority_key` 的 `"annotation"` 误判

- **动机**：`uri_priority_key`（`aggregation.rs:124-129`）把 `path.to_ascii_lowercase().contains("annotation")` 作为第一优先维度。任何路径**子串**包含 `"annotation"` 的文件（如业务目录 `my-annotation-helper/`、文件 `emit-annotation.lua`）都被误提权为"高优先级定义源"。
- **影响范围**：所有用到 `sort_by_cached_key(uri_priority_key)` 的 shard（`global_shard` / `type_shard`），最终影响 hover 展示的"主定义"、goto 单候选策略的落位。
- **建议方案**：
  - 将判断改为 **路径段**匹配：按 `/` 切段，命中 `"annotation"` / `"annotations"` 段才提权。
  - 或者走配置项：`workspace.annotationDirs: Vec<String>`，默认 `["annotation", "annotations", ".mylua/annotations"]`，用户可改。
- **验收**：新增单元测试覆盖"路径含 'annotation' 子串但不是目录段"的反例。
- **风险**：低。行为改变可能影响少量用户目录布局，但"子串匹配"本就是一个隐式契约，收敛成段匹配是更合理的默认。

### 1.4 [P2] `type_dependants` 线性查重 + 全桶 retain 的 O(N) 热路径

- **动机**：
  - 写入侧（`aggregation.rs:210`）：`uris.iter().any(|u| u == &uri)` 线性查；一个热门类型（如 `Entity`）被数千文件引用时，每次 upsert 都在 hot path 上扫一遍。
  - 删除侧（`aggregation.rs:260-263`）：`remove_contributions` 对**每个** bucket 全量 `retain`，即便本文件根本没出现过，也要遍历全部 bucket —— 复杂度 `O(Σ buckets)` 而非 `O(本文件贡献数)`。
- **影响范围**：`global_shard` / `type_shard` / `require_by_return` / `type_dependants` 四张表的删除/写入路径。
- **建议方案**：
  - `type_dependants` value 改为 `BTreeSet<Uri>` 或 `HashSet<Uri>`，查重/插入 O(log N) / O(1)。
  - `remove_contributions` 引入 `per_uri_contributions: HashMap<Uri, ContributionIndex>` 反查表：记录该 URI 贡献过哪些 `global_shard key` / `type_shard key` / `type_dependants key`，删除时只动这些桶。
- **验收**：
  - 压测：10,000 文件工作区，改一个高频引用类型的文件 → upsert 耗时从 O(all buckets) 降到 O(本文件贡献数)。
  - 现有 `test_type_dependants.rs` 不应有回归。
- **风险**：反查表会增加约 `files × avg_contributions` 条 HashMap 记录；内存影响需压测。

### 1.5 [P2] `collect_affected_names` 漏收 type_fact 引用的外部名字

- **动机**：`collect_affected_names` 只收 `gc.name` 和 `td.name`（本文件**定义**的名字），不收 `td.parents` / `td.fields[*].type_fact` / `gc.type_fact` 里**引用**的外部类型名或全局名。若一个文件的 `type_fact` 从 `GlobalRef { name: "OldMgr" }` 换成 `GlobalRef { name: "NewMgr" }`，`affected.global_names` 里只有本文件定义的名字，`CacheKey::Global { name: "OldMgr" }` / `"NewMgr"` 链路上的 cache **不会被标脏**。
- **影响范围**：`aggregation.rs:269-304`；`resolution_cache` 精度。
- **建议方案**：在 `collect_affected_names` 里扩展收集：
  - 扫 `old.global_contributions[*].type_fact` 和 `new.global_contributions[*].type_fact` 里所有 `Stub(GlobalRef { name })`、`Stub(TypeRef { name })`、`Known(EmmyType)` 等，把这些名字也加入 `affected`。
  - 逻辑可复用 `collect_referenced_type_names` 的 walk，扩展一份 "walk_for_names" 同时收类型名和 global 名。
- **验收**：新增用例：一个 typedef 的 field 类型从 `OldT` 改为 `NewT`，下游 `resolution_cache[CacheKey::Type { name: "OldT" }]` / `"NewT"` 都被标脏。
- **风险**：会扩大标脏范围（原来漏掉的现在要算），但这是**正确性**修复，权衡偏向做。

### 1.6 [P3] `TypeCandidate` 只存剪影 → 消费方二次线扫

- **动机**：`TypeCandidate` 只含 `name / kind / source_uri / range`，不含 fields / parents / generic_params。消费方（如 `get_generic_param_names` 在 `resolver.rs:843-856`）需要回 `summaries[source_uri].type_definitions` 再做一次 `find(|td| td.name == name)` 线扫。单次开销小，但 hover / completion 等高频路径累积起来不可忽略。
- **影响范围**：`aggregation.rs:50-57`（`TypeCandidate` 定义）、`resolver.rs` 和 `goto.rs` 里所有"拿到 candidate → 回查详情"的路径。
- **建议方案**：
  - 方案 A：`TypeCandidate` 直接内嵌 `TypeDefinition` 引用（`Arc<TypeDefinition>`），但需要改 summary 的生命周期管理。
  - 方案 B：`summaries` 的 `type_definitions` 改为 `HashMap<String, TypeDefinition>`（key = 类名），O(1) 查询，候选通过 `source_uri` 直接索引。注意 Lua 里同文件可以定义多个同名 class（虽然少见），需要加去重或改 `HashMap<String, Vec<TypeDefinition>>`。
- **验收**：profile hover 热路径，确认"候选 → 详情"的查找次数与耗时。
- **风险**：方案 B 涉及 `DocumentSummary` schema 变化，磁盘 cache 需要 bump `schema_version` 触发重建。

### 1.7 [P3] `invalidate_dependants_targeted` 命名/契约模糊

- **动机**：函数名叫"invalidate_dependants"但**只改** `resolution_cache` 的 dirty 位，不触发 `diagnostic_scheduler` 重跑，也不清 `require_by_return` / `type_dependants` 反向图。消费方（scheduler）需要自己显式查 `type_dependants` 决定要不要重跑下游。容易误解为"一键级联失效"。
- **影响范围**：`aggregation.rs:309-321`；所有调用方对其语义的预期。
- **建议方案**：
  - 最轻量：重命名为 `mark_resolution_cache_dirty_for(affected)`，在文档注释里把"哪一层负责什么"列清楚。
  - 或者提供 `cascade_invalidate(uri, affected) -> CascadeResult` 这样的 facade：一次性返回"需要重跑诊断的 URI 集合"供 scheduler 消费，把"查 `require_by_return` / `type_dependants`"的职责收敛回聚合层。
- **验收**：代码 review 确认每个调用方都在清楚自己要做什么；不再有"改了 aggregation，diagnostic 没刷"的 bug 归类到这里。
- **风险**：重命名本身零风险；facade 方案涉及 scheduler 重构，需单独排期。

---

## 2. 泛型支持现状与缺口

代码位置：`type_system.rs::EmmyGeneric`、`summary.rs::TypeDefinition.generic_params`、`emmy.rs::emmy_type_to_fact`、`resolver.rs::substitute_generics` 等。

### 2.1 已支持（当前工作良好）

| 能力 | 位置 |
|---|---|
| `@class Foo<T, U>` 解析 → `td.generic_params` | `summary_builder.rs` `pending_class` 链路 |
| `@generic T` + 后续 `@class Foo` 两步式 | `pending_generic_params` 缓冲 |
| `@type Foo<string>` / `@param x Foo<string>` → `EmmyGeneric(name, params)` | `emmy.rs:969-971` |
| `@field x T` 在 `Foo<string>` 实例上自动替换为 `string` | `resolver.rs:861-900`（`substitute_generics` / `substitute_in_fact`） |
| hover / goto / signature_help / diagnostics 识别 `EmmyGeneric` | `goto.rs:186`、`signature_help.rs:171`、`diagnostics.rs:699` |
| `type_dependants` 扫描时排除泛型参数名（防止 `T` 误连到真类 `T`） | `aggregation.rs:464-472` |
| `self` 类型替换成 class 名（fluent-style 方法链） | `type_system.rs::substitute_self` |

### 2.2 [P2] 函数级泛型的实参推断 ✅ 已实现（简化版本）

- **动机**：`@class Foo<T>` 上绑定的方法能 resolve；但**纯函数泛型**—— `---@generic T ---@param x T ---@return T` 这种不挂 class 的——**无法从调用点的实参类型反推 `T = string`**。`substitute_generics` 只接受显式的 `actual_params`（即 `Foo<string>` 里已经写死的）。
- **已实现**：
  - `FunctionSummary` 新增 `generic_params: Vec<String>` 字段，`build_function_summary` 收集 `@generic` 注解。
  - `resolver.rs` 新增 `unify_function_generics()` 函数，实现顶层一级合一：`@param x T` + actual `string` → `T = string`，支持 `T?`（Union with nil）。
  - `summary_builder::infer_call_return_type` 和 `hover::infer_call_return_fact` 在 plain function call 路径中调用泛型推断。
  - 测试覆盖：`test_hover.rs` 新增 `hover_generic_function_infers_return_type_string` 和 `hover_generic_function_infers_return_type_number`。
- **未覆盖（P3 追加）**：嵌套泛型合一（`List<T>` vs `List<string>`）、跨文件泛型函数调用的推断、`signature_help` / `diagnostics` 路径的泛型推断。

### 2.3 [P2] `is_named_type_compatible` 忽略泛型实参 → 假阴性

- **动机**：`diagnostics.rs:699-701` 的匹配是 `(KnownType::EmmyType(name), actual) | (KnownType::EmmyGeneric(name, _), actual)` ——模式里直接用 `_` 丢掉了 params。意味着 `List<string>` 和 `List<number>` 在诊断层被当成**同一个类型**，明显的类型错误不会报出来。
- **影响范围**：`diagnostics.rs` 的 `emmyTypeMismatch` / `argumentTypeMismatch` / `returnMismatch` 诊断路径。
- **建议方案**：
  - 改为按 params 递归比较 `known_types_compatible`：两边都是 `EmmyGeneric(name, _)` 且 name 相等时，`zip(params)` 后逐一递归检查。
  - 兼容考虑：允许其中一侧是 `EmmyType(name)`（无实参）匹配 `EmmyGeneric(name, _)`（有实参）—— 作为"未指定实参 == any"的宽松兜底，避免大量假阳性。
- **验收**：新增 `test_diagnostics.rs` 用例：`---@type List<string> local l = List<number>()`（类似语义）触发 `emmyTypeMismatch`。
- **风险**：打开后会在真实项目上产生新的诊断；**默认 severity 建议设为 `Warning` 或先放到 `default off`**，给用户 opt-in 窗口。

### 2.4 [P3] 泛型上界约束（`@generic T : Foo`）未解析也未校验

- **动机**：`summary.rs` 的 `generic_params: Vec<String>` 只存名字不存 bound；`emmy.rs` 对 `@generic T : Foo` 语法看起来也没有提取 bound 信息。违反约束的用法（`Container<Socket>` 但 `T : Comparable`）无法被诊断发现。
- **影响范围**：`summary.rs::TypeDefinition`、`emmy.rs` 解析、`diagnostics.rs` 新增约束校验。
- **建议方案**：
  - `TypeDefinition.generic_params` 升级为 `Vec<GenericParam { name: String, bound: Option<TypeFact> }>`（或独立字段 `generic_bounds: HashMap<String, TypeFact>`）。
  - `emmy.rs` 解析 `@generic T : Foo` 语法；`diagnostics.rs` 在泛型实参化时校验 `actual : bound` 的兼容性（复用 `known_types_compatible`）。
- **验收**：`test_diagnostics.rs` 新增约束违反 / 满足两类用例。
- **风险**：schema 改动 → 磁盘 cache 需 bump `schema_version`。

### 2.5 [P3] 泛型实参数量不校验

- **动机**：`@class Foo<T, U>` 但用 `@type Foo<string>`（少一个）或 `Foo<a, b, c>`（多一个），当前 `substitute_in_fact` 里 `actual_params.get(i)` 拿不到就保留原 `EmmyType` 不替换（`resolver.rs:881-886`）——**静默兜底不报错**，后续字段访问会出现"Unknown 但没提示"。
- **影响范围**：`diagnostics.rs` 新增诊断；`emmy.rs` 解析阶段可以提前 catch。
- **建议方案**：
  - `emmy.rs` 解析到 `Foo<...>` 时记录实参数量；`diagnostics.rs` 查 `type_shard[name]` 的 `generic_params.len()` 对比，不等就报 `genericArityMismatch` 诊断。
  - 配套配置项 `genericArityMismatch: Severity`（建议默认 `Warning`）。
- **验收**：`test_diagnostics.rs` 覆盖 arity 过多/过少两类。
- **风险**：低。

### 2.6 [P3] 递归泛型的栈溢出风险未显式防护

- **动机**：`@class Tree<T> ---@field children Tree<T>[]` 这类递归引用，`substitute_in_fact` 走 `EmmyGeneric` 递归分支（`resolver.rs:888-894`），**没有** `visited` 保护。深度递归或病态输入下可能栈溢出。`resolve_recursive` 层有 `visited`，但 substitute 路径是独立的。
- **影响范围**：`resolver.rs::substitute_in_fact`。
- **建议方案**：
  - 给 `substitute_in_fact` 加深度计数器（类似 `resolve_recursive` 的 `depth_budget`），超过阈值（如 32）就停止递归返回原 fact。
  - 或者引入 `visited: HashSet<(name, param_repr)>`，但 `TypeFact` 不是 `Hash`，实现更复杂。
- **验收**：构造一个递归泛型 fixture（`Tree<Tree<Tree<...>>>` 嵌套 40 层）跑 hover / completion，确认不 panic / 不栈溢出。
- **风险**：低，且是**防御性**修复，不改变正常路径行为。

### 2.7 [P3] `type_dependants` 的泛型参数过滤是"summary 全局并集"

- **动机**：`collect_referenced_type_names` 末尾（`aggregation.rs:464-472`）过滤泛型参数名的维度是"本 summary 里**所有** class 的 `generic_params` 并集"。意味着 class A 的泛型参数 `T` 会把 class B 里引用真类 `T` 的边也过滤掉（即便 B 没把 `T` 声明为泛型）。
- **影响范围**：`type_dependants` 的反向边完整性。
- **建议方案**：改为**作用域感知**：只在 walk 到某个 class 的 fields / alias_type / generic args 时排除**该 class 自身**的 generic_params。
- **验收**：新增 test：A 声明 `@class A<T>`、B 声明 `@field x T` 且 `T` 是真类 → `type_dependants["T"]` 包含 B。
- **风险**：低；实现需要把 walk 改成"带当前 class 上下文"，略微复杂。

---

## 3. 推荐落地顺序

按"影响 × 实现成本"排序，先做收益大 / 成本低的：

1. **1.1 require_by_return 回填**（P1）——冷启动正确性，动机强；改动局部。
2. **1.3 `uri_priority_key` 路径段匹配**（P2）——一小时工作量，修正一个隐藏的行为偏差。
3. **2.3 泛型 variance 诊断**（P2）——把 `_` 换成递归比较，收益明显。默认 off / warning 降低风险。
4. **1.2 per-name fingerprint**（P1）——改动较大，但对大型工作区的 cache 命中率有实质提升；排一个 sprint 做。
5. **1.4 反向图查重数据结构**（P2）——性能优化，在规模到 1 万+ 文件前不紧迫。
6. **1.5 `collect_affected_names` 扩展**（P2）——正确性修复，随 1.2 一起做。
7. **2.2 函数级泛型推断**（P2）——用户可见价值高，但合一实现需要认真设计。
8. 其余 P3 项按需补做。

---

## 4. 维护约定

- 每项落地后，在本文档对应条目下补 “**已完成于 <commit-hash / 日期>**”。
- 若条目导致 `DocumentSummary` / `TypeCandidate` 等磁盘可序列化结构变化，**必须** bump `lsp/crates/mylua-lsp/src/summary.rs` 的 `CACHE_SCHEMA_VERSION`。
- 对外能力变动同步 [`../ai-readme.md`](../ai-readme.md) 「已实现 LSP 能力」章节；架构变动同步 [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md)。

---

## 5. 新增能力时的维护清单

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