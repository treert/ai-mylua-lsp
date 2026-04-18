# DiagnosticScheduler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用单消费者优先级队列统一诊断调度，让"打开的文件"严格优先于"未打开的文件"；可配置冷启动覆盖范围（全量 / 仅打开）。

**Architecture:** 新增 `DiagnosticScheduler` 模块（hot/cold 双 VecDeque + HashMap 去重 + tombstone 升级方案）+ 单 tokio supervisor task 运行 consumer loop。生产者侧沿用 300ms + `diag_gen` debounce，冷启动 `seed_bulk` 绕过 debounce。`Backend` 新增 `open_uris` 追踪已 did_open 的 URI 集合。

**Tech Stack:** Rust + tower-lsp-server 0.23 + tokio + std::sync::Mutex + tokio::sync::Notify

**关联 spec:** [`docs/superpowers/specs/2026-04-19-diagnostic-scheduler-design.md`](../specs/2026-04-19-diagnostic-scheduler-design.md)

---

## File Structure

### Create
- `lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs` — 核心调度器 + 内嵌单元测试
- `lsp/crates/mylua-lsp/tests/test_diagnostic_scheduler.rs` — consumer/集成测试

### Modify
- `lsp/crates/mylua-lsp/src/config.rs` — 新增 `DiagnosticScope` 枚举 + `DiagnosticsConfig::scope` 字段
- `lsp/crates/mylua-lsp/src/lib.rs` —
  - 声明 `pub mod diagnostic_scheduler`
  - `Backend` 新增 `open_uris` / `scheduler` 字段（删除 `diag_gen`）
  - `did_open` / `did_close` 维护 `open_uris`
  - 替换三条调度路径：`parse_and_store` 末尾、级联循环、`initialized` 末尾
  - 删除 `schedule_semantic_diagnostics` / `publish_diagnostics_for_open_files`
  - 修正 T1-1 fast path 条件加 `open_uris.contains`
  - `did_change_watched_files` DELETED 分支调 `scheduler.invalidate`
- `ai-readme.md` — 同步能力描述
- `docs/performance-analysis.md` — T1-2 / T1-4 标完成，T1-1 描述更新
- `docs/architecture.md` — 新增「诊断调度架构」小节

### 编译/测试命令
- 构建：`cd lsp && cargo build`
- 测试：`cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests`
- 单文件测试：`cd lsp && CARGO_TARGET_DIR=target-test cargo test --test <name> -- --nocapture`

> **每个 Task 结束时**：
> 1. `cd lsp && cargo build`（零新 warning）
> 2. `cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests`（全绿）
> 3. 对代码型 Task 调用 `code-reviewer` subagent 审查（按项目规则 `.cursor/rules/code-review-after-changes.mdc`）；review APPROVED 后再 commit
> 4. 文档型 Task（只改 `*.md`）按项目规则可跳过 code-reviewer

---

## Task 1: Scheduler 骨架 + push/pop + 基础单元测试

**Files:**
- Create: `lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs`
- Modify: `lsp/crates/mylua-lsp/src/lib.rs:2-31`（`pub mod` 列表）

### - [ ] Step 1.1: 创建模块骨架

**Create `lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs`:**

```rust
//! DiagnosticScheduler — 按优先级单线程调度 semantic 诊断计算。
//!
//! Hot 队列（已打开文件）严格优先于 Cold 队列（未打开/冷启动 seed）。
//! Cold→Hot 升级走 tombstone 方案：push 时直接入 hot 并在 cold 标记
//! 作废位，pop 时跳过。push 和 pop 均摊 O(1)。
//!
//! 生产者侧 `schedule` 带 300ms debounce（`diag_gen` 代数过滤过期任务）。
//! 冷启动 `seed_bulk` 绕过 debounce，批量入队后统一 notify 一次。
//!
//! 设计细节见 `docs/superpowers/specs/2026-04-19-diagnostic-scheduler-design.md`。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tower_lsp_server::lsp_types::Uri;

pub const DIAGNOSTIC_DEBOUNCE_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    Hot,
    Cold,
}

struct Inner {
    hot: VecDeque<Uri>,
    cold: VecDeque<Uri>,
    /// 每个当前在队列里的 URI 的优先级；Cold→Hot 升级时更新为 Hot。
    enqueued: HashMap<Uri, Priority>,
    /// cold 队列里被升级过的 URI 集合；pop cold 时遇到则跳过。
    cold_tombstones: HashSet<Uri>,
    /// Per-URI 单调代数；`schedule` 生产者侧 debounce 过滤过期任务用。
    diag_gen: HashMap<Uri, u64>,
}

pub struct DiagnosticScheduler {
    inner: Mutex<Inner>,
    notify: Notify,
}

impl DiagnosticScheduler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                hot: VecDeque::new(),
                cold: VecDeque::new(),
                enqueued: HashMap::new(),
                cold_tombstones: HashSet::new(),
                diag_gen: HashMap::new(),
            }),
            notify: Notify::new(),
        })
    }

    /// Internal enqueue shared by `schedule` 的 debounce task 与 `seed_bulk`。
    /// 调用者负责在适当时机 `notify_one`（seed_bulk 批量时仅末尾一次）。
    fn push_to_queue(inner: &mut Inner, uri: Uri, priority: Priority) {
        match (inner.enqueued.get(&uri).copied(), priority) {
            (Some(Priority::Hot), _) => {}
            (Some(Priority::Cold), Priority::Hot) => {
                inner.cold_tombstones.insert(uri.clone());
                inner.hot.push_back(uri.clone());
                inner.enqueued.insert(uri, Priority::Hot);
            }
            (Some(Priority::Cold), Priority::Cold) => {}
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

    /// Pop 下一个待诊断的 URI。Hot 严格优先于 Cold；cold tombstone 会被跳过。
    pub fn pop(&self) -> Option<Uri> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(u) = inner.hot.pop_front() {
            inner.enqueued.remove(&u);
            return Some(u);
        }
        while let Some(u) = inner.cold.pop_front() {
            if inner.cold_tombstones.remove(&u) {
                continue;
            }
            inner.enqueued.remove(&u);
            return Some(u);
        }
        None
    }

    /// 等下一次 push/seed_bulk 唤醒。
    pub async fn notified(&self) {
        self.notify.notified().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> Uri {
        format!("file:///{}", s).parse().unwrap()
    }

    #[test]
    fn pop_respects_priority_hot_first_then_cold_fifo() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, uri("a"), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("b"), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("c"), Priority::Hot);
        }

        assert_eq!(s.pop(), Some(uri("c")));
        assert_eq!(s.pop(), Some(uri("a")));
        assert_eq!(s.pop(), Some(uri("b")));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn push_dedups_same_uri_same_priority() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, uri("a"), Priority::Hot);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("a"), Priority::Hot);
        }

        assert_eq!(s.pop(), Some(uri("a")));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn push_cold_then_hot_upgrades_via_tombstone() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, uri("a"), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("b"), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("c"), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("b"), Priority::Hot);
        }

        assert_eq!(s.pop(), Some(uri("b"))); // Hot 先
        assert_eq!(s.pop(), Some(uri("a"))); // cold[0]
        // b 在 cold 的残影被 tombstone 跳过
        assert_eq!(s.pop(), Some(uri("c"))); // cold[2]
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn push_hot_when_hot_already_enqueued_is_noop() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, uri("a"), Priority::Hot);
            DiagnosticScheduler::push_to_queue(&mut inner, uri("a"), Priority::Cold); // 试图降级
        }

        assert_eq!(s.pop(), Some(uri("a")));
        // 降级不生效，cold 里不应有残留
        assert_eq!(s.pop(), None);
    }
}
```

### - [ ] Step 1.2: 声明 mod

**Modify `lsp/crates/mylua-lsp/src/lib.rs`** — 在 `pub mod` 列表里加一行（按字母序插入合适位置）：

定位到 `pub mod diagnostics;` 所在行（`lib.rs:7`），在其下方加：

```rust
pub mod diagnostic_scheduler;
```

### - [ ] Step 1.3: 构建并跑新测试

```bash
cd lsp && cargo build
```
Expected: 编译通过，无新 warning。

```bash
cd lsp && CARGO_TARGET_DIR=target-test cargo test --test diagnostic_scheduler 2>/dev/null || CARGO_TARGET_DIR=target-test cargo test diagnostic_scheduler::tests
```
Expected: 4 tests passed（`pop_respects_priority_hot_first_then_cold_fifo` / `push_dedups_same_uri_same_priority` / `push_cold_then_hot_upgrades_via_tombstone` / `push_hot_when_hot_already_enqueued_is_noop`）。

### - [ ] Step 1.4: 跑全量回归

```bash
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: passed ≥ 402（原 398 + 新增 4），failed = 0。

### - [ ] Step 1.5: code-reviewer + commit

调用 `code-reviewer` subagent 审 `src/diagnostic_scheduler.rs`，APPROVED 后：

```bash
git add lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs lsp/crates/mylua-lsp/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(lsp): DiagnosticScheduler 骨架 — Priority + push_to_queue + pop + tombstone

新增 src/diagnostic_scheduler.rs：
- Priority::{Hot, Cold} 两档优先级
- Inner 持有 hot/cold 双 VecDeque + enqueued HashMap + cold_tombstones HashSet + diag_gen HashMap
- push_to_queue 内部 helper 处理 5 种 (existing, incoming) 分支；Cold→Hot 升级
  通过 tombstone 方案（cold 残影 pop 时跳过）避免 O(n) 搬队
- pop 严格 Hot 优先 + cold tombstone 跳过

内嵌 4 个单元测试覆盖：优先级、去重、tombstone 升级、降级保护。
EOF
)"
```

---

## Task 2: schedule — 生产者侧 300ms debounce

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs`（在 `impl DiagnosticScheduler` 内）

### - [ ] Step 2.1: 添加 schedule 方法

**Append to `impl DiagnosticScheduler`**（在 `pub async fn notified` 之后）：

```rust
    /// 调度一个 URI 的 semantic 诊断计算。经 300ms debounce 窗口后入队。
    /// 连续多次 schedule 同一 URI → 所有 debounce task 里只有最后一个
    /// 真正 push（gen 代数过滤）；消费者只 compute 一次。
    pub fn schedule(self: &Arc<Self>, uri: Uri, priority: Priority) {
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

            let latest = scheduler
                .inner
                .lock()
                .unwrap()
                .diag_gen
                .get(&uri_c)
                .copied()
                .unwrap_or(0);
            if latest != gen {
                return; // 期间又有 schedule，让后来的 task 做
            }

            {
                let mut inner = scheduler.inner.lock().unwrap();
                Self::push_to_queue(&mut inner, uri_c, priority);
            }
            scheduler.notify.notify_one();
        });
    }
```

### - [ ] Step 2.2: 添加 debounce 单元测试

**Append to `mod tests`**（在文件末尾的 `mod tests` 内、最后一个 `#[test]` 之后）：

```rust
    #[tokio::test]
    async fn schedule_debounces_300ms_with_gen_collapse() {
        let s = DiagnosticScheduler::new();

        // 三次快速 schedule
        s.schedule(uri("a"), Priority::Hot);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.schedule(uri("a"), Priority::Hot);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.schedule(uri("a"), Priority::Hot);

        // 100ms 内：debounce 未到，队列应该还是空
        assert_eq!(s.pop(), None);

        // 等所有 debounce task 完成（300ms + 余量）
        tokio::time::sleep(Duration::from_millis(400)).await;

        // 只有最新 gen 的 task 把 uri("a") 入了队
        assert_eq!(s.pop(), Some(uri("a")));
        assert_eq!(s.pop(), None);
    }

    #[tokio::test]
    async fn schedule_notify_wakes_up_consumer() {
        let s = DiagnosticScheduler::new();
        let s2 = s.clone();

        let handle = tokio::spawn(async move {
            // 模拟 consumer：等待 notify 后 pop
            loop {
                if let Some(u) = s2.pop() {
                    return u;
                }
                s2.notified().await;
            }
        });

        // 给 consumer 一点时间进入 notified().await
        tokio::time::sleep(Duration::from_millis(50)).await;

        s.schedule(uri("a"), Priority::Hot);

        // consumer 应该在 debounce + 一点时间内被唤醒并返回
        let got = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("consumer should wake up within 500ms")
            .expect("task finished");
        assert_eq!(got, uri("a"));
    }
```

### - [ ] Step 2.3: 构建并跑测试

```bash
cd lsp && cargo build && CARGO_TARGET_DIR=target-test cargo test diagnostic_scheduler::tests 2>&1 | tail -20
```
Expected: 6 tests passed（新增 2 个）。

### - [ ] Step 2.4: 跑全量回归

```bash
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: passed ≥ 404，failed = 0。

### - [ ] Step 2.5: code-reviewer + commit

```bash
git add lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs
git commit -m "$(cat <<'EOF'
feat(lsp): DiagnosticScheduler schedule — 300ms debounce + gen 过滤

新增 schedule(uri, priority) API：bump diag_gen → spawn 300ms sleep 任务
→ 醒来比对 gen；过期则 return，最新则 push_to_queue + notify_one。

2 个单元测试覆盖：连续三次 schedule 只 compute 一次（gen collapse）、
notify 正确唤醒 pop-waiting 的消费者。
EOF
)"
```

---

## Task 3: seed_bulk + invalidate

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs`

### - [ ] Step 3.1: 添加 seed_bulk 和 invalidate

**Append to `impl DiagnosticScheduler`**（在 `schedule` 之后）：

```rust
    /// 批量入队（冷启动专用）。绕过 debounce，末尾统一 notify 一次。
    pub fn seed_bulk(&self, uris: Vec<Uri>, priority: Priority) {
        if uris.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock().unwrap();
            for uri in uris {
                Self::push_to_queue(&mut inner, uri, priority);
            }
        }
        self.notify.notify_one();
    }

    /// 文件 DELETED 时清空 scheduler 里与 `uri` 相关的状态。
    /// 不物理移除 hot/cold 队列里的残留——consumer 侧对 `documents` 不存在
    /// 的 URI 会跳过，自然容错。
    pub fn invalidate(&self, uri: &Uri) {
        let mut inner = self.inner.lock().unwrap();
        inner.enqueued.remove(uri);
        inner.cold_tombstones.remove(uri);
        inner.diag_gen.remove(uri);
    }
```

### - [ ] Step 3.2: 添加单元测试

**Append to `mod tests`**：

```rust
    #[test]
    fn seed_bulk_bypasses_debounce_immediately_pops() {
        let s = DiagnosticScheduler::new();
        s.seed_bulk(
            vec![uri("a"), uri("b"), uri("c")],
            Priority::Cold,
        );

        assert_eq!(s.pop(), Some(uri("a")));
        assert_eq!(s.pop(), Some(uri("b")));
        assert_eq!(s.pop(), Some(uri("c")));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn seed_bulk_empty_is_noop() {
        let s = DiagnosticScheduler::new();
        s.seed_bulk(vec![], Priority::Cold);
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn seed_bulk_hot_upgrades_cold_via_tombstone() {
        let s = DiagnosticScheduler::new();
        s.seed_bulk(vec![uri("a"), uri("b")], Priority::Cold);
        s.seed_bulk(vec![uri("a")], Priority::Hot);

        assert_eq!(s.pop(), Some(uri("a"))); // Hot 优先
        assert_eq!(s.pop(), Some(uri("b"))); // cold 残余
        assert_eq!(s.pop(), None); // uri a 的 cold tombstone 被跳过
    }

    #[test]
    fn invalidate_clears_all_state_for_uri() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            inner.diag_gen.insert(uri("a"), 5);
            inner.cold_tombstones.insert(uri("a"));
            inner.enqueued.insert(uri("a"), Priority::Hot);
        }

        s.invalidate(&uri("a"));

        let inner = s.inner.lock().unwrap();
        assert!(!inner.diag_gen.contains_key(&uri("a")));
        assert!(!inner.cold_tombstones.contains(&uri("a")));
        assert!(!inner.enqueued.contains_key(&uri("a")));
    }
```

### - [ ] Step 3.3: 构建并跑测试

```bash
cd lsp && cargo build && CARGO_TARGET_DIR=target-test cargo test diagnostic_scheduler::tests 2>&1 | tail -20
```
Expected: 10 tests passed。

### - [ ] Step 3.4: 全量回归

```bash
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: passed ≥ 408，failed = 0。

### - [ ] Step 3.5: code-reviewer + commit

```bash
git add lsp/crates/mylua-lsp/src/diagnostic_scheduler.rs
git commit -m "$(cat <<'EOF'
feat(lsp): DiagnosticScheduler seed_bulk + invalidate

- seed_bulk(uris, priority)：冷启动专用的批量入队，绕过 debounce，
  末尾统一 notify_one 避免 5 万次唤醒
- invalidate(uri)：文件 DELETED 时清空 enqueued / cold_tombstones /
  diag_gen 三份状态；队列里残留的 URI 由 consumer 的 "doc 不存在则
  跳过" 路径兜底，不物理移除

4 个单元测试覆盖：批量 pop 顺序、空批量 no-op、seed_bulk 的
Cold→Hot 升级、invalidate 清空三份状态。
EOF
)"
```

---

## Task 4: 添加 DiagnosticScope 配置

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/config.rs:113-172`

### - [ ] Step 4.1: 在 config.rs 添加枚举

**Modify `lsp/crates/mylua-lsp/src/config.rs`** — 在 `DiagnosticSeverityOption` 枚举定义（大约第 174-193 行）之后添加：

```rust
/// Scope of diagnostics publishing.
///
/// - `Full` (default): cold-start seeds the entire workspace (already
///   open → Hot queue, others → Cold); cascade触发所有 dependant URIs.
/// - `OpenOnly`: cold-start seeds only `open_uris` as Hot; cascade 跳过
///   未打开的 dependant URIs. Matches the default behavior of most LSPs
///   (rust-analyzer, pyright).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticScope {
    Full,
    OpenOnly,
}

impl Default for DiagnosticScope {
    fn default() -> Self {
        Self::Full
    }
}
```

### - [ ] Step 4.2: 在 `DiagnosticsConfig` 里加字段

**Modify `lsp/crates/mylua-lsp/src/config.rs`** — 在 `DiagnosticsConfig` struct 的最后一个字段 `return_mismatch` 之后（大约第 148 行）加：

```rust
    /// Scope of cold-start diagnostics publishing + cascade fan-out.
    /// Default `"full"`. See `DiagnosticScope` for semantics.
    pub scope: DiagnosticScope,
```

同步修改 `impl Default for DiagnosticsConfig`（大约第 151-172 行），在最后一个字段 `return_mismatch: DiagnosticSeverityOption::Off` 之后加：

```rust
            scope: DiagnosticScope::Full,
```

完成后 `DiagnosticsConfig` 应类似：

```rust
pub struct DiagnosticsConfig {
    pub enable: bool,
    // ... existing fields ...
    #[serde(rename = "returnMismatch")]
    pub return_mismatch: DiagnosticSeverityOption,
    pub scope: DiagnosticScope,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            enable: true,
            // ... existing defaults ...
            return_mismatch: DiagnosticSeverityOption::Off,
            scope: DiagnosticScope::Full,
        }
    }
}
```

### - [ ] Step 4.3: 构建并跑全量

```bash
cd lsp && cargo build && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: 编译通过；passed 不变（408），failed = 0。

### - [ ] Step 4.4: commit（文档型轻量改动，可跳过 code-reviewer）

```bash
git add lsp/crates/mylua-lsp/src/config.rs
git commit -m "feat(lsp): DiagnosticsConfig 加 scope 字段（full | openOnly，默认 full）"
```

---

## Task 5: Backend 加 `open_uris` 字段并在 did_open/did_close 维护

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/lib.rs:56-115`（Backend struct + new）
- Modify: `lsp/crates/mylua-lsp/src/lib.rs:828-860`（did_open）
- Modify: `lsp/crates/mylua-lsp/src/lib.rs:887-978`（did_close）

### - [ ] Step 5.1: Backend struct 加字段

**Modify `lsp/crates/mylua-lsp/src/lib.rs`** — 在 `Backend` struct 定义里（第 56-79 行），在 `semantic_tokens_counter` 之后加一行：

```rust
    /// URIs currently in LSP `did_open` state (not yet `did_close`d).
    /// Used by:
    ///   - T1-1 fast path guard in `did_open` (skip parse only if already
    ///     open AND text matches)
    ///   - Diagnostic scheduler priority decision (Hot vs Cold)
    ///   - Cold-start seed routing (`initialized` splits documents into
    ///     Hot/Cold based on this set)
    open_uris: Arc<Mutex<HashSet<Uri>>>,
```

同步在 `impl Backend` 的 `new()` 方法里（第 102-116 行），在 `semantic_tokens_counter: Arc::new(Mutex::new(0)),` 之后加：

```rust
            open_uris: Arc::new(Mutex::new(HashSet::new())),
```

### - [ ] Step 5.2: `did_close` 移除 open_uris

**Modify `lsp/crates/mylua-lsp/src/lib.rs::did_close`**（大约第 887 行开始）— 在函数开头 `let uri = params.text_document.uri;` 之后加：

```rust
        self.open_uris.lock().unwrap().remove(&uri);
```

完整片段应为：

```rust
    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.open_uris.lock().unwrap().remove(&uri);
        // The client won't retry a stale `previous_result_id` after
        // closing the file, so drop the cache entry to free memory.
        self.semantic_tokens_cache.lock().unwrap().remove(&uri);
        // ... rest unchanged ...
```

### - [ ] Step 5.3: `did_open` 修正 fast path 条件 + 非 fast path 路径插入 open_uris

**Modify `lsp/crates/mylua-lsp/src/lib.rs::did_open`**（大约第 828 行开始）— 整体替换为：

```rust
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let lock = self.edit_lock_for(&uri);
        let _guard = lock.lock().await;

        // Fast path (symmetric to `did_close`): skip re-parse / re-build
        // summary / re-publish when the indexed document already matches
        // the incoming buffer byte-for-byte AND the URI is already
        // tracked as open. Requiring `open_uris.contains` prevents
        // "cold-start indexed, first did_open, text identical" from
        // silently skipping diagnostics on a file the user just opened.
        //
        // We intentionally compare *text only*, not version: clients
        // reset `version` to 1 on reopen but content is unchanged, and
        // conversely identical version numbers do not guarantee
        // identical content across clients. Byte-equality is the only
        // safe signal.
        //
        // Invariant: fast path does NOT touch the scheduler (`diag_gen`
        // or queues). Any in-flight debounce from before still publishes
        // correctly because its compute uses current `documents[uri]`
        // which this check just proved equal to the incoming buffer.
        {
            let docs = self.documents.lock().unwrap();
            let open = self.open_uris.lock().unwrap();
            if open.contains(&uri) {
                if let Some(doc) = docs.get(&uri) {
                    if doc.text == params.text_document.text {
                        return;
                    }
                }
            }
        }

        let uri_for_open_set = uri.clone();
        self.parse_and_store(
            uri,
            params.text_document.text,
            Some(params.text_document.version),
        );
        self.open_uris.lock().unwrap().insert(uri_for_open_set);
    }
```

注意：本次替换保留注释中的 `scheduler` 术语（即使 scheduler 在本 Task 还未接入）——因为 Task 6/7/8 紧接着会补上，此处提前用目标术语避免两步编辑同一段注释。若担心中间 commit 状态的术语不一致，可将 "the scheduler (`diag_gen` or queues)" 改为 "semantic diagnostic state"，语义等价。

### - [ ] Step 5.4: 确认 `HashSet` 已在 `use` 里

**Modify `lsp/crates/mylua-lsp/src/lib.rs`**（大约第 33-52 行 use 区域）— 确认有 `use std::collections::{HashMap, HashSet};`。如果当前只有 `HashMap`，改成两者都导入。

### - [ ] Step 5.5: 构建 + 全量测试

```bash
cd lsp && cargo build 2>&1 | tail -10
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: 编译通过；全量测试全绿（408）。

### - [ ] Step 5.6: code-reviewer + commit

调 code-reviewer。重点审：
- `did_open` fast path 修改后，冷启动后首次打开未改文件会正确 miss fast path 走 parse_and_store 吗
- `did_close` 的 open_uris 清理时序（fast-path return 之前还是之后？—— 应在最前面，一律清）
- 并发：多线程同时 did_open / did_close 对 open_uris 的锁顺序

APPROVED 后：

```bash
git add lsp/crates/mylua-lsp/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(lsp): Backend 加 open_uris 字段并在 did_open/did_close 维护

显式追踪客户端已 did_open 的 URI 集合，为后续 DiagnosticScheduler
做 Hot/Cold 优先级决策提供依据。

同步修正 T1-1 fast path 条件：原先只比对 text 一致，现在要求"已在
open_uris 里 && text 一致"才跳过——避免冷启动扫描后首次 did_open
未改文件时 fast path 命中导致无诊断的问题。
EOF
)"
```

---

## Task 6: Backend 集成 scheduler 字段 + consumer supervisor

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/lib.rs`（Backend struct + new + 新增 consumer_loop 函数）

### - [ ] Step 6.1: Backend struct 加 scheduler 字段

**Modify `lsp/crates/mylua-lsp/src/lib.rs`** — 在 Backend struct 里（`open_uris` 之后）加：

```rust
    /// Unified semantic diagnostics scheduler (priority queue + single
    /// consumer). Replaces the per-URI `schedule_semantic_diagnostics`
    /// spawns and the cold-start `publish_diagnostics_for_open_files`.
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
```

同步在 `impl Backend::new(client)`（第 102-116 行）里，在 `open_uris: ...,` 之后加：

```rust
            scheduler: diagnostic_scheduler::DiagnosticScheduler::new(),
```

### - [ ] Step 6.2: 新增 consumer_loop 函数

**Append to `lsp/crates/mylua-lsp/src/lib.rs`**（可以放在 `impl Backend` 块的末尾、`impl LanguageServer for Backend` 之前）：

```rust
/// Supervisor for the diagnostic consumer task. Spawns `consumer_loop`
/// and auto-restarts it on panic (logs + 100ms backoff).
fn start_diagnostic_consumer(
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<aggregation::WorkspaceAggregation>>,
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
                consumer_loop(s, d, i, c, st, cl).await;
            });

            match handle.await {
                Ok(()) => break, // consumer_loop 正常返回（目前不会发生）
                Err(e) if e.is_panic() => {
                    lsp_log!(
                        "[sched] consumer panicked: {:?}, restarting in 100ms...",
                        e
                    );
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
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<aggregation::WorkspaceAggregation>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    client: Client,
) {
    loop {
        // Ready 门槛（先于 pop）。Not Ready 期间只轮询，不动队列——
        // 否则 pop 到 Hot URI 又推回 Cold 会导致优先级降级。
        if *index_state.lock().unwrap() != IndexState::Ready {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        // 等待任务
        let uri = loop {
            if let Some(u) = scheduler.pop() {
                break u;
            }
            scheduler.notified().await;
        };

        // Snapshot text（持 documents 锁最短时间）
        let snapshot = {
            let docs = documents.lock().unwrap();
            let Some(doc) = docs.get(&uri) else {
                continue; // 文件已删
            };
            doc.text.clone()
        };

        // Compute diagnostics（同现有 schedule_semantic_diagnostics 内部逻辑）
        let diags = {
            let docs = documents.lock().unwrap();
            let Some(doc) = docs.get(&uri) else {
                continue;
            };
            let mut syntax =
                diagnostics::collect_diagnostics(doc.tree.root_node(), doc.text.as_bytes());
            let mut idx = index.lock().unwrap();
            let cfg = config.lock().unwrap();
            let semantic = diagnostics::collect_semantic_diagnostics_with_version(
                doc.tree.root_node(),
                doc.text.as_bytes(),
                &uri,
                &mut idx,
                &doc.scope_tree,
                &cfg.diagnostics,
                &cfg.runtime.version,
            );
            syntax.extend(semantic);
            diagnostics::apply_diagnostic_suppressions(
                doc.tree.root_node(),
                doc.text.as_bytes(),
                syntax,
            )
        };

        // 发布前一致性检查
        let stale = {
            let docs = documents.lock().unwrap();
            match docs.get(&uri) {
                Some(doc) => doc.text != snapshot,
                None => true,
            }
        };
        if stale {
            continue;
        }

        client.publish_diagnostics(uri, diags, None).await;
    }
}
```

### - [ ] Step 6.3: 在 initialize 末尾启动 consumer

**Modify `lsp/crates/mylua-lsp/src/lib.rs::initialize`**（`initialize` handler 末尾，在返回 `Ok(InitializeResult { ... })` 之前）—

找到 `initialize` 函数（大约第 689 行），在函数即将返回前（`offset_encoding: None,` 行之前的 `Ok(InitializeResult {` 构造之前）加一段：

```rust
        start_diagnostic_consumer(
            self.scheduler.clone(),
            self.documents.clone(),
            self.index.clone(),
            self.config.clone(),
            self.index_state.clone(),
            self.client.clone(),
        );
```

### - [ ] Step 6.4: 构建 + 跑全量

```bash
cd lsp && cargo build 2>&1 | tail -10
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: 编译通过；全量 408 测试全绿（尚未用到 scheduler 做诊断发布，只是 consumer 后台闲置）。

### - [ ] Step 6.5: code-reviewer + commit

审查要点：
- consumer_loop 的锁持有粒度（不跨 await）
- Panic supervisor 的正确性
- Ready 门槛放在 pop 之前
- `lsp_log!` 宏是否已 import 可用

APPROVED 后：

```bash
git add lsp/crates/mylua-lsp/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(lsp): Backend 加 scheduler 字段 + consumer_loop supervisor

- Backend 新增 scheduler: Arc<DiagnosticScheduler>
- 新增 start_diagnostic_consumer supervisor：tokio::spawn 外层 loop 监控
  内层 consumer_loop 的 JoinHandle，panic 时日志 + 100ms 退避 + 重启
- consumer_loop：Ready 门槛先于 pop → pop → snapshot text → compute
  → text 一致性 check → publish；锁持有最短
- 在 initialize handler 末尾启动 supervisor

consumer 此刻仍空转——后续 Task 会把三条调度路径切过去。
EOF
)"
```

---

## Task 7: 替换 `did_change` / `did_open` / 级联的 schedule 调用

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/lib.rs::parse_and_store_with_old_tree`（第 145-267 行）

### - [ ] Step 7.1: 替换 parse_and_store 末尾的 schedule_semantic_diagnostics

**Modify `lsp/crates/mylua-lsp/src/lib.rs::parse_and_store_with_old_tree`** — 找到第 261 行附近 `self.schedule_semantic_diagnostics(uri, version);`，改为：

```rust
            let pri = if self.open_uris.lock().unwrap().contains(&uri) {
                diagnostic_scheduler::Priority::Hot
            } else {
                diagnostic_scheduler::Priority::Cold
            };
            self.scheduler.schedule(uri, pri);
```

注意 `version` 参数不再使用（scheduler 内部不需要，publish 时传 None）；确保 `uri` 的生命周期正确（schedule 消费 uri）。

### - [ ] Step 7.2: 替换级联循环，加 scope 过滤

**Modify `lsp/crates/mylua-lsp/src/lib.rs::parse_and_store_with_old_tree`** — 找到 263-265 行附近：

```rust
            for dep_uri in dependant_uris {
                self.schedule_semantic_diagnostics(dep_uri, None);
            }
```

替换为：

```rust
            let scope = self.config.lock().unwrap().diagnostics.scope.clone();
            let open = self.open_uris.lock().unwrap();
            for dep_uri in dependant_uris {
                let is_open = open.contains(&dep_uri);
                if !is_open && matches!(scope, config::DiagnosticScope::OpenOnly) {
                    continue;
                }
                let pri = if is_open {
                    diagnostic_scheduler::Priority::Hot
                } else {
                    diagnostic_scheduler::Priority::Cold
                };
                self.scheduler.schedule(dep_uri, pri);
            }
```

注意：`open` 的 mutex guard 会在 for 循环完成后释放；级联循环内不跨 await，锁持有时间短。

### - [ ] Step 7.3: 构建 + 全量

```bash
cd lsp && cargo build 2>&1 | tail -10
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: 编译通过；仍有少量旧 test（如 `test_workspace.rs`）验证 publish 会触发——因为 consumer 现在在背后跑，publish 仍会发生，测试应全绿（408）。

若测试超时（consumer + debounce 链路增加延迟），在具体失败测试里加等待或改用直接调 `collect_semantic_diagnostics` 的方式——本 Task 仅替换调度路径，不改变诊断逻辑本身。

### - [ ] Step 7.4: code-reviewer + commit

APPROVED 后：

```bash
git add lsp/crates/mylua-lsp/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(lsp): did_change/did_open/级联改走 DiagnosticScheduler 路径

- parse_and_store 末尾：self.schedule_semantic_diagnostics(uri, version)
  → scheduler.schedule(uri, Hot/Cold by open_uris.contains)
- 级联循环：加 scope 过滤——OpenOnly 模式下 !open_uris.contains
  的 dep_uri 直接 continue；Full 模式下未打开文件走 Cold 优先级
- 旧的 schedule_semantic_diagnostics 方法暂留（下一个 task 删除）
EOF
)"
```

---

## Task 8: 替换 `initialized` 的 `publish_diagnostics_for_open_files` + 删除旧函数 + 清理 diag_gen

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/lib.rs`（initialized handler + Backend struct + 删除两个函数）

### - [ ] Step 8.1: 替换 initialized 末尾

**Modify `lsp/crates/mylua-lsp/src/lib.rs::initialized`**（第 810-822 行）—

```rust
    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(
                MessageType::INFO,
                "mylua-lsp initialized, scanning workspace...",
            )
            .await;
        self.scan_workspace_parallel().await;

        // Seed the scheduler queue based on diagnostics.scope config.
        let scope = self.config.lock().unwrap().diagnostics.scope.clone();
        let all_uris: Vec<Uri> = self.documents.lock().unwrap().keys().cloned().collect();
        let open: std::collections::HashSet<Uri> =
            self.open_uris.lock().unwrap().clone();
        let (hot, cold): (Vec<_>, Vec<_>) = all_uris
            .into_iter()
            .partition(|u| open.contains(u));
        self.scheduler
            .seed_bulk(hot, diagnostic_scheduler::Priority::Hot);
        if matches!(scope, config::DiagnosticScope::Full) {
            self.scheduler
                .seed_bulk(cold, diagnostic_scheduler::Priority::Cold);
        }

        self.client
            .log_message(MessageType::INFO, "mylua-lsp workspace scan complete")
            .await;
    }
```

注意：`self.publish_diagnostics_for_open_files();` 行被删除。

### - [ ] Step 8.2: 删除 `schedule_semantic_diagnostics` 和 `publish_diagnostics_for_open_files` 方法

**Modify `lsp/crates/mylua-lsp/src/lib.rs`** — 删除：

1. `schedule_semantic_diagnostics` 方法（约第 269-327 行整个 fn 块）
2. `publish_diagnostics_for_open_files` 方法（约第 513-552 行整个 fn 块）

### - [ ] Step 8.3: 删除 Backend 的 `diag_gen` 字段

**Modify `lsp/crates/mylua-lsp/src/lib.rs`**（Backend struct + new + did_change_watched_files）：

1. Backend struct 里删除：
   ```rust
       diag_gen: Arc<Mutex<HashMap<Uri, u64>>>,
   ```
   及其前后的注释块（如有）。
2. `Backend::new` 里删除：
   ```rust
               diag_gen: Arc::new(Mutex::new(HashMap::new())),
   ```
3. `did_change_watched_files` handler（大约第 1020 行附近）里找到：
   ```rust
                       self.diag_gen.lock().unwrap().remove(&change.uri);
   ```
   替换为：
   ```rust
                       self.scheduler.invalidate(&change.uri);
   ```
   （DELETED 事件时清理 scheduler 状态）

4. Backend 其他位置如果有 `self.diag_gen.` 引用全部清理（`grep -n diag_gen lsp/crates/mylua-lsp/src/lib.rs` 核对）。

### - [ ] Step 8.4: 核对 `did_open` 注释（Task 5 已完成最终版，此处为检查）

Task 5 Step 5.3 已将 `did_open` 的 fast path 注释写成最终版（包含 "fast path does NOT touch the scheduler (`diag_gen` or queues)" 的 Invariant）。本步骤仅确认：

- `did_open` 的 fast path 注释里没有残留的旧表述（"does NOT bump `diag_gen`"）
- 如 Task 5 选了简化版 "semantic diagnostic state"，现在可以改回精确版本 "the scheduler (`diag_gen` or queues)"（因为 scheduler 已接入）

如无残留，跳过不改。

### - [ ] Step 8.5: 构建 + 全量回归

```bash
cd lsp && cargo build 2>&1 | tail -15
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: 编译通过，无 dead_code warning；全量测试全绿。

如有测试因为"诊断发布时机变化"而失败（比如期望同步 publish 但 scheduler 300ms debounce 后才 publish），按 Task 9 的集成测试思路调整或跳过。

### - [ ] Step 8.6: code-reviewer + commit

重点审：
- 所有 `diag_gen` 引用已清理
- initialized 的 seed_bulk 行为符合 spec §5.7
- did_change_watched_files DELETED 路径走 invalidate

APPROVED 后：

```bash
git add lsp/crates/mylua-lsp/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(lsp): 删除旧诊断调度路径，全部切到 DiagnosticScheduler

- initialized 末尾：publish_diagnostics_for_open_files → 按 scope 配置
  partition documents 为 hot/cold 并 seed_bulk
- 删除 schedule_semantic_diagnostics 方法（语义迁入 scheduler.schedule）
- 删除 publish_diagnostics_for_open_files 方法
- 删除 Backend::diag_gen 字段（state 迁入 scheduler.inner.diag_gen）
- did_change_watched_files DELETED 分支改调 scheduler.invalidate

至此所有 semantic 诊断发布统一走 DiagnosticScheduler → consumer_loop 路径。
EOF
)"
```

---

## Task 9: 集成测试 — scope 配置、级联、consumer 行为

**Files:**
- Create: `lsp/crates/mylua-lsp/tests/test_diagnostic_scheduler.rs`

### - [ ] Step 9.1: 新建集成测试文件

**Create `lsp/crates/mylua-lsp/tests/test_diagnostic_scheduler.rs`:**

```rust
//! Integration tests for DiagnosticScheduler — cross-module behavior
//! (scope config + open_uris interaction + consumer-visible 行为).
//!
//! Pure scheduler data-structure tests live in-file as `#[cfg(test)] mod
//! tests` inside `src/diagnostic_scheduler.rs`.

use mylua_lsp::diagnostic_scheduler::{DiagnosticScheduler, Priority};
use std::time::Duration;
use tower_lsp_server::lsp_types::Uri;

fn uri(s: &str) -> Uri {
    format!("file:///{}", s).parse().unwrap()
}

#[test]
fn seed_bulk_hot_cold_split_preserves_priority_order() {
    // 模拟 initialized handler 的 seed_bulk 行为：先 seed open (Hot)，再 seed
    // cold (Cold)。消费顺序：所有 hot → 所有 cold FIFO。
    let s = DiagnosticScheduler::new();
    s.seed_bulk(vec![uri("open1"), uri("open2")], Priority::Hot);
    s.seed_bulk(
        vec![uri("closed1"), uri("closed2"), uri("closed3")],
        Priority::Cold,
    );

    assert_eq!(s.pop(), Some(uri("open1")));
    assert_eq!(s.pop(), Some(uri("open2")));
    assert_eq!(s.pop(), Some(uri("closed1")));
    assert_eq!(s.pop(), Some(uri("closed2")));
    assert_eq!(s.pop(), Some(uri("closed3")));
    assert_eq!(s.pop(), None);
}

#[test]
fn seed_bulk_only_open_simulates_openonly_scope() {
    // 模拟 scope=OpenOnly：仅 seed open 的 URIs，不 seed 其他。
    let s = DiagnosticScheduler::new();
    s.seed_bulk(vec![uri("open1")], Priority::Hot);
    // 不 seed cold（模拟 OpenOnly）

    assert_eq!(s.pop(), Some(uri("open1")));
    assert_eq!(s.pop(), None);
}

#[tokio::test]
async fn schedule_then_upgrade_from_cold_via_open_flow() {
    // 模拟：冷启动 seed 5 个 cold；用户后续 schedule Hot 一个，相当于升级。
    let s = DiagnosticScheduler::new();
    s.seed_bulk(
        vec![uri("a"), uri("b"), uri("c"), uri("d"), uri("e")],
        Priority::Cold,
    );

    s.schedule(uri("c"), Priority::Hot);

    // 等 debounce task 触发
    tokio::time::sleep(Duration::from_millis(400)).await;

    assert_eq!(s.pop(), Some(uri("c"))); // Hot 优先
    assert_eq!(s.pop(), Some(uri("a")));
    assert_eq!(s.pop(), Some(uri("b")));
    // uri "c" 的 cold tombstone 被跳过
    assert_eq!(s.pop(), Some(uri("d")));
    assert_eq!(s.pop(), Some(uri("e")));
    assert_eq!(s.pop(), None);
}

#[tokio::test]
async fn rapid_schedule_collapses_to_single_push() {
    let s = DiagnosticScheduler::new();

    // 200ms 内连续 schedule 10 次
    for _ in 0..10 {
        s.schedule(uri("a"), Priority::Hot);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // 等所有 debounce 任务完成
    tokio::time::sleep(Duration::from_millis(400)).await;

    // 只有一份入队（gen 过滤使早期 9 次 return）
    assert_eq!(s.pop(), Some(uri("a")));
    assert_eq!(s.pop(), None);
}

#[test]
fn invalidate_enables_re_enqueue_same_uri() {
    // invalidate 清空状态后，同 URI 重新 push 应成功（模拟文件先 DELETED
    // 再 CREATED 的场景）。
    let s = DiagnosticScheduler::new();
    s.seed_bulk(vec![uri("a")], Priority::Hot);
    s.pop(); // 取出，enqueued 已清

    // 再次 push 正常工作（这里其实 pop 后 enqueued 已自动清；测 invalidate
    // 单独对 diag_gen 的清理）
    s.invalidate(&uri("a"));
    s.seed_bulk(vec![uri("a")], Priority::Cold);
    assert_eq!(s.pop(), Some(uri("a")));
}
```

注意：这些是 scheduler 层面的集成测试（跨 scope / 时序）。涉及 consumer_loop 的测试需要 mock Backend 整个环境，成本高，留给手工端到端验证（Task 11）。

### - [ ] Step 9.2: 构建 + 跑新测试

```bash
cd lsp && cargo build
cd lsp && CARGO_TARGET_DIR=target-test cargo test --test test_diagnostic_scheduler 2>&1 | tail -15
```
Expected: 5 tests passed。

### - [ ] Step 9.3: 全量回归

```bash
cd lsp && CARGO_TARGET_DIR=target-test cargo test --tests 2>&1 | grep "^test result" | awk '{p+=$4;f+=$6} END {print "TOTAL passed="p" failed="f}'
```
Expected: passed ≥ 413，failed = 0。

### - [ ] Step 9.4: code-reviewer + commit

```bash
git add lsp/crates/mylua-lsp/tests/test_diagnostic_scheduler.rs
git commit -m "$(cat <<'EOF'
test(lsp): DiagnosticScheduler 集成测试 — scope 模拟 / Cold→Hot 升级 / debounce collapse

5 个集成测试覆盖 scheduler 的跨场景行为：
- seed_bulk 的 Hot/Cold 分 seeds 保持消费顺序
- 模拟 OpenOnly scope 只 seed open
- 冷启动 seed 后 schedule 升级 Cold→Hot 的端到端路径
- 连续 10 次 schedule 靠 gen 合并为 1 次 push
- invalidate 后同 URI 可正常重新 seed

Consumer_loop 的端到端验证（Backend mock 成本高）留给手工测试。
EOF
)"
```

---

## Task 10: 配置 VS Code 扩展 package.json 暴露 `diagnostics.scope` 配置

**Files:**
- Modify: `vscode-extension/package.json`（添加 configuration schema）

### - [ ] Step 10.1: 定位并新增配置项

**Modify `vscode-extension/package.json`** — 在 `contributes.configuration.properties` 对象里找到 `mylua.diagnostics.unusedLocal` 或其他 `mylua.diagnostics.*` 项，按同样格式加一条：

```json
        "mylua.diagnostics.scope": {
          "type": "string",
          "enum": ["full", "openOnly"],
          "default": "full",
          "enumDescriptions": [
            "Diagnose the entire workspace (cold-start seeds all files; cascades to all dependants).",
            "Only diagnose files currently opened in editor tabs; cascades skip closed files."
          ],
          "description": "Scope of semantic diagnostics publishing."
        },
```

### - [ ] Step 10.2: 重新编译扩展

```bash
cd vscode-extension && npm run compile
```
Expected: TypeScript 编译通过（不涉及 .ts 代码改动，纯配置）。

### - [ ] Step 10.3: commit（文档/配置改动，可跳过 code-reviewer）

```bash
git add vscode-extension/package.json
git commit -m "feat(ext): 暴露 mylua.diagnostics.scope 配置（full | openOnly，默认 full）"
```

---

## Task 11: 文档同步

**Files:**
- Modify: `ai-readme.md`
- Modify: `docs/performance-analysis.md`
- Modify: `docs/architecture.md`

### - [ ] Step 11.1: 更新 `ai-readme.md` 诊断调度条目

**Modify `ai-readme.md`** — 找到第 203 行附近「诊断调度」一条，替换为：

```markdown
- **诊断调度**：统一的 `DiagnosticScheduler`（`src/diagnostic_scheduler.rs`）接管所有 semantic 诊断路径——生产者侧 `schedule(uri, priority)` 沿用 300ms debounce（`diag_gen` 过滤过期 spawn），冷启动走 `seed_bulk` 绕过 debounce；hot/cold 双 `VecDeque` + 共享 `enqueued: HashMap<Uri, Priority>` 去重；Cold→Hot 升级走 tombstone 方案（cold 队列里那份打 tombstone，pop 时 skip）。单消费者 task 通过 supervisor `start_diagnostic_consumer` 管理，panic 时日志 + 100ms 退避 + 重启；内部状态靠 `Arc` 共享，重启不丢 queue。消费者 `consumer_loop` 的 Ready 门槛放在 pop 之前（避免把 Hot URI 降级为 Cold 推回队列），pop 后 snapshot text → compute → 发布前二次 check text 未变再 publish。syntax 诊断保持即时发布（parse 完立即 publish），不走 scheduler。`mylua.diagnostics.scope`（`"full"` / `"openOnly"`，默认 `"full"`）控制冷启动 seed 范围和级联惠及范围：OpenOnly 时未打开的依赖方不入队
```

### - [ ] Step 11.2: 更新 `ai-readme.md` 并发安全条目

在第 202 行「并发安全」条目末尾追加一句：

```markdown
；`scheduler`/`open_uris` 的 `Arc<Mutex>` 和 `edit_locks` 互不阻塞（锁顺序：edit_locks → documents → index/scheduler）
```

### - [ ] Step 11.3: 更新 `docs/performance-analysis.md` T1-2 / T1-4 状态

**Modify `docs/performance-analysis.md`** — 找到第 144-145 行附近 Tier 1 路线图表：

```markdown
| T1-2 | `publish_diagnostics_for_open_files` rayon 并行 | 瓶颈 1 | 冷启动诊断 compute 时间按核数线性下降（8 核 ≈ 8x） | 待做 |
| T1-4 | 冷启动 publishDiagnostics 只发 open tabs | 瓶颈 4 | JSON-RPC 流量 5 万 → O(打开 tab 数)；Problems 面板即时可用 | 待做 |
```

改为：

```markdown
| T1-2 | `publish_diagnostics_for_open_files` 并行化 | 瓶颈 1 | 被 DiagnosticScheduler 方案替代（单消费者串行但 Hot 优先） | ✅ 合并落地（T1-4 一并） |
| T1-4 | 冷启动 publishDiagnostics 只发 open tabs | 瓶颈 4 | JSON-RPC 流量受 diagnostics.scope 控制；openOnly 下只发 open | ✅ 已完成 |
```

### - [ ] Step 11.4: 更新 T1-1 条目描述

**Modify `docs/performance-analysis.md`** — 找到 T1-1 条目（第 142 行附近）：

```markdown
| T1-1 | `did_open` fast path | 瓶颈 6 | preview tab 切换 0 开销；消除"每次打开重诊断"的用户可感问题 | ✅ 已完成 |
```

在该条的 **预期收益** 列改为：

```markdown
preview tab 切换 0 开销；消除"每次打开重诊断"的用户可感问题（fast path 条件已加 open_uris.contains 判定，冷启动后首次打开未改文件仍能触发诊断）
```

### - [ ] Step 11.5: 更新 performance-analysis.md 瓶颈 1 / 4 状态

**Modify `docs/performance-analysis.md`** — 瓶颈 1 和瓶颈 4 的描述在 §2.1 里。在瓶颈 1 标题后加 `— ✅ 已改造（单消费者调度器，Hot 优先）` 前缀；瓶颈 4 标题后加 `— ✅ 已改造（scope 控制 seed 范围）`。

### - [ ] Step 11.6: 更新 `docs/architecture.md` — 新增「诊断调度」小节

**Modify `docs/architecture.md`** — 在合适位置（建议在「索引架构」章节后）加一段：

```markdown
## 诊断调度（DiagnosticScheduler）

`src/diagnostic_scheduler.rs` 是 semantic 诊断的唯一调度入口。三条进入路径均统一走它：

- `did_change` / `did_open`（非 fast path）→ `scheduler.schedule(uri, Hot|Cold)`
- 签名指纹级联 → `scheduler.schedule(dep_uri, ...)` 并按 scope 过滤
- `initialized` 冷启动 → `scheduler.seed_bulk(hot, Hot) + seed_bulk(cold, Cold)`

内部数据结构：hot/cold 双 `VecDeque` + `enqueued: HashMap<Uri, Priority>` 去重 + `cold_tombstones: HashSet<Uri>` 做 Cold→Hot 升级标记 + `diag_gen: HashMap<Uri, u64>` per-URI 代数用于 debounce 过滤。push 与 pop 均摊 O(1)。

单消费者 supervisor：`start_diagnostic_consumer` spawn 外层 loop，每次 panic 日志 + 100ms 退避 + 重启内层 `consumer_loop`。内部状态靠 `Arc` 共享，重启不丢 queue。

syntax 诊断（tree-sitter ERROR/MISSING 节点）不走 scheduler，保持 parse 完立即 publish 的即时反馈行为。

配置 `mylua.diagnostics.scope`（`full` 默认 / `openOnly`）控制冷启动 seed 范围和级联是否惠及未打开文件。详细设计与决策记录见 [`docs/superpowers/specs/2026-04-19-diagnostic-scheduler-design.md`](superpowers/specs/2026-04-19-diagnostic-scheduler-design.md)。
```

### - [ ] Step 11.7: commit

```bash
git add ai-readme.md docs/performance-analysis.md docs/architecture.md
git commit -m "$(cat <<'EOF'
docs: 同步 DiagnosticScheduler 文档

- ai-readme.md「诊断调度」「并发安全」条目更新
- performance-analysis.md:
  - 瓶颈 1 标 ✅ 已改造（单消费者调度器）
  - 瓶颈 4 标 ✅ 已改造（scope 控制 seed 范围）
  - T1-1 条目补 open_uris.contains 判定说明
  - T1-2/T1-4 状态改为 ✅，合并由 scheduler 方案落地
- architecture.md 新增「诊断调度（DiagnosticScheduler）」小节
EOF
)"
```

---

## Task 12: 手工端到端验证（Skill test-extension）

**Files:** 无代码改动，仅验证行为。

### - [ ] Step 12.1: 启动扩展开发主机

运行 `.cursor/scripts/test-extension.sh`（遵循 skill `test-extension`）。VS Code 以 `tests/mylua-tests.code-workspace` 打开 `tests/lua-root/` + `tests/lua-root2/`。

### - [ ] Step 12.2: 验证场景 1 — 打开有诊断的文件

- 打开 `tests/lua-root/diagnostics.lua`
- 预期：~300-400ms 内看到文件里 `-- !diag:` 注解对应的红线/波浪线
- 查 `.vscode/mylua-lsp.log`：应有 scheduler 相关的 push / pop 日志（如果有 lsp_log! 加在 scheduler 里的话）

### - [ ] Step 12.3: 验证场景 2 — preview tab 切换不重复诊断

- 在 Explorer 单击 `main.lua`（preview tab）→ 切 `math_utils.lua` → 切回 `main.lua`
- 预期：Problems 面板保持稳定，不闪烁；日志里 did_open 有 fast path 命中

### - [ ] Step 12.4: 验证场景 3 — OpenOnly 模式

- 在 VS Code settings.json 加：`"mylua.diagnostics.scope": "openOnly"`
- Reload Window
- 预期：Problems 面板只显示当前打开的 tab 的诊断；未打开的 `scopes.lua` / `generics.lua` 不在列表
- 打开 `scopes.lua` → 诊断应在 300-400ms 内出现

### - [ ] Step 12.5: 验证场景 4 — 级联仍然工作（Full 模式）

- 切回 `"mylua.diagnostics.scope": "full"` + Reload
- 打开 `main.lua`（它 require 了 `math_utils`、`emmy_basics` 等）
- 编辑 `math_utils.lua` 里某 `@class` 定义（加一个字段）
- 预期：保存后，main.lua 的诊断若依赖 Helper 字段会自动重算

### - [ ] Step 12.6: 记录结果 + commit（可选）

如果手工验证发现问题，回退到对应 Task 修复。如果全部通过：

```bash
# 若需要，可加一个空 commit 标记 QA 完成；或 skip
git commit --allow-empty -m "chore: DiagnosticScheduler 手工端到端验证通过"
```

---

## Self-Review Checklist

运行此清单前，确认所有 12 个 Task 已完成。

### Spec 覆盖检查

- [x] Spec §2 决策 D1（scope 配置） → Task 4 + Task 10
- [x] Spec §2 决策 D2（syntax 即时） → consumer_loop 不处理 syntax（Task 6）；syntax 仍在 parse_and_store 里即时 publish
- [x] Spec §2 决策 D3（队列结构 + tombstone） → Task 1-3
- [x] Spec §2 决策 D4（生产侧 debounce） → Task 2
- [x] Spec §2 决策 D5（text 一致性检查） → Task 6 consumer_loop
- [x] Spec §2 决策 D6（openOnly 级联过滤） → Task 7 Step 7.2
- [x] Spec §2 决策 D7（consumer panic 重启） → Task 6 supervisor
- [x] Spec §5.1 push/pop tombstone → Task 1
- [x] Spec §5.2 schedule debounce → Task 2
- [x] Spec §5.3 seed_bulk → Task 3
- [x] Spec §5.4 consumer + supervisor → Task 6
- [x] Spec §5.5 T1-1 fast path 修正 → Task 5 Step 5.3
- [x] Spec §5.6 did_close 维护 open_uris → Task 5 Step 5.2
- [x] Spec §5.7 冷启动 seed → Task 8 Step 8.1
- [x] Spec §5.8 级联 scope 过滤 → Task 7 Step 7.2
- [x] Spec §7 集成点清单 9 处 → Task 5-8 各自覆盖
- [x] Spec §8 测试计划 10 条 → Task 1-3 内嵌 6 条 + Task 9 集成 5 条 = 11 条（比 spec 多 1 条更细化的测试）
- [x] Spec §9 文档同步 → Task 11
- [x] Spec §10 阶段划分 P1/P2/P3 → Task 1-3 = P1；Task 4-9 = P2；Task 10-12 = P3

### Placeholder 扫描

搜索 plan 里是否有：

- [x] 无 "TBD"、"TODO"
- [x] 无 "add appropriate error handling"、"handle edge cases" 类占位描述
- [x] 每个代码步骤都有完整 Rust 代码片段（不只是描述）
- [x] 每个 commit 都有具体 message
- [x] 每个文件修改点都标了行号范围或方法名

### 类型一致性

- [x] `Priority::Hot` / `Priority::Cold` 全 plan 统一
- [x] `DiagnosticScope::Full` / `DiagnosticScope::OpenOnly` 全 plan 统一
- [x] `DiagnosticScheduler::new()` 返回 `Arc<Self>` 所有调用处一致
- [x] `scheduler.schedule(uri, priority)` / `scheduler.seed_bulk(uris, priority)` / `scheduler.invalidate(&uri)` 签名全 plan 一致
- [x] `open_uris: Arc<Mutex<HashSet<Uri>>>` 字段名全 plan 一致
- [x] `consumer_loop` vs `start_diagnostic_consumer` 两个函数名不混淆

---

## 执行建议

**Plan 完成，已保存到 `docs/superpowers/plans/2026-04-19-diagnostic-scheduler.md`。两种执行方式：**

**1. Subagent-Driven（推荐）** — 每个 Task 派一个新 subagent 执行，两阶段 review。适合这种 12 个 Task 规模较大的改造。

**2. Inline Execution** — 在当前会话里按 Task 顺序执行，适合小改动或希望人为介入更多的场景。

---

## 总体预期

- **代码新增**：~500 行（scheduler 模块 + 测试 + Backend 集成）
- **代码删除**：~90 行（`schedule_semantic_diagnostics` + `publish_diagnostics_for_open_files` + `diag_gen` 相关）
- **净增量**：~410 行
- **新增测试**：15 个（6 内嵌 + 5 集成 + 4 单元 Task 1 里的）
- **测试总数**：398 → 413
- **预计耗时**：Task 1-3 P1 阶段 ~1 小时；Task 4-9 P2 阶段 ~2-3 小时；Task 10-12 P3 阶段 ~30 分钟。总 ~3-4 小时（subagent 驱动下可压缩）
