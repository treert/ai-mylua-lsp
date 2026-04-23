# 性能现状分析与优化方向

> **状态**：基于当前实现（`lsp/crates/mylua-lsp/`）在 2026-04 的代码快照做的分维度评估。架构亮点、已知瓶颈、设计权衡、规模分级与优化路线按优先级排序。
>
> **目标对齐**：[`../ai-readme.md`](../ai-readme.md) 声明"需要支持 5 万个 lua 文件级别"；本文以 5 万文件作为性能对标上限。
>
> **核心设计契约**（§3）：全工作区 `text + tree + scope_tree` 常驻内存，**不做 LRU / 懒 parse**。代价是 5 万文件量级 RSS ~1.5–3GB，收益是任何跨文件跳转都恒定低延迟。

---

## 1. 做得扎实的部分（架构亮点）

这些是项目当前性能的基础，属于"见过不少野路子 LSP 才会知道值钱"的设计。

| 维度 | 现状 | 参考位置 |
|------|------|----------|
| **并行冷启动** | `rayon` + 50 文件/批，真·多核并行解析 + 生成 summary；**scan 整体在 `initialized` 里 `tokio::spawn` 后台执行**，`initialized` 立即返回，不阻塞后续 `did_open` / `hover` 等请求；每批结束后通过 `mylua/indexStatus` 通知 VS Code 扩展驱动 status-bar 进度 | `lib.rs::run_workspace_scan`（由 `initialized` 后台 spawn） |
| **并发安全的 scan/did_open 交错** | scan batch 的 merge 阶段按 canonical `open_uris → documents → index` **整段持 `open_uris`** 锁；命中 `open_uris` 的 URI 同时跳过 `upsert_summary` 与 `docs.insert`，避免把 `parse_and_store_with_old_tree` 内部两段独立临界区写入的 buffer 版本被 scan 的 disk 版本覆盖。批量单位 50 文件：既缩短 `open_uris` 锁持有窗口（单批 merge ~5ms），也让 status-bar 进度更新粒度更细 | `lib.rs::run_workspace_scan`（merge block） |
| **磁盘 Summary 缓存** | 三维失效：`schema_version` / `exe_mtime_ns` / 每文件 `content_hash`；热缓存下跳过 summary 重建；默认纯内存模式（`cacheMode = "memory"`） | `summary_cache.rs` |
| **增量 reparse** | `tree.edit(&InputEdit)` + `parse(new, Some(old))`，未变区域子树原地复用 | `lib.rs::parse_and_store_with_old_tree` |
| **诊断优先级调度** | 单一 `DiagnosticScheduler`：Hot / Cold 双 `VecDeque` + tombstone 升级 + 单消费者 + supervisor 自愈；300ms debounce + per-URI gen 去重；seed_bulk 冷启动批量入队（`mylua.diagnostics.scope` 控制 Full / OpenOnly） | `diagnostic_scheduler.rs`，`lib.rs::consumer_loop` |
| **级联精细化** | 签名指纹不变不级联；`require_by_return` + `type_dependants` 双向反向索引覆盖 require 与 Emmy 类型依赖；级联扇出按 scope 过滤 | `aggregation.rs`，`lib.rs::parse_and_store_with_old_tree` |
| **并发安全** | per-URI `Arc<tokio::sync::Mutex<()>>` 编辑锁；`did_close` 在锁内读磁盘防御 TOCTOU；`edit_locks` 仅 DELETE 时清理；锁顺序 `edit_locks` → `open_uris` → `documents` → `index` → `scheduler.inner` | `lib.rs::edit_locks` |
| **位置编码** | UTF-16 ↔ byte 严谨（中文/emoji 安全），`util::byte_col_to_utf16_col` / `utf16_col_to_byte_col` 全链路统一 | `util.rs` |
| **文件过滤** | `workspace.include` / `workspace.exclude` glob 全链路生效（冷启动扫描 + 增量变更） | `workspace_scanner.rs` |
| **索引状态机** | `Initializing` → `Ready`；未 Ready 时 consumer 500ms 轮询；Ready 后 seed_bulk 自动补发 | `lib.rs::IndexState`，`consumer_loop` |
| **`did_open` fast path** | text 与索引字节一致即跳过 parse / 重建 summary，仅 `open_uris.insert` + `schedule(Hot)` 后 return；与统一诊断路径（`consumer_loop` 为唯一 publisher）配合消除 close→reopen 同内容时的视觉闪烁 | `lib.rs::did_open` |
| **统一诊断发布路径** | 稳态：syntax + semantic 都经 `DiagnosticScheduler → consumer_loop` 合并后一次 `publishDiagnostics`；取代过去 `parse_and_store` 里无条件 syntax-only 立即 spawn 的两步发布模型 | `lib.rs::consumer_loop` |
| **冷启动期 syntax-only 抢跑** | `did_open` / `did_change` 在 `IndexState != Ready` 且 `open_uris.contains(uri)` 时立即发一次 syntax-only 快照，填补任务 1 后台化 scan 期间的"诊断真空"。Ready 后 no-op，consumer_loop 接管。因 Ready 前 consumer_loop 从未 publish 过该 URI，不存在 9→3→9 flicker 风险 | `lib.rs::publish_syntax_only_during_indexing` |
| **`did_close` fast path** | 磁盘内容 == 内存文本时直接 return，不 reparse 不 republish，避免 preview tab 抖动 | `lib.rs::did_close` |

---

## 2. 对 5 万文件目标的已知瓶颈

按"冷启动 / 稳态 / 查询"三大阶段梳理，所有瓶颈都是可测量的具体问题。

### 2.1 冷启动阶段

#### 瓶颈 1：`cache.load_all()` 同步阻塞

```text
// lib.rs run_workspace_scan
let cached_summaries = Arc::new(cache.as_ref().map_or_else(HashMap::new, |c| c.load_all()));
```

`load_all()` 是同步 IO，一次把 `.vscode/.cache-mylua-lsp/` 下全部 bincode 文件反序列化进内存。当前 `run_workspace_scan` 已经 `tokio::spawn` 到后台、不再阻塞 `initialized`，但 `load_all()` 本身仍在 `async fn` 的 call stack 上同步执行——5 万个 bincode 小文件走一遍反序列化仍可能占后台任务线程 1–5s 不 yield；期间消费者 `consumer_loop` 的"Ready 门槛前 sleep 500ms 轮询"会正常推进，但磁盘 IO 与 tree-sitter parse 共享 tokio blocking 池时有额外排队成本。

**改进方向**：把 `load_all()` 改到独立的 `spawn_blocking` 线程中与其他 parse 批次并行；或按需流式读取（文件被打开/索引命中时才 load 对应 summary）。

### 2.2 查询阶段

#### 瓶颈 2：`references` 查询是**全量线性扫**

Emmy 类型引用的收集（`references::find_references`）对所有索引文件的 `emmy_comment` / `comment` 节点做文本匹配 + ASCII 词边界判断。5 万文件级别单次 references 查询可能进入秒级，用户侧可感知延迟。

**改进方向**：在 `build_summary` 阶段就构建 `ReferenceIndex: symbol_name → Vec<(uri, range)>`，随 summary 持久化；查询 O(log N) 命中。

---

## 3. 已接受的设计权衡（不计划优化）

下列"看起来像瓶颈"的点，经过权衡后被**明确纳入设计契约**，不会作为优化对象。它们的代价是已知且可预期的。

### 3.1 全工作区 `text + tree + scope_tree` 常驻内存

**事实**：`run_workspace_scan` 把**所有**匹配 `workspace.include/exclude` 的 `.lua` 文件，连同 tree-sitter Tree、ScopeTree、源码文本，全部装进 `self.documents`，不区分是否被编辑器打开，也不做 LRU 驱逐。

**估算**（5 万文件，平均 5KB 源码）：

| 组成 | 估算占用 |
|------|----------|
| 纯文本 `text: String` | ~250 MB |
| `tree: tree_sitter::Tree` | ~500 MB – 1 GB（tree-sitter tree 一般是源码 2–5x） |
| `scope_tree: ScopeTree` | ~100–300 MB |
| Summary / Aggregation 索引 | ~150–300 MB |
| **合计** | **~1.5–3 GB RSS** |

**为什么不做 LRU / 懒 parse**：

- **goto / hover / references / diagnostics 都需要任意文件的语法树与 scope**——不是只对"当前打开 tab"运行的能力。
- 一旦驱逐未打开文件的 Document，首次跨文件跳转（`definition` / `references` / 级联诊断扇出）会触发 on-demand tree-sitter parse + `build_scope_tree`，典型 5–50ms/文件，折合到"定义跳到未打开的大文件"就是**用户可感知的卡顿**。
- rust-analyzer / clangd 走 LRU 是因为 C++/Rust 的 AST + type check 体量比 Lua 大一到两个数量级，权衡不同。对 Lua 而言"全部 tree 常驻"的内存成本仍在可接受量级，换来的是"任何跨文件操作都恒定低延迟"。

**代价（明确承担）**：

- 5 万文件级别峰值 RSS 1.5–3 GB。
- 冷启动必须把所有文件 tree-sitter parse 一遍（即便 summary 磁盘缓存命中，`tree_sitter::Tree` 不可序列化，仍需重 parse）；5 万 × 5KB × 200μs/KB ≈ 50–100s 纯 parse CPU，rayon 并行到 8 核 10s+。
- 但**冷启动已经完全后台化**（见 §6 变更简史末两行），期间 `did_open` / `did_change` 通过 `publish_syntax_only_during_indexing` 抢跑 syntax-only 诊断，用户侧不存在"黑屏等 Ready"窗口。

**不做的事一览**：

- ❌ `documents` LRU / 容量上限
- ❌ 未打开文件懒 parse（on-demand 构建 tree）
- ❌ Tree 分级 retain policy（按文件重要性驱逐）

---

## 4. 规模分级实测结论

综合上述瓶颈与设计权衡，按规模给出可预期的性能表现：

| 规模 | 评估 | 说明 |
|------|------|------|
| 小型（< 1,000 文件） | **优秀** | 冷启动 < 2s；增量编辑 < 100ms 响应；体感丝滑；内存 ~100MB |
| 中型（1,000 – 10,000） | **良好** | 冷启动 5–20s（后台化，不阻塞 UI）；增量编辑 < 200ms；内存 ~500MB；Problems 面板无压力 |
| 大型（10,000 – 50,000） | **Ready 前后均可用** | 冷启动 parse CPU 总量 30s–2min（后台 rayon 并行）；内存 1.5–3GB（§3.1 设计契约）；冷启动期间打开 tab 有 syntax-only 诊断抢跑，Ready 后合并成完整诊断；**稳态增量编辑流畅**（debounce + 级联 + incremental reparse + Hot 调度都已到位） |
| 目标规模（50,000+） | **基本达成**（瓶颈 2 待优化） | 内存与冷启动 parse 成本按 §3.1 契约承担；剩余真实短板是 references 秒级查询（瓶颈 2，可优化） |

> **关键判断**：
> - **"稳态增量"与"冷启动期 syntax-only 抢跑 + Ready 后合并诊断"是当前架构的强项**。
> - **"全内存驻留"是主动选择而非意外**：换来"任何跨文件跳转恒定低延迟"，代价是 RSS 与冷启动 parse CPU 按线性放大（§3.1）。
> - **真实未解决短板**：`references` 全量线性扫（瓶颈 2），5 万规模下用户可感知。

---

## 5. 优化路线图（按性价比排序）

> **不在本路线图内**：参见 §3「已接受的设计权衡」。`documents` LRU / 懒 parse / on-demand tree 重建均**不计划实施**。

### Tier 1 — 低垂果实（预计每项半天内完成，收益明显）

| 序号 | 项目 | 对应瓶颈 | 预期收益 |
|------|------|----------|----------|
| T1-1 | `cache.load_all()` 搬到 `spawn_blocking` | 瓶颈 1 | 后台 scan task 内部 IO 不再占用 tokio worker，与 parse 批次真正并行 |

### Tier 2 — 架构调整（每项 1–3 天，收益大但需谨慎设计）

| 序号 | 项目 | 对应瓶颈 | 预期收益 |
|------|------|----------|----------|
| T2-1 | References 反向索引持久化 | 瓶颈 2 | References 查询延迟秒级 → 毫秒级 |

### Tier 3 — 高级优化（项目稳定后再考虑）

- Summary 增量落盘（每文件独立 bincode，而非 `save_all` 一次性写）
- Aggregation 层（`global_shard` / `type_shard` / `require_by_return` / `type_dependants`）也持久化，冷启动跳过重建
- 冷启动分段调度（先索引 open tabs，Ready 后台续扫其余）——注意：这只影响 Ready 时序与诊断 seed 顺序，不改变"全部文件最终都进 `documents`"的设计契约

---

## 6. 已落地变更简史

下列历史瓶颈已有系统性改造，保留在此仅做线索用，不再出现在 §2 瓶颈清单中：

| 原瓶颈 | 改造方案 | 落地 commit / 文档 |
|--------|----------|--------------------|
| 初始诊断单线程串行 compute | `DiagnosticScheduler` 单消费者 + Hot/Cold 优先级队列；打开的 tab 走 Hot 先行，closed 文件按 scope 决定是否 seed | `diagnostic_scheduler.rs`、`lib.rs::consumer_loop` |
| 冷启动 publishDiagnostics 发全工作区 | `mylua.diagnostics.scope`（`full` / `openOnly`）控制 seed 范围与级联扇出 | 同上 |
| `did_open` 无条件重 parse | fast path：`open_uris.contains` && text 一致即跳过；对称于 `did_close` fast path | `lib.rs::did_open` |
| `did_close` → `did_open`（内容未变）出现诊断 9→3→9 视觉闪烁 | 删除 `parse_and_store` 里 syntax-only 的立即 spawn，让 `consumer_loop` 成为唯一发布者（合并发 syntax + semantic）；`did_open` fast path 改为 `text == buffer` 即跳过（去掉 `is_tracked_open` 门槛），跳过时仍 `schedule(Hot)` 保证 consumer 至少处理一次。代价：`did_change` 的 syntax 错误从 ~10ms 推迟到 300ms debounce 后出现 | `lib.rs::did_open`、`lib.rs::parse_and_store_with_old_tree`、`lib.rs::consumer_loop` |
| `initialized` `.await` workspace scan，冷启动期间所有 LSP 请求排队 | 把 `scan_workspace_parallel` 方法重构为自由函数 `run_workspace_scan` + `tokio::spawn` 后台执行；`initialized` 立即返回，让 `did_open`/hover/completion/semanticTokens 与 scan 并行；merge 批次整段持 `open_uris` 锁强制与 `did_open` 严格前后排序，避免 disk 版本覆盖未保存 buffer；`scheduler.seed_bulk(…)` 迁到 scan 完成后（`IndexState::Ready` 之后）。代价：单个 batch merge 期间（~10-20ms）并发 `did_open` 会被 `open_uris` 锁挡住一个批次周期，冷启动窗口可观测但不影响正确性 | `lib.rs::initialized`、`lib.rs::run_workspace_scan` |
| scan 后台化后冷启动期用户打开/编辑文件看不到任何诊断（consumer_loop 被 IndexState gate 挡住） | `did_open` / `did_change` 新增 `publish_syntax_only_during_indexing(&uri).await`：双条件 gate（`IndexState != Ready` + `open_uris.contains`）下立即发 syntax-only 快照（tree-sitter ERROR/MISSING 节点）；`apply_diagnostic_suppressions` 与 consumer_loop 一致；`Ready` 后 no-op，由 consumer_loop 接管合并 publish。因 Ready 前 consumer_loop 从未 publish 过此 URI，不会出现 9→3→9 flicker 回退 | `lib.rs::publish_syntax_only_during_indexing`、`lib.rs::did_open`、`lib.rs::did_change` |

---

## 7. 如何度量

做优化前后，建议统一测量下列指标，方便横向对比：

| 指标 | 采集方式 |
|------|----------|
| **Cold-start to Ready**（首次扫描 → `IndexState::Ready`） | `initialized` 开始到 `scan complete` 日志的 wall-clock |
| **Cold-start to First Diagnostic**（冷启动到用户看到第一个 tab 的诊断） | 客户端侧 `publishDiagnostics` 接收时间 |
| **Peak RSS**（进程驻留内存峰值） | macOS Activity Monitor / Linux `ps -o rss=` |
| **Incremental edit latency**（从 `did_change` 到 `publishDiagnostics`） | LSP 消息日志时间差 |
| **References query latency** | client 侧请求发出到响应接收 |
| **Cache hit ratio** | 日志 `[mylua-lsp] cache hits: X/Y` |

测试 fixture 建议准备三档：
- 小：`tests/lua-root/` 本身（~20 文件）
- 中：`vscode-extension/assets/lua5.4/` + 若干合成（~200 文件）
- 大：程序化生成的 5k / 10k / 50k 合成工作区（全随机 EmmyLua class + require 链）

---

## 8. 与需求文档的对齐

- [`requirements.md`](requirements.md) 声明了全工作区能力（定义/引用/符号）与 5 万文件目标
- [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md) 描述了数据模型与冷启动/增量流程
- 本文是**现状评估**，对 `implementation-roadmap.md` 里阶段 D 完成之后的"性能窗口期"的具体目标与风险做披露
- 具体待办一旦确认要实施，按 [`future-work.md`](future-work.md) 的模板追加条目

---

**维护提示**：若对冷启动路径、`documents` 生命周期、诊断调度策略做了实质性调整，请同步更新本文件的相关章节（特别是 §2 瓶颈条目、§3 设计权衡、§4 规模分级表与 §6 变更简史）。若将来**反悔** §3.1 的"全内存驻留"决策要引入 LRU，需要先在 §3 标注并同步更新 `ai-readme.md` 的架构描述。
