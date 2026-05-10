//! DiagnosticScheduler — 按优先级单线程调度 semantic 诊断计算。
//!
//! Scheduler owns diagnostic target collection: callers report a changed
//! URI (and whether the change should cascade), then a shared 300ms debounce
//! window collects the actual files to diagnose from current config,
//! open files, and indexed documents.
//!
//! Modified files keep their highest priority until consumer `pop()` returns
//! that URI. New batches preserve previously queued files, then rebuild one
//! sorted ready queue: modified (newer first) → open → unopened.
//!
//! 设计细节见 `docs/architecture.md` §3.4 与 `docs/performance-analysis.md` §6。

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

use crate::config::{DiagnosticScope, LspConfig};
use crate::document::Document;
use crate::uri_id::{path_uri, UriId};

pub const DIAGNOSTIC_DEBOUNCE_MS: u64 = 300;

enum FileSource {
    Documents(Arc<Mutex<HashMap<UriId, Document>>>),
    Static(Mutex<HashSet<UriId>>),
}

struct Inner {
    ready_queue: VecDeque<UriId>,
    ready_set: HashSet<UriId>,
    /// Changed URIs that have not been popped yet. Higher sequence means
    /// more recent edit and therefore higher priority.
    modified: HashMap<UriId, u64>,
    /// Explicit one-file requests that should be diagnosed after debounce
    /// but should not outrank modified files.
    explicit: HashSet<UriId>,
    global_gen: u64,
    change_seq: u64,
    needs_cascade: bool,
}

struct CollectionSnapshot {
    open: HashSet<UriId>,
    scope: DiagnosticScope,
    all_uri_ids: Vec<UriId>,
}

pub struct DiagnosticScheduler {
    inner: Mutex<Inner>,
    notify: Notify,
    files: FileSource,
    open_uris: Arc<Mutex<HashSet<UriId>>>,
    config: Arc<Mutex<LspConfig>>,
}

impl DiagnosticScheduler {
    pub fn new(
        documents: Arc<Mutex<HashMap<UriId, Document>>>,
        open_uris: Arc<Mutex<HashSet<UriId>>>,
        config: Arc<Mutex<LspConfig>>,
    ) -> Arc<Self> {
        Self::with_file_source(FileSource::Documents(documents), open_uris, config)
    }

    pub fn new_for_test(
        uri_ids: Vec<UriId>,
        open_uri_ids: Vec<UriId>,
        scope: DiagnosticScope,
    ) -> Arc<Self> {
        let mut cfg = LspConfig::default();
        cfg.diagnostics.scope = scope;
        Self::with_file_source(
            FileSource::Static(Mutex::new(uri_ids.into_iter().collect())),
            Arc::new(Mutex::new(open_uri_ids.into_iter().collect())),
            Arc::new(Mutex::new(cfg)),
        )
    }

    fn with_file_source(
        files: FileSource,
        open_uris: Arc<Mutex<HashSet<UriId>>>,
        config: Arc<Mutex<LspConfig>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                ready_queue: VecDeque::new(),
                ready_set: HashSet::new(),
                modified: HashMap::new(),
                explicit: HashSet::new(),
                global_gen: 0,
                change_seq: 0,
                needs_cascade: false,
            }),
            notify: Notify::new(),
            files,
            open_uris,
            config,
        })
    }

    /// Pop 下一个待诊断的 URI。stale queue items are skipped via ready_set.
    pub fn pop(&self) -> Option<UriId> {
        let mut inner = self.inner.lock().unwrap();
        while let Some(uri_id) = inner.ready_queue.pop_front() {
            if inner.ready_set.remove(&uri_id) {
                inner.modified.remove(&uri_id);
                return Some(uri_id);
            }
        }
        None
    }

    /// 等下一次 ready queue rebuild/seed 唤醒。
    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    /// Record a changed URI and rebuild the diagnostic batch after the shared
    /// debounce window stays quiet for 300ms.
    pub fn schedule_changed(self: &Arc<Self>, uri_id: UriId, should_cascade: bool) {
        let gen = self.record_changed(uri_id, should_cascade);
        self.spawn_debounce(gen);
    }

    /// Schedule one URI without marking it as modified. Used for explicit
    /// requests like did_open fast path.
    pub fn schedule_uri(self: &Arc<Self>, uri_id: UriId) {
        let gen = {
            let mut inner = self.inner.lock().unwrap();
            inner.explicit.insert(uri_id);
            inner.global_gen += 1;
            inner.global_gen
        };
        self.spawn_debounce(gen);
    }

    fn spawn_debounce(self: &Arc<Self>, gen: u64) {
        let scheduler = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS)).await;
            if scheduler.rebuild_ready_queue_if_current(gen) {
                scheduler.notify.notify_one();
            }
        });
    }

    fn record_changed(&self, uri_id: UriId, should_cascade: bool) -> u64 {
        let mut inner = self.inner.lock().unwrap();
        inner.change_seq += 1;
        let seq = inner.change_seq;
        inner.modified.insert(uri_id, seq);
        inner.needs_cascade |= should_cascade;
        inner.global_gen += 1;
        inner.global_gen
    }

    fn rebuild_ready_queue_if_current(&self, gen: u64) -> bool {
        let snapshot = self.collection_snapshot();
        let mut inner = self.inner.lock().unwrap();
        if inner.global_gen != gen {
            return false;
        }
        Self::rebuild_ready_queue(&mut inner, &snapshot, false);
        !inner.ready_set.is_empty()
    }

    /// Cold-start seed. Bypasses debounce and collects from current scope.
    pub fn seed_workspace(&self) {
        let snapshot = self.collection_snapshot();
        let should_notify = {
            let mut inner = self.inner.lock().unwrap();
            Self::rebuild_ready_queue(&mut inner, &snapshot, true);
            !inner.ready_set.is_empty()
        };
        if should_notify {
            self.notify.notify_one();
        }
    }

    pub fn schedule_changed_now_for_test(&self, uri_id: UriId, should_cascade: bool) {
        self.record_changed(uri_id, should_cascade);
        let snapshot = self.collection_snapshot();
        let mut inner = self.inner.lock().unwrap();
        Self::rebuild_ready_queue(&mut inner, &snapshot, false);
    }

    pub fn schedule_uri_now_for_test(&self, uri_id: UriId) {
        let snapshot = self.collection_snapshot();
        let mut inner = self.inner.lock().unwrap();
        inner.explicit.insert(uri_id);
        Self::rebuild_ready_queue(&mut inner, &snapshot, false);
    }

    /// 当前待处理 URI 数量（即尚未被 consumer pop 走的任务数）。
    pub fn pending_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.ready_set.len()
    }

    /// 文件 DELETED 时清空 scheduler 里与 `uri` 相关的状态。
    pub fn invalidate(&self, uri_id: &UriId) {
        let mut inner = self.inner.lock().unwrap();
        inner.ready_set.remove(uri_id);
        inner.modified.remove(uri_id);
        inner.explicit.remove(uri_id);
    }

    fn collection_snapshot(&self) -> CollectionSnapshot {
        let open = { self.open_uris.lock().unwrap().clone() };
        let scope = { self.config.lock().unwrap().diagnostics.scope.clone() };
        let all_uri_ids = self.all_uri_ids();

        CollectionSnapshot {
            open,
            scope,
            all_uri_ids,
        }
    }

    fn rebuild_ready_queue(
        inner: &mut Inner,
        snapshot: &CollectionSnapshot,
        force_scope_seed: bool,
    ) {
        let mut candidates = inner.ready_set.clone();

        candidates.extend(inner.modified.keys().copied());
        candidates.extend(inner.explicit.drain());

        if force_scope_seed || inner.needs_cascade {
            match snapshot.scope {
                DiagnosticScope::OpenOnly => candidates.extend(snapshot.open.iter().copied()),
                DiagnosticScope::Full => candidates.extend(snapshot.all_uri_ids.iter().copied()),
            }
        }
        inner.needs_cascade = false;

        let mut ordered: Vec<_> = candidates.into_iter().collect();
        ordered.sort_by(|a, b| Self::compare_priority(*a, *b, &inner.modified, &snapshot.open));

        inner.ready_set = ordered.iter().copied().collect();
        inner.ready_queue = ordered.into_iter().collect();
    }

    fn all_uri_ids(&self) -> Vec<UriId> {
        match &self.files {
            FileSource::Documents(documents) => documents.lock().unwrap().keys().copied().collect(),
            FileSource::Static(uri_ids) => uri_ids.lock().unwrap().iter().copied().collect(),
        }
    }

    fn compare_priority(
        a: UriId,
        b: UriId,
        modified: &HashMap<UriId, u64>,
        open: &HashSet<UriId>,
    ) -> std::cmp::Ordering {
        match (modified.get(&a), modified.get(&b)) {
            (Some(a_seq), Some(b_seq)) => b_seq.cmp(a_seq),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => match (open.contains(&a), open.contains(&b)) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => path_uri(a).cmp(path_uri(b)),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DiagnosticScope;
    use crate::uri_id::intern_uri;
    use tower_lsp_server::ls_types::Uri;

    fn id(raw: i32) -> UriId {
        let uri: Uri = format!("file:///diagnostic_scheduler/{}.lua", raw)
            .parse()
            .unwrap();
        intern_uri(&uri)
    }

    #[test]
    fn pop_respects_modified_then_open_then_unopened_priority() {
        let s = DiagnosticScheduler::new_for_test(
            vec![id(1), id(2), id(3), id(4)],
            vec![id(2)],
            DiagnosticScope::Full,
        );
        s.schedule_changed_now_for_test(id(3), false);
        s.schedule_changed_now_for_test(id(1), false);
        s.seed_workspace();

        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(3)));
        assert_eq!(s.pop(), Some(id(2)));
        assert_eq!(s.pop(), Some(id(4)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn explicit_schedule_dedups_same_uri() {
        let s = DiagnosticScheduler::new_for_test(vec![id(1)], vec![id(1)], DiagnosticScope::Full);
        s.schedule_uri_now_for_test(id(1));
        s.schedule_uri_now_for_test(id(1));

        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn modified_entry_survives_rebuild_until_popped() {
        let s = DiagnosticScheduler::new_for_test(
            vec![id(1), id(2), id(3)],
            vec![id(1), id(2), id(3)],
            DiagnosticScope::Full,
        );
        s.schedule_changed_now_for_test(id(2), false);
        s.schedule_changed_now_for_test(id(3), false);
        assert_eq!(s.pop(), Some(id(3)));
        s.seed_workspace();

        assert_eq!(s.pop(), Some(id(2))); // Hot 先
        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(3)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn openonly_cascade_collects_open_files() {
        let s = DiagnosticScheduler::new_for_test(
            vec![id(1), id(2), id(3)],
            vec![id(1), id(3)],
            DiagnosticScope::OpenOnly,
        );
        s.schedule_changed_now_for_test(id(2), true);

        assert_eq!(s.pop(), Some(id(2)));
        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(3)));
        assert_eq!(s.pop(), None);
    }

    #[tokio::test]
    async fn schedule_debounces_with_shared_gen_collapse() {
        let s =
            DiagnosticScheduler::new_for_test(vec![id(1), id(2)], vec![], DiagnosticScope::Full);
        s.schedule_changed(id(1), false);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.schedule_changed(id(2), false);
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.schedule_changed(id(1), false);
        assert_eq!(s.pop(), None);

        tokio::time::sleep(Duration::from_millis(400)).await;

        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(2)));
        assert_eq!(s.pop(), None);
    }

    #[tokio::test]
    async fn schedule_notify_wakes_up_consumer() {
        let s = DiagnosticScheduler::new_for_test(vec![id(1)], vec![], DiagnosticScope::Full);
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

        s.schedule_changed(id(1), false);

        let got = tokio::time::timeout(Duration::from_millis(500), handle)
            .await
            .expect("consumer should wake up within 500ms")
            .expect("task finished");
        assert_eq!(got, id(1));
    }

    #[test]
    fn seed_workspace_bypasses_debounce_immediately_pops() {
        let s = DiagnosticScheduler::new_for_test(
            vec![id(1), id(2), id(3)],
            vec![id(2)],
            DiagnosticScope::Full,
        );
        s.seed_workspace();

        assert_eq!(s.pop(), Some(id(2)));
        assert_eq!(s.pop(), Some(id(1)));
        assert_eq!(s.pop(), Some(id(3)));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn seed_workspace_empty_openonly_is_noop() {
        let s = DiagnosticScheduler::new_for_test(vec![id(1)], vec![], DiagnosticScope::OpenOnly);
        s.seed_workspace();
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn invalidate_skips_stale_ready_queue_entry() {
        let s = DiagnosticScheduler::new_for_test(vec![id(1)], vec![id(1)], DiagnosticScope::Full);
        s.schedule_changed_now_for_test(id(1), false);
        s.invalidate(&id(1));
        assert_eq!(s.pop(), None);
    }

    #[test]
    fn invalidate_clears_all_state_for_uri() {
        let s = DiagnosticScheduler::new_for_test(vec![id(1)], vec![id(1)], DiagnosticScope::Full);
        {
            let mut inner = s.inner.lock().unwrap();
            inner.modified.insert(id(1), 5);
            inner.explicit.insert(id(1));
            inner.ready_set.insert(id(1));
        }

        s.invalidate(&id(1));

        let inner = s.inner.lock().unwrap();
        assert!(!inner.modified.contains_key(&id(1)));
        assert!(!inner.explicit.contains(&id(1)));
        assert!(!inner.ready_set.contains(&id(1)));
    }
}
