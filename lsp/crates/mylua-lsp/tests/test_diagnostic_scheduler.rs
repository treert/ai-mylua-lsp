//! Integration tests for DiagnosticScheduler — cross-module behavior
//! (scope config + open_uris interaction + consumer-visible 行为).
//!
//! Pure scheduler data-structure tests live in-file as `#[cfg(test)] mod
//! tests` inside `src/diagnostic_scheduler.rs`.

use mylua_lsp::diagnostic_scheduler::{DiagnosticScheduler, Priority};
use std::time::Duration;
use tower_lsp_server::ls_types::Uri;

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

    s.invalidate(&uri("a"));
    s.seed_bulk(vec![uri("a")], Priority::Cold);
    assert_eq!(s.pop(), Some(uri("a")));
}
