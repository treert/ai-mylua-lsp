//! DiagnosticScheduler — 按优先级单线程调度 semantic 诊断计算。
//!
//! Hot 队列（已打开文件）严格优先于 Cold 队列（未打开/冷启动 seed）。
//! Cold→Hot 升级走 tombstone 方案：push 时直接入 hot 并在 cold 标记
//! 作废位，pop 时跳过。push 和 pop 均摊 O(1)。
//!
//! 生产者侧 `schedule` 带 300ms debounce（`diag_gen` 代数过滤过期任务）。
//! 冷启动 `seed_bulk` 绕过 debounce，批量入队后统一 notify 一次。
//!
//! 设计细节见 `docs/architecture.md` §3.4 与 `docs/performance-analysis.md` §6。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use crate::uri_id::UriId;

pub const DIAGNOSTIC_DEBOUNCE_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Priority {
    Hot,
    Cold,
}

struct Inner {
    hot: VecDeque<UriId>,
    cold: VecDeque<UriId>,
    /// 每个当前在队列里的 URI 的优先级；Cold→Hot 升级时更新为 Hot。
    enqueued: HashMap<UriId, Priority>,
    /// cold 队列里被升级过的 URI 集合；pop cold 时遇到则跳过。
    cold_tombstones: HashSet<UriId>,
    /// Per-URI 单调代数；`schedule` 生产者侧 debounce 过滤过期任务用。
    diag_gen: HashMap<UriId, u64>,
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
    fn push_to_queue(inner: &mut Inner, uri_id: UriId, priority: Priority) {
        match (inner.enqueued.get(&uri_id).copied(), priority) {
            (Some(Priority::Hot), _) => {}
            (Some(Priority::Cold), Priority::Hot) => {
                inner.cold_tombstones.insert(uri_id);
                inner.hot.push_back(uri_id);
                inner.enqueued.insert(uri_id, Priority::Hot);
            }
            (Some(Priority::Cold), Priority::Cold) => {}
            (None, Priority::Hot) => {
                inner.hot.push_back(uri_id);
                inner.enqueued.insert(uri_id, Priority::Hot);
            }
            (None, Priority::Cold) => {
                inner.cold.push_back(uri_id);
                inner.enqueued.insert(uri_id, Priority::Cold);
            }
        }
    }

    /// Pop 下一个待诊断的 URI。Hot 严格优先于 Cold；cold tombstone 会被跳过。
    pub fn pop(&self) -> Option<UriId> {
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

    /// 调度一个 URI 的 semantic 诊断计算。经 300ms debounce 窗口后入队。
    /// 连续多次 schedule 同一 URI → 所有 debounce task 里只有最后一个
    /// 真正 push（gen 代数过滤）；消费者只 compute 一次。
    pub fn schedule(self: &Arc<Self>, uri_id: UriId, priority: Priority) {
        let gen = {
            let mut inner = self.inner.lock().unwrap();
            let entry = inner.diag_gen.entry(uri_id).or_insert(0);
            *entry += 1;
            *entry
        };

        let scheduler = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS)).await;

            // 合并一次 lock：gen 过期检查 + push_to_queue 在同一临界区内完成，
            // 避免中间 gap 被新 schedule 的 task 插入（虽然 dedup 会兜底，但
            // 原子合并更优雅）。notify_one 放在锁释放之后。
            {
                let mut inner = scheduler.inner.lock().unwrap();
                let latest = inner.diag_gen.get(&uri_id).copied().unwrap_or(0);
                if latest != gen {
                    return;
                }
                Self::push_to_queue(&mut inner, uri_id, priority);
            }
            scheduler.notify.notify_one();
        });
    }

    /// 批量入队（冷启动专用）。绕过 debounce，末尾统一 notify 一次。
    pub fn seed_bulk(&self, uri_ids: Vec<UriId>, priority: Priority) {
        if uri_ids.is_empty() {
            return;
        }
        {
            let mut inner = self.inner.lock().unwrap();
            for uri_id in uri_ids {
                Self::push_to_queue(&mut inner, uri_id, priority);
            }
        }
        self.notify.notify_one();
    }

    /// 当前待处理 URI 数量（即尚未被 consumer pop 走的任务数）。
    pub fn pending_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.enqueued.len()
    }

    /// 文件 DELETED 时清空 scheduler 里与 `uri` 相关的状态。
    /// 不物理移除 hot/cold 队列里的残留——consumer 侧对 `documents` 不存在
    /// 的 URI 会跳过，自然容错。
    ///
    /// **语义注意**：清除 `cold_tombstones` 后，若 URI 在 cold 队列里有
    /// stale 残影（之前 Cold→Hot 升级时标记的），残影会被当作有效项在
    /// 后续 pop cold 时返回一次。对文件生命周期而言这是可接受的——
    /// 文件已 DELETED，consumer `documents.get(&uri)` 返回 None 会跳过；
    /// 若同 URI 之后再被 CREATED 并 `schedule(Cold)`，最多一次冗余
    /// compute（无副作用，publish 走一致性检查兜底）。
    pub fn invalidate(&self, uri_id: &UriId) {
        let mut inner = self.inner.lock().unwrap();
        inner.enqueued.remove(uri_id);
        inner.cold_tombstones.remove(uri_id);
        inner.diag_gen.remove(uri_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uri_id::intern;

    fn id(raw: i32) -> UriId {
        intern(format!("file:///diagnostic_scheduler/{}.lua", raw).parse().unwrap())
    }

    #[test]
    fn pop_respects_priority_hot_first_then_cold_fifo() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, id(2), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, id(3), Priority::Hot);
        }

        assert_eq!(s.pop(), Some(id(3)));
        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(2)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn push_dedups_same_uri_same_priority() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Hot);
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Hot);
        }

        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn push_cold_then_hot_upgrades_via_tombstone() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, id(2), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, id(3), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, id(2), Priority::Hot);
        }

        assert_eq!(s.pop(), Some(id(2))); // Hot 先
        assert_eq!(s.pop(), Some(id(1))); // cold[0]
        // b 在 cold 的残影被 tombstone 跳过
        assert_eq!(s.pop(), Some(id(3))); // cold[2]
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn push_hot_when_hot_already_enqueued_is_noop() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Hot);
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Cold); // 试图降级
        }

        assert_eq!(s.pop(), Some(id(1)));
        // 降级不生效，cold 里不应有残留
        assert_eq!(s.pop(), None);
    }

    #[tokio::test]
    async fn schedule_debounces_300ms_with_gen_collapse() {
        let s = DiagnosticScheduler::new();

        s.schedule(id(1), Priority::Hot);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.schedule(id(1), Priority::Hot);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.schedule(id(1), Priority::Hot);

        assert_eq!(s.pop(), None);

        tokio::time::sleep(Duration::from_millis(400)).await;

        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), None);
    }

    #[tokio::test]
    async fn schedule_notify_wakes_up_consumer() {
        let s = DiagnosticScheduler::new();
        let s2 = s.clone();

        let handle = tokio::spawn(async move {
            loop {
                if let Some(u) = s2.pop() {
                    return u;
                }
                s2.notified().await;
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        s.schedule(id(1), Priority::Hot);

        let got = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("consumer should wake up within 500ms")
            .expect("task finished");
        assert_eq!(got, id(1));
    }

    #[test]
    fn seed_bulk_bypasses_debounce_immediately_pops() {
        let s = DiagnosticScheduler::new();
        s.seed_bulk(
            vec![id(1), id(2), id(3)],
            Priority::Cold,
        );

        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(2)));
        assert_eq!(s.pop(), Some(id(3)));
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
        s.seed_bulk(vec![id(1), id(2)], Priority::Cold);
        s.seed_bulk(vec![id(1)], Priority::Hot);

        assert_eq!(s.pop(), Some(id(1))); // Hot 优先
        assert_eq!(s.pop(), Some(id(2))); // cold 残余
        assert_eq!(s.pop(), None); // uri a 的 cold tombstone 被跳过
    }

    #[test]
    fn invalidate_after_cold_to_hot_leaves_cold_residue_revivable() {
        // 场景：Cold(a) → Hot(a) upgrade 让 cold 队列有 stale(a) + tomb={a}；
        // 接着 invalidate(a) 清 tomb。stale 在后续 pop cold 会被返回一次——
        // 由 doc comment 明确为可接受语义（consumer 对已删除 URI 容错）。
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Cold);
            DiagnosticScheduler::push_to_queue(&mut inner, id(1), Priority::Hot);
        }
        assert_eq!(s.pop(), Some(id(1))); // Hot 被取走
        // 此时 cold 还有 stale(a)，tomb={a}
        s.invalidate(&id(1));
        // Doc-spec: stale 残影因 tomb 被清而能被 pop 出来一次
        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn invalidate_clears_all_state_for_uri() {
        let s = DiagnosticScheduler::new();
        {
            let mut inner = s.inner.lock().unwrap();
            inner.diag_gen.insert(id(1), 5);
            inner.cold_tombstones.insert(id(1));
            inner.enqueued.insert(id(1), Priority::Hot);
        }

        s.invalidate(&id(1));

        let inner = s.inner.lock().unwrap();
        assert!(!inner.diag_gen.contains_key(&id(1)));
        assert!(!inner.cold_tombstones.contains(&id(1)));
        assert!(!inner.enqueued.contains_key(&id(1)));
    }
}
