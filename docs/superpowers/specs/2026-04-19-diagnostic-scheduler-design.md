# 诊断调度器（DiagnosticScheduler）设计

**状态**：Draft · 待用户 review
**日期**：2026-04-19
**关联问题**：[`docs/performance-analysis.md`](../../performance-analysis.md) 瓶颈 1 / 瓶颈 4
**路线图位置**：T1-2（冷启动诊断并行化）+ T1-4（冷启动只对 open tabs publish）的合并替代方案

---

## 1. 背景与动机

### 1.1 现状

当前诊断调度分散在三处，缺少统一的优先级管理：

| 触发 | 调用 | 行为 |
|------|------|------|
| `did_open` / `did_change` 完成 | `schedule_semantic_diagnostics(uri, version)` | spawn 一个 tokio task，sleep 300ms，gen 过滤后 compute + publish |
| 级联依赖 | 同上 | 对每个 `dep_uri` 调一次 |
| 冷启动 `initialized` 末尾 | `publish_diagnostics_for_open_files()` | **对 documents 所有 URI 串行 compute**，再 `tokio::spawn` fire-and-forget publish |

**痛点**：

- 冷启动发诊断的覆盖范围是"全工作区"，而非真正"已打开的 tab"——函数名带 `open_files` 但实现遍历 `documents.keys()`
- 冷启动 compute 是**单线程串行**（阻塞当前 task），且阻塞期间 `index` 锁占用；5 万文件估算串行耗时 >150s
- 已打开的 tab 没有优先级——它们在冷启动队列里和其他文件混在一起，按 HashMap 迭代顺序被处理（接近随机）
- 没有"当前打开文件集合"的显式状态，T1-1 fast path 无法利用此集合做精确过滤

### 1.2 目标

用一个统一的、优先级感知的调度器接替上述三条路径，满足：

1. 打开的文件严格优先于未打开的
2. 消费端单线程，CPU 使用可控
3. 现有 300ms debounce 语义（连敲合并、取消过期）保留
4. 冷启动范围可配置（全量 / 仅打开）
5. T1-1 fast path 修正为"已打开 + 文本相等才跳过"，避免"冷启动后首次打开文件无诊断"

### 1.3 非目标（YAGNI）

- **多消费者并行**：明确单线程；若以后性能瓶颈再评估
- **cooperative cancellation**：compute 进行中不做中断（Rust 侧改造成本高），靠"发布前 text 一致性检查"做结果丢弃
- **priority upgrade 的 O(1) 实现**：用 tombstone 方案，允许 cold 队列里残留作废项，pop 时 skip
- **slow-pass 速率限制**（如"每 50ms 一个文件"）：单消费者 + compute 本身耗时已是天然节流
- **syntax 诊断走队列**：syntax 保持即时发布（parse 完立即 publish），只 semantic 诊断走队列
- **跨 DocumentSummary 指纹级联的扩展**（未打开文件在 openOnly 下不级联）：openOnly 模式级联只惠及已打开的依赖方

---

## 2. 决策清单（已与用户确认）

| # | 决策 | 取舍 |
|---|------|------|
| D1 | 冷启动诊断范围配置化，默认 `"full"` | 新增 `diagnostics.scope: "full" \| "openOnly"` |
| D2 | syntax 诊断保持即时，semantic 走队列 | syntax 开销小、即时反馈对用户友好 |
| D3 | 队列结构：hot/cold 双 `VecDeque` + `HashMap<Uri, Priority>` 去重 + tombstone 集合 | 优先级升级无需 O(n) 搬队 |
| D4 | 生产者侧 debounce（300ms + gen 过滤），消费者即时 | 消费者不被 sleep 拖累，单线程吞吐最大化 |
| D5 | 消费者侧做"text 一致性检查"作第二道防线 | compute 期间被编辑则丢弃 publish |
| D6 | openOnly 模式下，级联只惠及已打开的依赖方 | 与 scope 语义一致；未打开文件等用户打开时 did_open 自然重算 |
| D7 | consumer task panic 自动重启 | 避免整个 LSP 瘫痪 |

---

## 3. 架构概览

### 3.1 组件

新增模块 `lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs`，单职责：**接收待诊断的 URI，按优先级单线程消化**。

```text
DiagnosticScheduler
├─ 状态（Arc<Mutex<Inner>>）
│  ├─ hot: VecDeque<Uri>              — 已打开文件队列
│  ├─ cold: VecDeque<Uri>             — 未打开文件队列
│  ├─ enqueued: HashMap<Uri, Priority> — 每 URI 当前所在优先级
│  ├─ cold_tombstones: HashSet<Uri>   — cold 里被升级到 hot 的作废项
│  └─ diag_gen: HashMap<Uri, u64>     — per-URI 代数，生产侧 debounce 使用
├─ notify: Arc<tokio::sync::Notify>    — 队列非空信号
└─ 对外 API
   ├─ schedule(uri, priority)          — 300ms debounce + 入队
   ├─ seed_bulk(uris, priority)        — 冷启动用，绕过 debounce
   ├─ invalidate(uri)                  — 文件删除时清理状态
   └─ start_consumer(...)              — 启动后台 consumer task
```

### 3.2 `Backend` 的改动

| 字段 | 变化 |
|------|------|
| `diag_gen: Arc<Mutex<HashMap<Uri, u64>>>` | 删除；语义移入 scheduler |
| `scheduler: Arc<DiagnosticScheduler>` | **新增** |
| `open_uris: Arc<Mutex<HashSet<Uri>>>` | **新增**；显式追踪 LSP did_open/did_close 状态 |

### 3.3 配置项

```jsonc
{
  "mylua.diagnostics.scope": "full"  // 或 "openOnly"
}
```

- `"full"`（默认）：冷启动 seed 全工作区（open → Hot，其他 → Cold）；级联惠及所有依赖方
- `"openOnly"`：冷启动 seed 仅 open_uris；级联只惠及 `open_uris.contains` 的依赖方；未打开文件的诊断靠 did_open 自然触发

---

## 4. 数据流

```text
 ┌──────────────┐                         ┌────────────────────────────┐
 │  did_open    │── schedule(uri,Hot)────▶│    DiagnosticScheduler     │
 │  did_change  │    (+ spawn 300ms       │                            │
 │  级联依赖    │     debounce task)      │  ┌──────────┐  ┌────────┐  │
 └──────────────┘                         │  │ hot: VD  │  │cold:VD │  │
                                          │  └──────────┘  └────────┘  │
 ┌──────────────┐                         │  ┌────────────────────────┐│
 │ initialized  │── seed_bulk(open,Hot)──▶│  │ enqueued: HashMap       ││
 │ (冷启动)     │   seed_bulk(rest,Cold)──│  │ cold_tombstones: HashSet││
 └──────────────┘    (openOnly 时跳过     │  │ diag_gen: HashMap       ││
                     rest)                │  └────────────────────────┘│
                                          │             │               │
                                          │             ▼               │
                                          │  ┌──────────────────────┐   │
                                          │  │ consumer_loop (1个)  │   │
                                          │  │  pop hot → cold      │   │
                                          │  │  skip tombstones     │   │
                                          │  │  snapshot text       │   │
                                          │  │  compute diagnostics │   │
                                          │  │  text 未变 → publish │   │
                                          │  └──────────────────────┘   │
                                          └────────────────────────────┘
                                                        │
                                                        ▼
                                               client.publishDiagnostics
```

---

## 5. 核心算法

### 5.1 `push` / `pop`（内部）

```rust
// 概要伪码，实际实现在 diagnostic_scheduler.rs
fn push(&self, uri: Uri, priority: Priority) {
    let mut inner = self.inner.lock().unwrap();
    match (inner.enqueued.get(&uri).copied(), priority) {
        (Some(Priority::Hot), _) => return,                 // 已是 Hot，最高
        (Some(Priority::Cold), Priority::Hot) => {          // 升级 Cold → Hot
            inner.cold_tombstones.insert(uri.clone());       // cold 里那份作废
            inner.hot.push_back(uri.clone());
            inner.enqueued.insert(uri, Priority::Hot);
        }
        (Some(Priority::Cold), Priority::Cold) => return,   // 已有同级
        (None, Priority::Hot) => {
            inner.hot.push_back(uri.clone());
            inner.enqueued.insert(uri, Priority::Hot);
        }
        (None, Priority::Cold) => {
            inner.cold.push_back(uri.clone());
            inner.enqueued.insert(uri, Priority::Cold);
        }
    }
    self.notify.notify_one();
}

fn pop(&self) -> Option<Uri> {
    let mut inner = self.inner.lock().unwrap();
    // --- Hot 严格优先 ---
    if let Some(u) = inner.hot.pop_front() {
        inner.enqueued.remove(&u);
        return Some(u);
    }
    // --- Cold 次之，跳过 tombstones ---
    while let Some(u) = inner.cold.pop_front() {
        if inner.cold_tombstones.remove(&u) { continue; }
        inner.enqueued.remove(&u);
        return Some(u);
    }
    None
}
```

**复杂度**：`push` O(1)，`pop` 均摊 O(1)，tombstone 会被顺序 pop 消耗。
**空间**：tombstones 最多 ≤ cold 历史峰值；实际编辑场景下典型 < 100。

### 5.2 生产者侧 debounce（`schedule`）

沿用现有 `diag_gen` 机制：

```rust
pub fn schedule(&self, uri: Uri, priority: Priority) {
    let gen = {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.diag_gen.entry(uri.clone()).or_insert(0);
        *entry += 1;
        *entry
    };

    let scheduler = Arc::clone(self);
    let uri_c = uri.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS)).await;

        // gen 过期说明期间又有 schedule，让新 task 做
        let latest = scheduler.inner.lock().unwrap()
            .diag_gen.get(&uri_c).copied().unwrap_or(0);
        if latest != gen { return; }

        scheduler.push_internal(uri_c, priority);
    });
}
```

### 5.3 冷启动 seed（`seed_bulk`）

冷启动不经 debounce。为避免 5 万次 `notify_one` 开销，批量入队后只 notify 一次：

```rust
pub fn seed_bulk(&self, uris: Vec<Uri>, priority: Priority) {
    let mut inner = self.inner.lock().unwrap();
    for uri in uris {
        // 复用 push 的分支逻辑，但不逐条 notify
        match (inner.enqueued.get(&uri).copied(), priority) {
            (Some(Priority::Hot), _) => continue,
            (Some(Priority::Cold), Priority::Hot) => {
                inner.cold_tombstones.insert(uri.clone());
                inner.hot.push_back(uri.clone());
                inner.enqueued.insert(uri, Priority::Hot);
            }
            (Some(Priority::Cold), Priority::Cold) => continue,
            (None, Priority::Hot) => {
                inner.hot.push_back(uri.clone());
                inner.enqueued.insert(uri, Priority::Hot);
            }
            (None, Priority::Cold) => {
                inner.cold.push_back(uri.clone());
                inner.enqueued.insert(uri, Priority::Cold);
            }
        }
    }
    drop(inner);
    self.notify.notify_one();  // 统一唤醒 consumer
}
```

`push` 内部逻辑（§5.1）可以抽出为共享 helper 避免重复，实际实现时统一即可。

### 5.4 消费者循环 + Panic 重启

单消费者是关键路径，panic 后必须自动重启避免整个 LSP 失去诊断能力。用 **supervisor pattern**：

```rust
/// 启动消费者（supervisor）：内部 loop 每次 panic 自动重启 inner task。
pub fn start_consumer(
    scheduler: Arc<DiagnosticScheduler>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    client: Client,
) {
    tokio::spawn(async move {
        loop {
            let s = scheduler.clone();
            let d = documents.clone();
            let i = index.clone();
            let c = config.clone();
            let st = index_state.clone();
            let cl = client.clone();

            let handle = tokio::spawn(async move {
                consumer_loop(s, d, i, c, st, cl).await
            });

            match handle.await {
                Ok(()) => {
                    // consumer_loop 正常返回（只在 shutdown 时发生）
                    break;
                }
                Err(e) if e.is_panic() => {
                    lsp_log!("[sched] consumer panicked: {:?}, restarting...", e);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(e) => {
                    lsp_log!("[sched] consumer task cancelled: {:?}", e);
                    break;
                }
            }
        }
    });
}

async fn consumer_loop(
    scheduler: Arc<DiagnosticScheduler>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    client: Client,
) {
    loop {
        // --- Ready 门槛（先于 pop）---
        // 必须在 pop 之前检查，否则 Hot URI 被 pop 后又"推回 Cold"会导致
        // 优先级降级。Not Ready 期间不动队列，仅轮询 state。
        // 500ms 轮询开销：冷启动 scan 最多几分钟，~几十次空转，可忽略。
        if *index_state.lock().unwrap() != IndexState::Ready {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        // --- 等待有任务 ---
        let uri = loop {
            if let Some(u) = scheduler.pop() { break u; }
            scheduler.notify.notified().await;
        };

        // --- Snapshot text（持 documents 锁最短时间）---
        let snapshot = {
            let docs = documents.lock().unwrap();
            let Some(doc) = docs.get(&uri) else { continue; };  // 文件已删
            doc.text.clone()
        };

        // --- Compute（内部会重新 lock documents + index）---
        // 注意：和现有 schedule_semantic_diagnostics 行为一致，compute
        // 本身不能完全无锁；尽量减少锁持有窗口，不跨 await 持锁
        let diags = compute_all_diagnostics(&uri, &documents, &index, &config);

        // --- 发布前一致性检查 ---
        let stale = {
            let docs = documents.lock().unwrap();
            match docs.get(&uri) {
                Some(doc) => doc.text != snapshot,
                None => true,  // 文件已删
            }
        };
        if stale { continue; }

        client.publish_diagnostics(uri, diags, None).await;
    }
}
```

**Panic 恢复行为**：
- `tokio::spawn` 的 `JoinHandle::await` 返回 `Err` 且 `is_panic() == true` → supervisor 睡 100ms 避免 busy-restart → 重建 inner task
- scheduler 内部状态（hot/cold/enqueued/tombstones/diag_gen）在 supervisor 层是共享的 `Arc`，重启 inner task 不丢数据
- panic 时正在处理的 URI 会"丢失"（其诊断计算中断），但下次编辑这个文件会重新 schedule——可接受

**Shutdown 行为**：LSP shutdown 时 `Backend::shutdown` 不显式停 consumer；整个 tokio runtime 退出时 consumer task 自然终止。

### 5.5 `Backend::did_open` fast path 修正

T1-1 已落地的 fast path 条件加严：

```rust
// 修正后：
{
    let docs = self.documents.lock().unwrap();
    let open = self.open_uris.lock().unwrap();
    if open.contains(&uri) {
        if let Some(doc) = docs.get(&uri) {
            if doc.text == params.text_document.text {
                return;  // fast path：已打开 + text 一致
            }
        }
    }
}
// fast path miss → 走 parse_and_store
// parse_and_store 尾部 self.open_uris.lock().unwrap().insert(uri.clone());
// （首次打开未改文件也会重算一次诊断）
```

### 5.6 `Backend::did_close` 维护 `open_uris`

```rust
async fn did_close(&self, params: DidCloseTextDocumentParams) {
    let uri = params.text_document.uri;
    self.open_uris.lock().unwrap().remove(&uri);  // 新增
    // ... 现有逻辑（semantic_tokens_cache 清理、fast path、磁盘重读）
}
```

### 5.7 冷启动 seed（`initialized` 末尾）

替换现有 `publish_diagnostics_for_open_files()`：

```rust
async fn initialized(&self, _: InitializedParams) {
    self.scan_workspace_parallel().await;

    let scope = self.config.lock().unwrap().diagnostics.scope.clone();
    let all_uris: Vec<Uri> = self.documents.lock().unwrap().keys().cloned().collect();
    let open: HashSet<Uri> = self.open_uris.lock().unwrap().clone();

    let (hot, cold): (Vec<_>, Vec<_>) = all_uris.into_iter()
        .partition(|u| open.contains(u));

    self.scheduler.seed_bulk(hot, Priority::Hot);
    match scope {
        DiagnosticScope::Full    => self.scheduler.seed_bulk(cold, Priority::Cold),
        DiagnosticScope::OpenOnly => { /* 不 seed */ }
    }
}
```

### 5.8 级联调度改造

`parse_and_store_with_old_tree` 里的级联 for 循环：

```rust
// 现有：
for dep_uri in dependant_uris {
    self.schedule_semantic_diagnostics(dep_uri, None);
}

// 改为：
let scope = self.config.lock().unwrap().diagnostics.scope.clone();
let open = self.open_uris.lock().unwrap();
for dep_uri in dependant_uris {
    let is_open = open.contains(&dep_uri);
    match (scope, is_open) {
        (DiagnosticScope::OpenOnly, false) => continue,   // openOnly 跳过未打开
        _ => {
            let pri = if is_open { Priority::Hot } else { Priority::Cold };
            self.scheduler.schedule(dep_uri, pri);
        }
    }
}
```

---

## 6. 错误处理

| 场景 | 行为 |
|------|------|
| compute 期间文件 DELETED | `documents[uri]` 已 remove → consumer snapshot 或一致性检查失败 → 跳过 publish，继续 loop |
| compute panic（解析器 / summary_builder bug） | `tokio::spawn` 的 consumer task 带 catch_unwind 包裹；panic 记日志 + spawn 新 consumer 替代（避免整个 LSP 瘫痪） |
| IndexState::Initializing 时 consumer 启动 | **在 pop 之前检查 state**，Not Ready 则 sleep 500ms 轮询而不 pop（避免把 Hot URI 降级为 Cold，详见 §5.4） |
| 冷启动过程中用户 did_open | 正常走 `schedule(Hot)` 路径，与冷启动 seed 互不干扰（入队去重阻止重复） |
| 消费者 pop cold 时发现 tombstone | 跳过，继续 pop 下一个 |
| `diagnostics.scope` 配置动态变化（通过 `didChangeConfiguration`） | 仅影响后续 seed / 级联决策；队列里已有的 URI 不清空（它们仍会被消费发布，即使 scope 刚切到 openOnly） |

---

## 7. 集成点改造清单

| 现有 | 改造后 |
|------|--------|
| `Backend::diag_gen` 字段 | 删除（语义移入 scheduler） |
| `Backend::schedule_semantic_diagnostics(uri, version)` | 删除；调用点改为 `self.scheduler.schedule(uri, priority)` |
| `Backend::publish_diagnostics_for_open_files()` | 删除；由 `initialized` 末尾的 `seed_bulk` 替代 |
| `Backend::did_open` fast path 条件 | 加 `open_uris.contains(uri) &&` 前缀（5.5） |
| `Backend::did_close` | 开头加 `open_uris.remove(&uri)`（5.6） |
| `Backend::did_change_watched_files` DELETED 分支 | 新增 `scheduler.invalidate(uri)` 清理 enqueued / diag_gen / tombstones |
| `parse_and_store_with_old_tree` 级联 for 循环 | 按 5.8 改造 |
| `parse_and_store_with_old_tree` 尾部 | 调 `self.scheduler.schedule(uri, Priority::Hot)` 替代 `schedule_semantic_diagnostics` |
| `LspConfig` 新增字段 `diagnostics.scope` | 默认 `"full"`；`config.rs::DiagnosticsConfig` 加枚举 `DiagnosticScope { Full, OpenOnly }` |

---

## 8. 测试计划

新增 `lsp/crates/mylua-lsp/tests/test_diagnostic_scheduler.rs`：

| # | 测试名 | 覆盖内容 |
|---|--------|----------|
| 1 | `schedule_respects_priority` | push Cold×3 + Hot×1 → pop 顺序 = [Hot, Cold, Cold, Cold] |
| 2 | `schedule_dedups_same_uri_same_priority` | 同 URI、同优先级 push 两次 → pop 一次 |
| 3 | `push_cold_then_hot_upgrades_via_tombstone` | Cold push A、B、C → 再 Hot push B → pop 顺序 [B, A, (B tombstone skipped), C] |
| 4 | `schedule_debounces_300ms_with_gen` | 快速 `schedule(uri, Hot)` 三次 → 只真正入队一次（gen 过滤生效） |
| 5 | `seed_bulk_bypasses_debounce` | `seed_bulk` 后立刻 `pop` 能拿到 URI，无需等 300ms |
| 6 | `consumer_drops_stale_after_text_change` | pop 后将 `documents[uri].text` 改掉 → 一致性检查失败 → 不调 publish |
| 7 | `consumer_handles_missing_document` | pop 到一个已从 documents 移除的 URI → 跳过不 panic |
| 8 | `open_only_scope_skips_closed_cascade` | `scope=openOnly` 时，级联到未打开文件不入队（检查队列状态） |
| 9 | `full_scope_cascades_to_closed_as_cold` | `scope=full` 时，级联到未打开文件进 Cold 队列 |
| 10 | `invalidate_cleans_all_scheduler_state` | 对某 URI 调 `invalidate` 后，enqueued / diag_gen / tombstones 均不含该 URI |

**回归护栏**：现有 `test_diagnostics.rs`（36 个）、`test_workspace.rs`（7 个）等不改。这些测试直接调用 `diagnostics::collect_semantic_diagnostics_with_version`，绕过 scheduler，不受影响。

**端到端手工验证**（Skill `test-extension`）：

1. 打开 `tests/lua-root/` 工作区 → 观察 `.vscode/mylua-lsp.log` 里的 scheduler 日志（`[sched] push/pop/publish`）
2. 打开 `diagnostics.lua` → 确认诊断在 ~300-400ms 内出现
3. 切 preview tab 来回切同一文件 → 无重复诊断（T1-1 fast path 命中）
4. 切到 `openOnly` 配置 → 重启 LSP → 未打开的 `scopes.lua` 不再自动出诊断；打开它时才出

---

## 9. 文档同步（提交时一起更新）

- `ai-readme.md`「已实现 LSP 能力」章节：
  - 修改"诊断调度"条目描述（生产者侧 debounce + 单消费者 + 优先级队列）
  - 新增"`open_uris` 集合"条目说明
- `docs/performance-analysis.md`：
  - 瓶颈 1 标 ✅ 已改造为 "单消费者调度（带宽可控）"
  - 瓶颈 4 标 ✅ 已解决
  - T1-2 / T1-4 路线图状态改为 ✅ 完成，合并为单一条目
  - T1-1 条目描述更新（加 open_uris 判定）
- `docs/architecture.md`：新增 "诊断调度架构" 子章节，简要说明 scheduler 模块地位

---

## 10. 实现阶段（建议拆分为若干独立提交）

1. **P1** — `DiagnosticScheduler` 模块骨架 + 单元测试（测试 1-5、10）
   - 新文件 `src/diagnostic_scheduler.rs`
   - 数据结构、push/pop 逻辑、seed_bulk、invalidate
   - 单元测试 `#[cfg(test)] mod tests` 覆盖队列行为
2. **P2** — Backend 集成 + 配置项
   - `LspConfig::diagnostics.scope` 新增 + 默认值
   - `Backend::open_uris` + `did_open`/`did_close` 维护
   - `Backend::scheduler` 字段 + consumer task 启动
   - 替换三条调度路径（did_open/did_change、级联、冷启动 seed）
   - 删除 `schedule_semantic_diagnostics` / `publish_diagnostics_for_open_files` / `diag_gen`
   - 集成测试 6-9
3. **P3** — 文档同步 + 手工端到端验证
   - 同步 ai-readme / performance-analysis / architecture
   - 跑 `.cursor/scripts/test-extension.sh` 手工验证体感

每阶段独立跑 `cargo build + cargo test --tests + code-reviewer`；APPROVED 后提交。

---

## 11. 风险与未决

### 11.1 已知风险

- **consumer 长时间 block**：单消费者在 compute 某大文件时，hot 队列里其他新打开的文件要等。**缓解**：compute 通常 < 500ms；若出现极大文件（>10 万行）可在 `publish_diagnostics_for_open_files` 替换后观察实际延迟再决定是否加 "对超大文件单独 spawn" 的绕行
- **debounce task 堆积**：极端连敲下每次 did_change spawn 一个 debounce task。**缓解**：每个 task ~几 KB，gen 过滤使大部分提前 return，堆积可控。若实际出现问题再用 per-URI `JoinHandle::abort()` 替代
- **配置动态切换的一致性**：运行时从 `"full"` 切到 `"openOnly"`，队列里已有的 cold 项继续被消费。**决策**：不做队列清理；语义是"切换后对新行为生效，不回滚已调度的"。需在 `ai-readme.md` 里说明这一点

### 11.2 未决问题

**无**。所有关键决策已闭环：
- D1（scope 默认）✅
- D2（syntax 即时）✅
- D3（队列结构 + tombstone）✅
- D4（生产侧 debounce）✅
- D5（text 一致性检查）✅
- D6（openOnly 级联）✅
- D7（consumer panic 重启）✅

---

## 12. 回滚策略

若实现后发现严重问题（如 consumer 消费过慢、tombstone 累积不释放、级联语义回归）：

- 保留 `schedule_semantic_diagnostics` 和 `publish_diagnostics_for_open_files` 的备份（实际做法：先不删，在 P2 阶段加 feature flag `mylua.diagnostics.useScheduler` 默认 true，关闭时走旧路径；待稳定后 P3 或下一个小版本移除 flag 和旧代码）

如果用户倾向更激进——直接删除旧路径不保留回退——也可以，P2 就删、靠 git 回滚兜底。这点在 P2 提交前再确认一次。

---

## 13. 度量与验收

**验收指标**（改造完成后）：

- `cargo test --tests` 全绿（≥ 398 + 新增 10 个 = 408 个测试）
- 手工端到端验证 4 条场景（§8 末尾）通过
- code-reviewer APPROVED
- 冷启动到首个打开文件诊断出现的时间（在 `tests/lua-root/` 工作区）相比当前快（定性，无硬指标）

**不做硬性能回归测试**（5 万文件级别 fixture 暂未准备，留作 T2 阶段性能工程的一部分）。
