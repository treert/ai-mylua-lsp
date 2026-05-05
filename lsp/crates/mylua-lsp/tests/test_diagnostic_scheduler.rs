//! Integration tests for DiagnosticScheduler — scope config, open_uris
//! interaction, global debounce, and consumer-visible ordering.

use mylua_lsp::config::DiagnosticScope;
use mylua_lsp::diagnostic_scheduler::{DiagnosticScheduler, DIAGNOSTIC_DEBOUNCE_MS};
use mylua_lsp::uri_id::{intern_uri, UriId};
use tower_lsp_server::ls_types::Uri;

fn uri(s: &str) -> Uri {
    format!("file:///{}", s).parse().unwrap()
}

fn id(s: &str) -> UriId {
    intern_uri(&uri(s))
}

#[test]
fn seed_workspace_full_prioritizes_open_before_unopened() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("closed2"), id("open1"), id("closed1"), id("open2")],
        vec![id("open1"), id("open2")],
        DiagnosticScope::Full,
    );

    s.seed_workspace();

    assert_eq!(s.pop(), Some(id("open1")));
    assert_eq!(s.pop(), Some(id("open2")));
    assert_eq!(s.pop(), Some(id("closed1")));
    assert_eq!(s.pop(), Some(id("closed2")));
    assert_eq!(s.pop(), None);
}

#[test]
fn seed_workspace_openonly_collects_only_open_files() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("closed1"), id("open1"), id("closed2")],
        vec![id("open1")],
        DiagnosticScope::OpenOnly,
    );

    s.seed_workspace();

    assert_eq!(s.pop(), Some(id("open1")));
    assert_eq!(s.pop(), None);
}

#[tokio::test]
async fn alternating_changes_share_one_global_debounce_window() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("a"), id("b"), id("c")],
        vec![id("a"), id("b")],
        DiagnosticScope::Full,
    );

    s.schedule_changed(id("a"), false);
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    s.schedule_changed(id("b"), false);
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    s.schedule_changed(id("a"), false);

    tokio::time::sleep(std::time::Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS - 100)).await;
    assert_eq!(s.pop(), None);

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(s.pop(), Some(id("a")));
    assert_eq!(s.pop(), Some(id("b")));
    assert_eq!(s.pop(), None);
}

#[tokio::test]
async fn cascade_full_collects_all_files_after_debounce() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("closed2"), id("open1"), id("changed"), id("closed1")],
        vec![id("open1")],
        DiagnosticScope::Full,
    );

    s.schedule_changed(id("changed"), true);
    tokio::time::sleep(std::time::Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS + 100)).await;

    assert_eq!(s.pop(), Some(id("changed")));
    assert_eq!(s.pop(), Some(id("open1")));
    assert_eq!(s.pop(), Some(id("closed1")));
    assert_eq!(s.pop(), Some(id("closed2")));
    assert_eq!(s.pop(), None);
}

#[test]
fn modified_priority_is_retained_until_uri_is_popped() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("a"), id("b"), id("c")],
        vec![id("a"), id("b"), id("c")],
        DiagnosticScope::Full,
    );

    s.schedule_changed_now_for_test(id("b"), false);
    s.schedule_changed_now_for_test(id("c"), false);
    assert_eq!(s.pop(), Some(id("c")));

    s.seed_workspace();

    assert_eq!(s.pop(), Some(id("b")));
    assert_eq!(s.pop(), Some(id("a")));
    assert_eq!(s.pop(), Some(id("c")));
    assert_eq!(s.pop(), None);
}

#[test]
fn pending_files_are_preserved_when_new_batch_is_built() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("a"), id("b"), id("c")],
        vec![id("a")],
        DiagnosticScope::OpenOnly,
    );

    s.schedule_uri_now_for_test(id("c"));
    s.schedule_changed_now_for_test(id("b"), false);

    assert_eq!(s.pop(), Some(id("b")));
    assert_eq!(s.pop(), Some(id("c")));
    assert_eq!(s.pop(), None);
}

#[test]
fn invalidate_removes_ready_and_modified_state() {
    let s = DiagnosticScheduler::new_for_test(
        vec![id("a"), id("b")],
        vec![id("a")],
        DiagnosticScope::Full,
    );

    s.schedule_changed_now_for_test(id("a"), false);
    s.invalidate(&id("a"));

    assert_eq!(s.pop(), None);
    s.seed_workspace();
    assert_eq!(s.pop(), Some(id("a")));
    assert_eq!(s.pop(), Some(id("b")));
}
