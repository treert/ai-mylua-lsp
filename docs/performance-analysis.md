# 性能分析

> **目标**：支持 5 万个 Lua 文件级别（见 [`ai-readme.md`](../ai-readme.md)）。
>
> **核心设计契约**：全工作区 `text + tree + scope_tree` 常驻内存，不做 LRU / 懒 parse。代价是 5 万文件量级 RSS ~1.5–3GB，收益是任何跨文件跳转恒定低延迟。

---

## 1. 架构性能亮点

详细实现见 [`architecture.md`](architecture.md) 和 [`index-architecture.md`](index-architecture.md)。

| 维度 | 要点 |
|------|------|
| 并行冷启动 | 5 阶段流水线，rayon 全量并行 parse，后台化不阻塞 UI |
| 增量 reparse | `tree.edit` + `parse(new, Some(old))`，未变区域子树复用 |
| 诊断调度 | Hot/Cold 双队列 + 300ms debounce + per-URI gen 去重 |
| 级联精细化 | 签名指纹不变不级联；双向反向索引覆盖 require 与类型依赖 |
| 磁盘缓存 | 三维失效（schema_version / exe_mtime / content_hash） |
| Fast path | `did_open`/`did_close` 内容未变时跳过 reparse |
| 冷启动抢跑 | Ready 前打开的文件立即发 syntax-only 诊断 |
| 模块解析 | last-segment 索引 + 最长后缀匹配，O(1) 查找无 fallback |

---

## 2. 已知瓶颈

### 2.1 `cache.load_all()` 同步阻塞

`run_workspace_scan` 中 `cache.load_all()` 同步反序列化所有 bincode 文件。5 万文件时可能占后台线程 1–5s 不 yield。

**改进方向**：搬到 `spawn_blocking` 与 parse 批次并行；或按需流式读取。

### 2.2 `references` 全量线性扫

`find_references` 对所有索引文件做文本匹配。5 万文件级别单次查询可能进入秒级。

**改进方向**：`build_summary` 阶段构建 `ReferenceIndex: symbol_name → Vec<(uri, range)>`，查询 O(log N)。

---

## 3. 设计权衡：全内存驻留

全工作区 `text + tree + scope_tree` 常驻 `documents`，不做 LRU 驱逐。

**5 万文件内存估算**（平均 5KB 源码）：

| 组成 | 估算 |
|------|------|
| 源码文本 | ~250 MB |
| tree-sitter Tree | ~500 MB – 1 GB |
| ScopeTree | ~100–300 MB |
| Summary / 索引 | ~150–300 MB |
| **合计** | **~1.5–3 GB** |

**不做 LRU 的理由**：goto / hover / references / 级联诊断都需要任意文件的语法树。on-demand parse 会导致跨文件跳转 5–50ms 可感知卡顿。Lua AST 体量远小于 C++/Rust，全驻留内存成本可接受。

**明确不做**：documents LRU、未打开文件懒 parse、Tree 分级驱逐。

---

## 4. 规模分级预期

| 规模 | 评估 | 说明 |
|------|------|------|
| < 1K 文件 | 优秀 | 冷启动 < 2s，内存 ~100MB |
| 1K – 10K | 良好 | 冷启动 5–20s（后台化），内存 ~500MB |
| 10K – 50K | 可用 | 冷启动 30s–2min（后台 rayon 并行），内存 1.5–3GB |
| 50K+ | 基本达成 | 瓶颈 2（references）待优化 |

---

## 5. 优化路线图

### Tier 1 — 低垂果实（半天内）

| 项目 | 瓶颈 | 预期收益 |
|------|------|----------|
| `cache.load_all()` → `spawn_blocking` | §2.1 | IO 不占 tokio worker，与 parse 并行 |

### Tier 2 — 架构调整（1–3 天）

| 项目 | 瓶颈 | 预期收益 |
|------|------|----------|
| References 反向索引持久化 | §2.2 | 查询延迟秒级 → 毫秒级 |

### Tier 3 — 高级优化（项目稳定后）

- Summary 增量落盘（每文件独立 bincode）
- Aggregation 层持久化，冷启动跳过重建
- 冷启动分段调度（先索引 open tabs）

---

## 6. 度量方法

| 指标 | 采集方式 |
|------|----------|
| Cold-start to Ready | `initialized` → `scan complete` 日志 wall-clock |
| Peak RSS | macOS Activity Monitor / Linux `ps -o rss=` |
| Incremental edit latency | `did_change` → `publishDiagnostics` 日志时间差 |
| References query latency | client 侧请求→响应时间 |

另有独立 CLI 性能分析工具，见 [`../lsp/README.md`](../lsp/README.md) 中 `mylua-perf` 说明。

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
- 本文是**现状评估**，对所有规划阶段完成之后的"性能窗口期"的具体目标与风险做披露
- 具体待办一旦确认要实施，按 [`future-work.md`](future-work.md) 的模板追加条目

---

**维护提示**：若对冷启动路径、`documents` 生命周期、诊断调度策略做了实质性调整，请同步更新本文件的相关章节（特别是 §2 瓶颈条目、§3 设计权衡、§4 规模分级表与 §6 变更简史）。若将来**反悔** §3.1 的"全内存驻留"决策要引入 LRU，需要先在 §3 标注并同步更新 `ai-readme.md` 的架构描述。
