# 性能现状分析与优化方向

> **状态**：基于当前实现（`lsp/crates/mylua-lsp/`）在 2026-04 的代码快照做的分维度评估。架构亮点、已知瓶颈、规模分级与优化路线按优先级排序。
>
> **目标对齐**：[`../ai-readme.md`](../ai-readme.md) 声明"需要支持 5 万个 lua 文件级别"；本文以 5 万文件作为性能对标上限。

---

## 1. 做得扎实的部分（架构亮点）

这些是项目当前性能的基础，属于"见过不少野路子 LSP 才会知道值钱"的设计。

| 维度 | 现状 | 参考位置 |
|------|------|----------|
| **并行冷启动** | `rayon` + 200 文件/批，真·多核并行解析 + 生成 summary | `lib.rs::scan_workspace_parallel` |
| **磁盘 Summary 缓存** | 四维失效：`schema_version` / `exe_mtime_ns` / `config_fingerprint` / 每文件 `content_hash`；热缓存下跳过 summary 重建 | `summary_cache.rs` |
| **增量 reparse** | `tree.edit(&InputEdit)` + `parse(new, Some(old))`，未变区域子树原地复用 | `lib.rs::parse_and_store_with_old_tree` |
| **诊断调度** | 300ms debounce + per-URI generation counter 去重，编辑风暴下不重复计算 | `lib.rs::schedule_semantic_diagnostics` |
| **级联精细化** | 签名指纹不变不级联；`require_by_return` + `type_dependants` 双向反向索引覆盖 require 与 Emmy 类型依赖 | `aggregation.rs` |
| **并发安全** | per-URI `Arc<tokio::sync::Mutex<()>>` 编辑锁；`did_close` 在锁内读磁盘防御 TOCTOU；`edit_locks` 仅 DELETE 时清理 | `lib.rs::edit_locks` |
| **位置编码** | UTF-16 ↔ byte 严谨（中文/emoji 安全），`util::byte_col_to_utf16_col` / `utf16_col_to_byte_col` 全链路统一 | `util.rs` |
| **文件过滤** | `workspace.include` / `workspace.exclude` glob 全链路生效（冷启动扫描 + 增量变更） | `workspace_scanner.rs` |
| **索引状态机** | `Initializing` → `Ready`；未 Ready 时跳过语义诊断避免假阳性，Ready 后自动补发 | `lib.rs::IndexState` |
| **`did_close` fast path** | 磁盘内容 == 内存文本时直接 return，不 reparse 不 republish，避免 preview tab 抖动 | `lib.rs::did_close` |

---

## 2. 对 5 万文件目标的已知瓶颈

按"冷启动 / 稳态 / 查询"三大阶段梳理，所有瓶颈都是可测量的具体问题。

### 2.1 冷启动阶段

#### 瓶颈 1：初始诊断是**单线程串行 compute**

`publish_diagnostics_for_open_files` 在 `scan_workspace_parallel` 完成后被 `initialized` 调用，行为是：

- 遍历 `self.documents.keys()`（冷启动后已是**全工作区**文件）
- **在当前 task 里顺序**对每个文件计算 `collect_diagnostics` + `collect_semantic_diagnostics_with_version` + `apply_diagnostic_suppressions`
- 每个文件都要 lock `self.index`，其他 LSP 请求期间被挡住
- 只有最后 `publish_diagnostics(...)` 这个 notification 的发送用 `tokio::spawn` fire-and-forget

**估算**：单文件平均 1–5ms semantic 诊断，5 万 × 平均 3ms ≈ **150s 串行**。

**改进方向**：`par_iter` 并行 compute + 批量 publish；长期上只对已 `did_open` 的 URI 做这件事，其他文件按需触发。

#### 瓶颈 2：`cache.load_all()` 同步阻塞

```text
// lib.rs scan_workspace_parallel
let cached_summaries = Arc::new(cache.as_ref().map_or_else(HashMap::new, |c| c.load_all()));
```

`load_all()` 是同步 IO，一次把 `.vscode/.cache-mylua-lsp/` 下全部 bincode 文件反序列化进内存。**在 `initialized` 这个 async fn 的 call stack 上直接执行**，没 spawn 任何后台任务。

5 万次小文件 IO + 反序列化 → 几秒到十几秒，期间 `initialized` future 不 yield，client 表现为"卡住没响应"。

**改进方向**：搬进 `tokio::task::spawn_blocking` 后台加载；或按需流式读取（文件被打开/索引命中时才 load 对应 summary）。

#### 瓶颈 3：tree-sitter Tree 无法序列化 → 每次冷启动全量 reparse

即使 summary 命中磁盘缓存，**每个文件仍必须重新 tree-sitter parse 一次**（因为 `Document` 结构体要求 `tree` 字段）：

```text
// lib.rs scan_workspace_parallel (cache-hit path)
let tree = parser.parse(text.as_bytes(), None)?;
let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
```

5 万文件按平均 5KB × 200μs/KB ≈ **50–100s** 纯 parse CPU 时间（即便 rayon 并行到 8 核也要 10s+）。

**改进方向**：懒 parse —— summary 热缓存下**不进 `documents`**，只在文件被打开 / 被 goto / 被 hover 查询到时 on-demand parse 并进 `documents`。

#### 瓶颈 4：冷启动后 publishDiagnostics **发全工作区**

紧接瓶颈 1，即便 client 只开了 1 个 tab，server 冷启动完成后仍会发送 ~5 万条 `textDocument/publishDiagnostics` notification。

- 传输层：每条 JSON-RPC 消息都要序列化、写 stdio、client 反序列化
- Client 层：VS Code Problems 面板会被塞爆；文件资源管理器徽章全量刷新；大内存占用

**改进方向**：冷启动只对已 `did_open` 的 URI 发诊断；未打开文件等自然触发（`did_open` / 依赖级联）。

### 2.2 稳态阶段

#### 瓶颈 5：`self.documents` 全工作区常驻内存

`scan_workspace_parallel` 把**所有**文件的 `text + tree + scope_tree` 都塞进 `documents`，不区分打不打开。

**估算**（5 万文件，平均 5KB 源码）：

| 组成 | 估算占用 |
|------|----------|
| 纯文本 `text: String` | ~250 MB |
| `tree: tree_sitter::Tree` | ~500 MB – 1 GB（tree-sitter tree 一般是源码 2–5x） |
| `scope_tree: ScopeTree` | ~100–300 MB |
| Summary / Aggregation 索引 | ~150–300 MB |
| **合计** | **~1.5–3 GB RSS** |

rust-analyzer / clangd 的通行做法是**只给打开文件留完整 Document，未打开文件只留 summary/index**；我们现在是"全开"，对 5 万规模是显式悬崖。

**改进方向**：`documents` 改为 LRU + 懒加载；配合瓶颈 3 的改造天然成立。

#### 瓶颈 6：`did_open` 无条件重 parse — ✅ **已落地（T1-1）**

曾经即便 `documents[uri].text` 和 `params.text_document.text` 完全相同，也会走一遍完整 parse + build_summary + upsert_summary + publish_diagnostics 流水线——VS Code preview-tab 单击切换场景下，每次切 tab 都是一次纯浪费。

**落地方案**：对称于 `did_close` 的 fast path，在 `did_open` 开头判断 `documents[uri].text == params.text_document.text`，相等则直接 return。**只比较 text 不比较 version**（LSP version 不与内容强关联，client 重开时会重置）；不清 `documents` / `edit_locks` / `diag_gen` / `semantic_tokens_cache`，保持所有缓存复用；配合冷启动把全工作区索引进 `documents` 的行为，用户**首次**打开一个未修改文件也能命中。

当前实现位置：`lib.rs::did_open`。

### 2.3 查询阶段

#### 瓶颈 7：`references` 查询是**全量线性扫**

Emmy 类型引用的收集（`references::find_references`）对所有索引文件的 `emmy_comment` / `comment` 节点做文本匹配 + ASCII 词边界判断。5 万文件级别单次 references 查询可能进入秒级，用户侧可感知延迟。

**改进方向**：在 `build_summary` 阶段就构建 `ReferenceIndex: symbol_name → Vec<(uri, range)>`，随 summary 持久化；查询 O(log N) 命中。

---

## 3. 规模分级实测结论

综合上述瓶颈，按规模给出可预期的性能表现：

| 规模 | 评估 | 说明 |
|------|------|------|
| 小型（< 1,000 文件） | **优秀** | 冷启动 < 2s；增量编辑 < 100ms 响应；体感丝滑 |
| 中型（1,000 – 10,000） | **良好** | 冷启动 5–20s；增量编辑仍 < 200ms；内存 ~500MB；Problems 面板无压力 |
| 大型（10,000 – 50,000） | **刚及格** | 冷启动 30s – 3min（主要消耗在瓶颈 1+3）；内存 1–3GB；首次打开 Problems 面板要等较久；**但 Ready 后的增量编辑仍然流畅**（debounce + 级联 + incremental reparse 都已到位） |
| 目标规模（50,000+） | **未达成** | 瓶颈 1–4 显式化，需要专门优化阶段 |

> **关键判断**：**"稳态增量"是当前架构的强项**；**"冷启动"和"未打开文件的内存占用"是当前架构的短板**。

---

## 4. 优化路线图（按性价比排序）

### Tier 1 — 低垂果实（预计每项半天内完成，收益明显）

| 序号 | 项目 | 对应瓶颈 | 预期收益 | 状态 |
|------|------|----------|----------|------|
| T1-1 | `did_open` fast path | 瓶颈 6 | preview tab 切换 0 开销；消除"每次打开重诊断"的用户可感问题 | ✅ 已完成 |
| T1-2 | `publish_diagnostics_for_open_files` rayon 并行 | 瓶颈 1 | 冷启动诊断 compute 时间按核数线性下降（8 核 ≈ 8x） | 待做 |
| T1-3 | `cache.load_all()` 搬到 `spawn_blocking` | 瓶颈 2 | `initialized` 不再阻塞，client 冷启动响应立刻就绪 | 待做 |
| T1-4 | 冷启动 publishDiagnostics 只发 open tabs | 瓶颈 4 | JSON-RPC 流量 5 万 → O(打开 tab 数)；Problems 面板即时可用 | 待做 |

### Tier 2 — 架构调整（每项 1–3 天，收益大但需谨慎设计）

| 序号 | 项目 | 对应瓶颈 | 预期收益 |
|------|------|----------|----------|
| T2-1 | `documents` 懒加载 + LRU | 瓶颈 3 + 瓶颈 5 | 内存峰值从 1–3GB 降到 ~500MB；冷启动 parse 阶段可跳过 |
| T2-2 | References 反向索引持久化 | 瓶颈 7 | References 查询延迟秒级 → 毫秒级 |

> **T2-1 备注**：T1-1 的 fast path 命中率强依赖"冷启动把所有 .lua 文件都装进 `documents`"。落地 T2-1（`documents` 变 LRU / 懒加载）后，未被 LRU 命中的文件首次 `did_open` 必 miss fast path 回到全量 parse 路径。需同步评估 T1-1 的实际命中模型与用户体验。

### Tier 3 — 高级优化（项目稳定后再考虑）

- Summary 增量落盘（每文件独立 bincode，而非 `save_all` 一次性写）
- Aggregation 层（`global_shard` / `type_shard` / `require_by_return` / `type_dependants`）也持久化，冷启动跳过重建
- Tree 分级 retain policy（重要文件 vs. 普通文件不同 LRU 策略）
- 冷启动分段调度（先索引 open tabs，Ready 后台续扫其余）

---

## 5. 如何度量

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
- 中：`assets/lua5.4/` + 若干合成（~200 文件）
- 大：程序化生成的 5k / 10k / 50k 合成工作区（全随机 EmmyLua class + require 链）

---

## 6. 与需求文档的对齐

- [`requirements.md`](requirements.md) 声明了全工作区能力（定义/引用/符号）与 5 万文件目标
- [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md) 描述了数据模型与冷启动/增量流程
- 本文是**现状评估**，对 `implementation-roadmap.md` 里阶段 D 完成之后的"性能窗口期"的具体目标与风险做披露
- 具体待办一旦确认要实施，按 [`future-work.md`](future-work.md) 的模板追加条目

---

**维护提示**：若对冷启动路径、`documents` 生命周期、诊断调度策略做了实质性调整，请同步更新本文件的相关章节（特别是瓶颈条目和规模分级表）。
