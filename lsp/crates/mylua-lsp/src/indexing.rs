//! Workspace indexing pipeline and diagnostic consumer loop.
//!
//! This module contains the cold-start workspace scan (four-phase
//! pipeline: scan → module_map → parallel parse → atomic merge),
//! the unified diagnostic consumer loop, and related helpers.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tower_lsp_server::ls_types::*;
use tower_lsp_server::Client;

use crate::aggregation::WorkspaceAggregation;
use crate::config;
use crate::config::LspConfig;
use crate::diagnostic_scheduler;
use crate::diagnostics;
use crate::document::Document;
use crate::summary;
use crate::summary_builder;
use crate::summary_cache;
use crate::uri_id::UriInterner;
use crate::util;
use crate::workspace_scanner;
use crate::{new_parser, IndexState, IndexStatusNotification, IndexStatusParams, ParsedFile};

// ── Index status notification helpers ──────────────────────────────

pub(crate) async fn send_index_status(client: &Client, state: &str, indexed: u64, total: u64) {
    client
        .send_notification::<IndexStatusNotification>(IndexStatusParams {
            state: state.to_string(),
            indexed,
            total,
            elapsed_ms: None,
            phase: None,
            message: None,
            remaining: None,
        })
        .await;
}

pub(crate) async fn send_index_phase(
    client: &Client,
    phase: &str,
    message: &str,
    indexed: u64,
    total: u64,
) {
    client
        .send_notification::<IndexStatusNotification>(IndexStatusParams {
            state: "indexing".to_string(),
            indexed,
            total,
            elapsed_ms: None,
            phase: Some(phase.to_string()),
            message: Some(message.to_string()),
            remaining: None,
        })
        .await;
}

pub(crate) async fn send_index_ready(client: &Client, indexed: u64, total: u64, elapsed_ms: u64) {
    client
        .send_notification::<IndexStatusNotification>(IndexStatusParams {
            state: "ready".to_string(),
            indexed,
            total,
            elapsed_ms: Some(elapsed_ms),
            phase: None,
            message: None,
            remaining: None,
        })
        .await;
}

// ── Workspace scan ─────────────────────────────────────────────────

/// Run the workspace scan as a background task (spawned from `initialized`).
///
/// Four-phase pipeline:
///   1. **Scan** — discover `.lua` files + build `require_map`
///   2. **Parse** — rayon-parallel read + tree-sitter parse + build_summary
///   3. **Merge** — atomic one-pass construction of the global index
///      (`build_initial`) from all file summaries, then insert documents
///   4. **Ready** — flip `IndexState`, seed diagnostics
///
/// Phase 2 runs entirely off the main thread (no locks held). Phase 3
/// is a single critical section that holds `open_uris → documents →
/// index` for the duration of the merge — this is the "atomic sync
/// point" that eliminates the old batch-ordering bug where early files
/// couldn't resolve `require` targets defined in later batches.
///
/// Files that the client has already `did_open`'d before the merge
/// are skipped (their buffer version wins). After the merge, any
/// `did_open` / `did_change` arriving for an already-indexed URI goes
/// through the normal `upsert_summary` incremental path.
pub async fn run_workspace_scan(
    client: Client,
    roots: Vec<PathBuf>,
    library_roots: Vec<PathBuf>,
    library_file_uris: HashSet<Uri>,
    config: Arc<Mutex<LspConfig>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    open_uris: Arc<Mutex<HashSet<Uri>>>,
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    uri_interner: Arc<UriInterner>,
    index_state: Arc<Mutex<IndexState>>,
    started_at: std::time::Instant,
) {
    let (require_config, workspace_config, cache_mode, index_mode) = {
        let cfg = config.lock().unwrap();
        (
            cfg.require.clone(),
            cfg.workspace.clone(),
            cfg.index.cache_mode.clone(),
            cfg.workspace.index_mode.clone(),
        )
    };

    if index_mode == config::IndexMode::Isolated && roots.len() > 1 {
        lsp_log!(
            "[mylua-lsp] WARNING: indexMode 'isolated' is not yet implemented; \
             falling back to 'merged' for {} workspace roots",
            roots.len()
        );
    }

    // ── Phase 1: Scan ──────────────────────────────────────────────
    let phase1_started = std::time::Instant::now();
    send_index_phase(&client, "scanning", "Scanning workspace…", 0, 0).await;

    // Deduplicate library roots that fall under an existing workspace
    // root to avoid double-scanning. On Windows, `canonicalize()`
    // returns a `\\?\` verbatim prefix with an uppercase drive letter;
    // normalize it so `starts_with` comparisons against the already-
    // normalized `library_roots` work correctly.
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|r| {
            let c = r.canonicalize().unwrap_or_else(|_| r.clone());
            #[cfg(windows)]
            let c = workspace_scanner::normalize_windows_path(c);
            c
        })
        .collect();
    let mut all_roots = roots.clone();
    for lib in &library_roots {
        let already_covered = canonical_roots.iter().any(|r| lib.starts_with(r));
        if !already_covered {
            all_roots.push(lib.clone());
        } else {
            lsp_log!(
                "[mylua-lsp] library root {} already covered by workspace; \
                 skipping duplicate scan",
                lib.display()
            );
        }
    }

    let use_disk_cache = cache_mode == config::CacheMode::Summary;
    let cache = if use_disk_cache {
        roots
            .first()
            .map(|r| summary_cache::SummaryCache::new(r))
    } else {
        None
    };

    let cached_summaries = Arc::new(cache.as_ref().map_or_else(HashMap::new, |c| c.load_all()));

    let (module_entries, files) =
        workspace_scanner::scan_and_collect_lua_files(&all_roots, &require_config, &workspace_config);
    let total = files.len();

    let phase1_ms = phase1_started.elapsed().as_millis();
    lsp_log!(
        "[scan] phase 1 (scan): {} files discovered, module_index {} entries, {} ms",
        total,
        module_entries.len(),
        phase1_ms
    );

    // ── Phase 1.5: Populate module_index immediately ───────────────
    // The module_index (require_map) is fully determined by file paths
    // alone — no parsing needed. Write it into the index now so that
    // `document_link` (require string → clickable link) works during
    // the parse phase, before the full global index is built.
    {
        let mut idx = index.lock().unwrap();
        idx.require_aliases = require_config.aliases.clone();
        for (module, uri) in &module_entries {
            idx.set_require_mapping(module.clone(), uri.clone());
        }
    }
    *index_state.lock().unwrap() = IndexState::ModuleMapReady;
    send_index_phase(
        &client,
        "module_map_ready",
        &format!("Module map ready ({} entries), parsing…", module_entries.len()),
        0,
        total as u64,
    )
    .await;

    // ── Phase 2: Parse (parallel) ──────────────────────────────────
    let phase2_started = std::time::Instant::now();
    lsp_log!("[scan] phase 2 (parse): parsing {} files in parallel...", total);

    let token = NumberOrString::String("mylua-indexing".to_string());
    let progress = client
        .progress(token, "Indexing Lua workspace")
        .with_percentage(0)
        .with_message(format!("Scanning: {} files found", total))
        .begin()
        .await;

    send_index_phase(
        &client,
        "parsing",
        &format!("Parsing 0/{} files…", total),
        0,
        total as u64,
    )
    .await;

    // Parse all files in parallel using rayon. An AtomicUsize counter
    // tracks progress for periodic status updates without holding any
    // locks during the parse phase.
    let parse_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let cache_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // Spawn a progress-reporting task that periodically reads the
    // atomic counter and pushes status updates to the client. This
    // runs concurrently with the blocking parse task below.
    let progress_client = client.clone();
    let progress_counter = parse_counter.clone();
    let progress_total = total;
    let progress_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let done = progress_counter.load(std::sync::atomic::Ordering::Relaxed);
            if done >= progress_total {
                break;
            }
            let pct = ((done as u64) * 80 / progress_total.max(1) as u64).min(79) as u32;
            send_index_phase(
                &progress_client,
                "parsing",
                &format!("Parsing {}/{} files…", done, progress_total),
                done as u64,
                progress_total as u64,
            )
            .await;
            // Re-use the progress token for the percentage bar.
            // We don't have direct access to `progress` here, so
            // we send a raw notification. The percentage is capped
            // at 79% — the remaining 20% is reserved for the merge
            // phase, and 1% for the final ready transition.
            send_index_status(&progress_client, "indexing", done as u64, progress_total as u64).await;
            let _ = pct; // used conceptually for the progress bar
        }
    });

    let parsed: Vec<ParsedFile> = {
        let library_uris = library_file_uris.clone();
        let cached = cached_summaries.clone();
        let hits = cache_hits.clone();
        let counter = parse_counter.clone();

        tokio::task::spawn_blocking(move || {
            use rayon::prelude::*;
            files
                .par_iter()
                .filter_map(|path| {
                    let file_started = std::time::Instant::now();
                    let text = std::fs::read_to_string(path).ok()?;
                    let uri = workspace_scanner::path_to_uri(path)?;
                    let content_hash = content_hash(&text);
                    let is_library = library_uris.contains(&uri);

                    let lua_source = util::LuaSource::new(text);

                    let result = if let Some(cached_summary) = cached.get(&uri.to_string()) {
                        if cached_summary.content_hash == content_hash {
                            hits.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let mut parser = new_parser();
                            let tree = parser.parse(lua_source.source(), None)?;
                            let (_, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
                            let mut summary = cached_summary.clone();
                            if is_library {
                                summary.is_meta = true;
                            }
                            Some((tree, summary, scope_tree))
                        } else {
                            None // cache miss — fall through to fresh parse
                        }
                    } else {
                        None
                    };

                    let (tree, summary, scope_tree) = if let Some(hit) = result {
                        hit
                    } else {
                        let mut parser = new_parser();
                        let tree = parser.parse(lua_source.source(), None)?;
                        let (mut summary, scope_tree) =
                            summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
                        if is_library {
                            summary.is_meta = true;
                        }
                        (tree, summary, scope_tree)
                    };

                    let elapsed_ms = file_started.elapsed().as_millis();
                    if elapsed_ms > 500 {
                        lsp_log!(
                            "[scan] SLOW {} ms ({} bytes): {}",
                            elapsed_ms,
                            lua_source.text().len(),
                            path.display()
                        );
                    }

                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Some(ParsedFile { uri, lua_source, tree, summary, scope_tree })
                })
                .collect()
        })
        .await
        .unwrap_or_else(|e| {
            lsp_log!("[mylua-lsp] parallel parse failed: {}", e);
            vec![]
        })
    };

    // Stop the progress reporter.
    progress_task.abort();

    let phase2_ms = phase2_started.elapsed().as_millis();
    let hits = cache_hits.load(std::sync::atomic::Ordering::Relaxed);
    lsp_log!(
        "[scan] phase 2 (parse): {} files parsed in {} ms (cache hits: {})",
        parsed.len(),
        phase2_ms,
        hits
    );

    // ── Phase 3: Merge (atomic) ────────────────────────────────────
    let phase3_started = std::time::Instant::now();
    lsp_log!("[scan] phase 3 (merge): building global index from {} summaries...", parsed.len());
    send_index_phase(
        &client,
        "merging",
        &format!("Building global index ({} files)…", parsed.len()),
        total as u64,
        total as u64,
    )
    .await;
    progress.report(80).await;

    let mut skipped_open = 0usize;
    {
        // Hold all three locks for the entire merge — this is the
        // atomic sync point. The merge itself is O(total contributions)
        // and completes in tens of milliseconds even for 20k+ files.
        //
        // Lock order: open_uris → documents → index (canonical).
        let open_held = open_uris.lock().unwrap();
        let mut docs = documents.lock().unwrap();
        let mut idx = index.lock().unwrap();

        // module_index and aliases were already populated in Phase 1.5
        // (right after the scan). build_initial() does NOT clear
        // module_index — it only clears summaries, global_shard,
        // type_shard.
        // So module_index is already ready for
        // resolve_module_to_uri during the merge.

        // Separate parsed files into two sets: those already open in
        // the editor (skip — buffer version wins) and the rest.
        let mut summaries_to_merge: Vec<summary::DocumentSummary> = Vec::with_capacity(parsed.len());
        for pf in parsed {
            if open_held.contains(&pf.uri) {
                skipped_open += 1;
                continue;
            }
            summaries_to_merge.push(pf.summary);
            docs.insert(
                pf.uri,
                Document {
                    lua_source: pf.lua_source,
                    tree: pf.tree,
                    scope_tree: pf.scope_tree,
                },
            );
        }

        // Also collect summaries from files that were did_open'd
        // during the parse phase — they already have summaries in
        // the index from parse_and_store_with_old_tree, but
        // build_initial replaces the entire aggregation state, so
        // we must include them.
        for uri in open_held.iter() {
            if let Some(existing) = idx.summaries.get(uri) {
                summaries_to_merge.push(existing.clone());
            }
        }

        // Atomic one-pass construction of the global index.
        // This replaces the old batch-by-batch upsert_summary loop
        // and eliminates the file-ordering dependency bug.
        idx.build_initial(summaries_to_merge);
    }

    let phase3_ms = phase3_started.elapsed().as_millis();
    lsp_log!(
        "[scan] phase 3 (merge): global index built in {} ms (skipped {} open files)",
        phase3_ms,
        skipped_open
    );
    progress.report(95).await;

    // ── Phase 4: Ready ─────────────────────────────────────────────
    *index_state.lock().unwrap() = IndexState::Ready;
    progress.finish().await;
    let elapsed_ms = started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
    send_index_ready(&client, total as u64, total as u64, elapsed_ms).await;
    lsp_log!(
        "[mylua-lsp] workspace indexing complete: {} files (Ready) in {} ms \
         [scan={} ms, parse={} ms, merge={} ms]",
        total,
        elapsed_ms,
        phase1_ms,
        phase2_ms,
        phase3_ms
    );

    // Seed the diagnostics scheduler now that `IndexState::Ready` is
    // set. `documents` is fully populated at this point.
    let open: HashSet<Uri> = open_uris.lock().unwrap().clone();
    let all_uris: Vec<Uri> = documents.lock().unwrap().keys().cloned().collect();
    let diag_scope = config.lock().unwrap().diagnostics.scope.clone();
    let (hot, cold): (Vec<_>, Vec<_>) = all_uris.into_iter().partition(|u| open.contains(u));
    let hot_ids = hot.into_iter().map(|uri| uri_interner.intern(uri)).collect();
    scheduler.seed_bulk(hot_ids, diagnostic_scheduler::Priority::Hot);
    if matches!(diag_scope, config::DiagnosticScope::Full) {
        let cold_ids = cold.into_iter().map(|uri| uri_interner.intern(uri)).collect();
        scheduler.seed_bulk(cold_ids, diagnostic_scheduler::Priority::Cold);
    }

    client
        .log_message(MessageType::INFO, "mylua-lsp workspace scan complete")
        .await;

    if let Some(cache) = &cache {
        let summaries = index.lock().unwrap().summaries.clone();
        tokio::task::spawn_blocking({
            let cache_dir = cache.cache_dir().to_path_buf();
            move || {
                let c = summary_cache::SummaryCache::new_from_dir(cache_dir);
                c.save_all(&summaries);
                lsp_log!("[mylua-lsp] saved {} summaries to cache", summaries.len());
            }
        });
    }
}

// ── Diagnostic consumer ────────────────────────────────────────────

/// Supervisor for the diagnostic consumer task. Spawns `consumer_loop`
/// and auto-restarts it on panic (logs + 100ms backoff). The internal
/// scheduler state lives behind `Arc`, so a restarted consumer picks up
/// the existing queue without loss.
pub(crate) fn start_diagnostic_consumer(
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    uri_interner: Arc<UriInterner>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    library_uris: Arc<Mutex<HashSet<Uri>>>,
    client: Client,
) {
    // Diagnostic progress reporter: 100ms snapshot of remaining queue size.
    // Exits once the queue is first drained after having seen work.
    // Uses `seen_nonzero` to avoid a race where Ready is set but
    // seed_bulk hasn't run yet (pending_count would be 0 briefly).
    {
        let sched = scheduler.clone();
        let state = index_state.clone();
        let cl = client.clone();
        tokio::spawn(async move {
            let mut seen_nonzero = false;
            let mut ready_ticks: u32 = 0;
            loop {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if *state.lock().unwrap() != IndexState::Ready {
                    continue;
                }
                let remaining = sched.pending_count();
                if remaining > 0 {
                    seen_nonzero = true;
                    ready_ticks = 0;
                    cl.send_notification::<IndexStatusNotification>(IndexStatusParams {
                        state: "diagnosing".to_string(),
                        indexed: 0,
                        total: 0,
                        elapsed_ms: None,
                        phase: Some("diagnosing".to_string()),
                        message: Some(format!("{} files remaining", remaining)),
                        remaining: Some(remaining as u64),
                    })
                    .await;
                } else if seen_nonzero {
                    // All caught up — send final "ready" with remaining=0 and exit.
                    cl.send_notification::<IndexStatusNotification>(IndexStatusParams {
                        state: "ready".to_string(),
                        indexed: 0,
                        total: 0,
                        elapsed_ms: None,
                        phase: None,
                        message: None,
                        remaining: Some(0),
                    })
                    .await;
                    break;
                } else {
                    // Ready but seed_bulk hasn't fired yet — wait up to 2s.
                    ready_ticks += 1;
                    if ready_ticks >= 20 {
                        break; // Nothing was seeded (e.g. OpenOnly scope with no files).
                    }
                }
            }
        });
    }

    tokio::spawn(async move {
        loop {
            let s = scheduler.clone();
            let ui = uri_interner.clone();
            let d = documents.clone();
            let i = index.clone();
            let c = config.clone();
            let st = index_state.clone();
            let lu = library_uris.clone();
            let cl = client.clone();

            let handle = tokio::spawn(async move {
                consumer_loop(s, ui, d, i, c, st, lu, cl).await;
            });

            match handle.await {
                Ok(()) => break,
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

/// Single-consumer loop draining `DiagnosticScheduler.pop()`. Waits
/// for the workspace index to reach `Ready` before doing any work
/// (gated before pop — otherwise popping a Hot URI while Not Ready
/// would require re-enqueuing it which would silently downgrade to
/// Cold on the next loop iteration).
///
/// Mirrors the body of the legacy `schedule_semantic_diagnostics`
/// closure: snapshot text → compute (syntax + semantic) → text
/// consistency check → publish. Locks are held for the minimum
/// duration and never across `.await`.
async fn consumer_loop(
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    uri_interner: Arc<UriInterner>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    library_uris: Arc<Mutex<HashSet<Uri>>>,
    client: Client,
) {
    loop {
        if *index_state.lock().unwrap() != IndexState::Ready {
            tokio::time::sleep(Duration::from_millis(500)).await;
            continue;
        }

        let uri = loop {
            if let Some(uri_id) = scheduler.pop() {
                let Some(uri) = uri_interner.resolve(uri_id) else {
                    continue;
                };
                break uri;
            }
            scheduler.notified().await;
        };

        // Library stubs (Lua stdlib + user-configured annotation
        // packages) contribute type facts but should never produce
        // user-visible diagnostics — they're not the user's code.
        // Publishing an empty diagnostic vector clears any stale
        // state on the client side if this URI was ever diagnosed
        // previously (e.g. config change enabling the library path).
        let is_library = library_uris.lock().unwrap().contains(&uri);
        if is_library {
            client.publish_diagnostics(uri, Vec::new(), None).await;
            continue;
        }

        let snapshot = {
            let docs = documents.lock().unwrap();
            let Some(doc) = docs.get(&uri) else {
                continue;
            };
            doc.text().to_string()
        };

        let diags = {
            let docs = documents.lock().unwrap();
            let Some(doc) = docs.get(&uri) else {
                continue;
            };
            let mut syntax =
                diagnostics::collect_diagnostics(doc.tree.root_node(), doc.source(), doc.line_index());
            let idx = index.lock().unwrap();
            let cfg = config.lock().unwrap();
            let semantic = diagnostics::collect_semantic_diagnostics_with_version(
                doc.tree.root_node(),
                doc.source(),
                &uri,
                &idx,
                &doc.scope_tree,
                &cfg.diagnostics,
                &cfg.runtime.version,
                doc.line_index(),
            );
            syntax.extend(semantic);
            diagnostics::apply_diagnostic_suppressions(
                doc.tree.root_node(),
                doc.source(),
                syntax,
            )
        };

        // Consistency check: if the document's text changed while we
        // were computing (another did_change in flight), skip publish.
        // The newer edit already re-scheduled its own compute.
        let stale = {
            let docs = documents.lock().unwrap();
            match docs.get(&uri) {
                Some(doc) => doc.text() != snapshot,
                None => true,
            }
        };
        if stale {
            continue;
        }

        client.publish_diagnostics(uri, diags, None).await;
    }
}

// ── Helpers ────────────────────────────────────────────────────────

pub(crate) fn content_hash(s: &str) -> u64 {
    util::hash_bytes(s.as_bytes())
}
