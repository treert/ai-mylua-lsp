#[macro_use]
pub mod logger;
pub mod aggregation;
pub mod call_hierarchy;
pub mod completion;
pub mod config;
pub mod diagnostic_scheduler;
pub mod diagnostics;
pub mod document;
pub mod document_highlight;
pub mod document_link;
pub mod emmy;
pub mod folding_range;
pub mod inlay_hint;
pub mod lua_builtins;
pub mod goto;
pub mod hover;
pub mod references;
pub mod rename;
pub mod resolver;
pub mod scope;
pub mod selection_range;
pub mod semantic_tokens;
pub mod signature_help;
pub mod summary;
pub mod summary_builder;
pub mod symbols;
pub mod table_shape;
pub mod type_system;
pub mod types;
pub mod util;
pub mod summary_cache;
pub mod workspace_scanner;
pub mod workspace_symbol;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::notification::Notification;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use aggregation::WorkspaceAggregation;
use config::LspConfig;
use document::Document;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Initializing,
    Ready,
}

/// Custom notification `mylua/indexStatus` pushed to the client
/// whenever indexing progress changes (scan start, per-batch, and
/// Ready). The VS Code extension uses it to drive a status-bar item
/// (`💛mylua 123/5000` → `💚mylua`). `state` is either `"indexing"`
/// or `"ready"`; `indexed`/`total` are file counts. `elapsed_ms` is
/// only populated on the terminal `"ready"` notification and carries
/// the wall-clock duration from the `initialized` handler entry to
/// the moment `IndexState::Ready` is committed — the extension uses
/// it to show a one-shot "索引完成，耗时 X.X 秒" toast.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatusParams {
    pub state: String,
    pub indexed: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

pub enum IndexStatusNotification {}

impl Notification for IndexStatusNotification {
    type Params = IndexStatusParams;
    const METHOD: &'static str = "mylua/indexStatus";
}

async fn send_index_status(client: &Client, state: &str, indexed: u64, total: u64) {
    client
        .send_notification::<IndexStatusNotification>(IndexStatusParams {
            state: state.to_string(),
            indexed,
            total,
            elapsed_ms: None,
        })
        .await;
}

async fn send_index_ready(client: &Client, indexed: u64, total: u64, elapsed_ms: u64) {
    client
        .send_notification::<IndexStatusNotification>(IndexStatusParams {
            state: "ready".to_string(),
            indexed,
            total,
            elapsed_ms: Some(elapsed_ms),
        })
        .await;
}

pub struct Backend {
    client: Client,
    parser: Mutex<tree_sitter::Parser>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    workspace_roots: Mutex<Vec<PathBuf>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    /// Per-URI async serialization for document-mutating handlers
    /// (`did_open` / `did_change`). Without this, two concurrent
    /// `did_change` for the same URI could both `.remove(&uri)` from the
    /// documents map and race on the re-insert, corrupting text state.
    /// The outer `std::sync::Mutex` only guards the HashMap itself; the
    /// inner `tokio::sync::Mutex` is awaited while parsing/applying edits.
    edit_locks: Arc<Mutex<HashMap<Uri, Arc<tokio::sync::Mutex<()>>>>>,
    /// Per-URI semantic-tokens delta cache: stores the token data
    /// last returned for each URI so `semanticTokens/full/delta` can
    /// compute a compact edit set. `u64` counter is appended to the
    /// URI to form `result_id`.
    semantic_tokens_cache: Arc<Mutex<HashMap<Uri, semantic_tokens::TokenCacheEntry>>>,
    /// Monotonic counter used to mint unique `result_id`s.
    semantic_tokens_counter: Arc<Mutex<u64>>,
    /// URIs currently in LSP `did_open` state (not yet `did_close`d).
    /// Used by:
    ///   - T1-1 fast path guard in `did_open` (skip parse only if already
    ///     open AND text matches)
    ///   - Diagnostic scheduler priority decision (Hot vs Cold)
    ///   - Cold-start seed routing (`initialized` splits documents into
    ///     Hot/Cold based on this set)
    open_uris: Arc<Mutex<HashSet<Uri>>>,
    /// URIs indexed via `config.workspace.library` (stdlib stubs and
    /// other external annotation packages). Populated by
    /// `run_workspace_scan` after library roots are resolved. Used by
    /// `consumer_loop` to publish an empty diagnostic set for these
    /// files — library stubs exist only to contribute type facts, so
    /// they should never clutter the client's Problems panel even if
    /// a stub file happens to contain tree-sitter ERROR nodes or
    /// shape-level warnings.
    library_uris: Arc<Mutex<HashSet<Uri>>>,
    /// Unified semantic diagnostics scheduler (priority queue + single
    /// consumer). Replaces the per-URI `schedule_semantic_diagnostics`
    /// spawns and the cold-start `publish_diagnostics_for_open_files`.
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
}

struct ParsedFile {
    uri: Uri,
    text: String,
    tree: tree_sitter::Tree,
    summary: summary::DocumentSummary,
    scope_tree: scope::ScopeTree,
}

fn semantic_tokens_legend() -> SemanticTokensLegend {
    semantic_tokens::semantic_tokens_legend()
}

fn new_parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_mylua::LANGUAGE.into())
        .expect("failed to load mylua grammar");
    parser
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Backend {
            client,
            parser: Mutex::new(new_parser()),
            documents: Arc::new(Mutex::new(HashMap::new())),
            index: Arc::new(Mutex::new(WorkspaceAggregation::new())),
            workspace_roots: Mutex::new(Vec::new()),
            config: Arc::new(Mutex::new(LspConfig::default())),
            index_state: Arc::new(Mutex::new(IndexState::Initializing)),
            edit_locks: Arc::new(Mutex::new(HashMap::new())),
            semantic_tokens_cache: Arc::new(Mutex::new(HashMap::new())),
            semantic_tokens_counter: Arc::new(Mutex::new(0)),
            open_uris: Arc::new(Mutex::new(HashSet::new())),
            library_uris: Arc::new(Mutex::new(HashSet::new())),
            scheduler: diagnostic_scheduler::DiagnosticScheduler::new(),
        }
    }

    /// Mint a new semantic-tokens `result_id` string. Monotonic u64
    /// counter suffixed to `mylua-stk-` keeps IDs unique across the
    /// whole session (client-facing).
    fn mint_semantic_token_result_id(&self) -> String {
        let mut c = self.semantic_tokens_counter.lock().unwrap();
        *c += 1;
        format!("mylua-stk-{}", *c)
    }

    /// Resolve `config.workspace.library` into absolute roots and
    /// the corresponding URI set, write the URI set into
    /// `self.library_uris`, and return both for downstream use by
    /// `run_workspace_scan`. Called from `initialized` BEFORE the
    /// handler's first `.await` so that tower-lsp-server cannot
    /// interleave a concurrent `did_open` / `did_change` on a
    /// library URI before `self.library_uris` is populated —
    /// otherwise `parse_and_store_with_old_tree` would fail to
    /// force `is_meta=true` on library URIs during the cold-start
    /// race window, leaving stubs flagged as regular user code and
    /// drowning in `undefinedGlobal` warnings.
    fn initialize_library_uris(
        &self,
        workspace_roots: &[PathBuf],
    ) -> (Vec<PathBuf>, HashSet<Uri>) {
        let (library_cfg, workspace_config) = {
            let cfg = self.config.lock().unwrap();
            (cfg.workspace.library.clone(), cfg.workspace.clone())
        };
        let library_roots =
            workspace_scanner::resolve_library_roots(&library_cfg, workspace_roots);
        if !library_roots.is_empty() {
            lsp_log!(
                "[mylua-lsp] library roots: {:?}",
                library_roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
            );
        }
        let library_file_uris: HashSet<Uri> = if library_roots.is_empty() {
            HashSet::new()
        } else {
            workspace_scanner::collect_lua_files(&library_roots, &workspace_config)
                .into_iter()
                .filter_map(|p| workspace_scanner::path_to_uri(&p))
                .collect()
        };
        if !library_file_uris.is_empty() {
            *self.library_uris.lock().unwrap() = library_file_uris.clone();
            lsp_log!(
                "[mylua-lsp] library files to index: {}",
                library_file_uris.len()
            );
        }
        (library_roots, library_file_uris)
    }

    /// Fetch (or create) the per-URI async edit lock. Callers `.await` its
    /// `lock()` to serialize document mutations for a single URI.
    fn edit_lock_for(&self, uri: &Uri) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.edit_locks.lock().unwrap();
        locks
            .entry(uri.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn parse_and_store(&self, uri: Uri, text: String) {
        self.parse_and_store_with_old_tree(uri, text, None);
    }

    /// Parse `text` optionally reusing an `old_tree` (already `.edit()`-ed
    /// to reflect the delta). When `old_tree` is provided, tree-sitter
    /// will incrementally reparse — only the changed regions get new
    /// nodes, everything else is reused.
    fn parse_and_store_with_old_tree(
        &self,
        uri: Uri,
        text: String,
        old_tree: Option<tree_sitter::Tree>,
    ) {
        let tree = {
            let mut parser = self.parser.lock().unwrap();
            // First try incremental reparse; on failure fall back to a
            // fresh parse so that parser state (timeout / limits) does not
            // propagate to the fresh attempt.
            parser
                .parse(text.as_bytes(), old_tree.as_ref())
                .or_else(|| {
                    let mut fresh = new_parser();
                    fresh.parse(text.as_bytes(), None)
                })
        };

        // If parsing failed even with a fresh parser (realistically only
        // possible under parser cancellation / limits, neither currently
        // enabled), preserve the previous document state rather than
        // dropping the document entirely. `did_change` removed the old
        // Document from the map; without this we'd leave the server
        // claiming the file doesn't exist.
        let tree = match tree {
            Some(t) => t,
            None => {
                if let Some(old) = old_tree {
                    let scope_tree = scope::build_scope_tree(&old, text.as_bytes());
                    self.documents.lock().unwrap().insert(
                        uri,
                        Document { text, tree: old, scope_tree },
                    );
                }
                return;
            }
        };

        {
            let mut summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
            // Library stubs retain their meta treatment across edits.
            // `summary_builder::build_summary` infers `is_meta` from
            // `---@meta` headers, and bundled stdlib stubs typically
            // don't carry that header — without the override here, a
            // user navigating to `print`'s definition and editing the
            // stub would flip the flag back to false and start
            // triggering `undefinedGlobal` inside the library file.
            if self.library_uris.lock().unwrap().contains(&uri) {
                summary.is_meta = true;
            }
            lsp_log!(
                "[index] summary for {:?}: locals={:?} types={:?} globals={}",
                uri,
                summary.local_type_facts.keys().collect::<Vec<_>>(),
                summary.type_definitions
                    .iter()
                    .map(|t| &t.name)
                    .collect::<Vec<_>>(),
                summary.global_contributions.len(),
            );

            // Snapshot the set of *indexed* URIs (entire workspace after
            // cold-start, not just client-opened ones) BEFORE locking
            // index, to avoid lock-order inversion with `consumer_loop`
            // (which locks documents then index). Named `indexed_uris`
            // to avoid confusion with the new `self.open_uris` field
            // which is strictly the set of client-opened URIs.
            let indexed_uris: std::collections::HashSet<Uri> =
                self.documents.lock().unwrap().keys().cloned().collect();

            let dependant_uris = {
                let mut idx = self.index.lock().unwrap();
                let old_fp = idx.summaries.get(&uri).map(|s| s.signature_fingerprint);
                let new_fp = summary.signature_fingerprint;

                // Snapshot the OLD summary's type names before the
                // upsert swaps the summary away. Together with the
                // new summary's names below, this covers:
                //   - rename (`@class Foo` → `@class Bar` — old `Foo`
                //     is in old set, lets us invalidate its dependants)
                //   - delete (`@class` annotation removed entirely —
                //     only old set contains the name)
                //   - add / edit (new set contains the name)
                let old_type_names: Vec<String> = idx
                    .summaries
                    .get(&uri)
                    .map(|s| s.type_definitions.iter().map(|t| t.name.clone()).collect())
                    .unwrap_or_default();
                let new_type_names: Vec<String> = summary
                    .type_definitions
                    .iter()
                    .map(|t| t.name.clone())
                    .collect();

                idx.upsert_summary(summary);

                if old_fp.map_or(false, |old| old != new_fp) {
                    let mut affected = old_type_names;
                    for n in new_type_names {
                        if !affected.contains(&n) {
                            affected.push(n);
                        }
                    }
                    collect_dependant_uris(&uri, &idx, &indexed_uris, &affected)
                } else {
                    Vec::new()
                }
            };

            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());

            self.documents.lock().unwrap().insert(
                uri.clone(),
                Document { text, tree, scope_tree },
            );

            // All diagnostics (both syntax and semantic) flow through
            // the unified scheduler → consumer_loop, which recomputes
            // syntax from the same tree and merges with semantic
            // before a single `publishDiagnostics`. This eliminates
            // the legacy two-step publish (syntax-first, semantic-
            // later) and its visible flicker on close→reopen with
            // unchanged content. Trade-off: syntax errors on
            // `did_change` now appear after the 300ms debounce
            // instead of immediately.
            //
            // Hot/Cold priority is decided by whether the client has
            // `did_open`'d this URI.
            let is_open = self.open_uris.lock().unwrap().contains(&uri);
            let pri = if is_open {
                diagnostic_scheduler::Priority::Hot
            } else {
                diagnostic_scheduler::Priority::Cold
            };
            self.scheduler.schedule(uri, pri);

            // Cascade: signature-fingerprint change → re-diagnose
            // dependent URIs. Scope config (Full | OpenOnly) decides
            // whether we also re-diagnose closed dependants. Snapshot
            // `open_uris` as a cheap clone (typical <100 opened URIs)
            // and drop the lock before the scheduler.schedule loop to
            // keep the lock-hold window tight — matches the style of
            // `initialized`'s seed routine below.
            let scope = self.config.lock().unwrap().diagnostics.scope.clone();
            let open: HashSet<Uri> = self.open_uris.lock().unwrap().clone();
            for dep_uri in dependant_uris {
                let dep_is_open = open.contains(&dep_uri);
                if !dep_is_open && matches!(scope, config::DiagnosticScope::OpenOnly) {
                    continue;
                }
                let dep_pri = if dep_is_open {
                    diagnostic_scheduler::Priority::Hot
                } else {
                    diagnostic_scheduler::Priority::Cold
                };
                self.scheduler.schedule(dep_uri, dep_pri);
            }
        }
    }

    fn index_file_from_disk(&self, path: &std::path::Path) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return,
        };
        let uri = match workspace_scanner::path_to_uri(path) {
            Some(u) => u,
            None => return,
        };
        let tree = {
            let mut parser = self.parser.lock().unwrap();
            parser.parse(text.as_bytes(), None)
        };
        if let Some(tree) = tree {
            let mut summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
            // Keep library files flagged `is_meta=true` across
            // watcher-driven re-indexes. `summary_builder` infers
            // `is_meta` from an explicit `---@meta` header which
            // bundled stdlib stubs rarely carry, so without this
            // override a `didChangeWatchedFiles(CHANGED)` on a
            // library file (happens only when the user configured a
            // library path inside the workspace tree) would flip the
            // flag back to false and surface `undefinedGlobal`
            // warnings inside the stub file.
            if self.library_uris.lock().unwrap().contains(&uri) {
                summary.is_meta = true;
            }
            self.index.lock().unwrap().upsert_summary(summary);
            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
            self.documents
                .lock()
                .unwrap()
                .insert(uri, Document { text, tree, scope_tree });
        }
    }

    /// Cold-start workaround: while `IndexState != Ready`, the unified
    /// `consumer_loop` gate-sleeps and publishes nothing — so users who
    /// open a file during workspace indexing would see no diagnostics
    /// at all for however long the scan takes.
    ///
    /// This helper publishes the *syntax-only* diagnostics snapshot
    /// (tree-sitter ERROR / MISSING nodes: mismatched brackets, missing
    /// `end`, malformed strings, etc.) immediately after a did_open /
    /// did_change parse, but only when two conditions hold:
    ///   1. `IndexState != Ready` — we are in the cold-start window.
    ///   2. `uri` is in `open_uris` — the client has an active editor
    ///      for this file and will render the diagnostics.
    ///
    /// Once the scan completes and `IndexState::Ready` is set,
    /// `consumer_loop` takes over with merged (syntax + semantic)
    /// diagnostics on its normal 300ms-debounced cadence. The client
    /// sees a progressive enhancement: first the syntax-only snapshot
    /// we published here, then a strict superset from `consumer_loop`.
    /// There is no "downgrade flicker" (the historical reason this
    /// kind of immediate syntax publish was removed in the past) —
    /// that scenario required a *prior* full publish to downgrade from,
    /// which cannot exist during cold-start when `consumer_loop` has
    /// never run for this URI.
    ///
    /// Applies `apply_diagnostic_suppressions` for consistency with
    /// `consumer_loop`, so `---@diagnostic disable-line syntax` still
    /// takes effect in the early publish.
    ///
    /// Does NOT interact with the scheduler: the `schedule(Hot)` call
    /// in `parse_and_store_with_old_tree` remains, and when Ready it
    /// will fire normally.
    async fn publish_syntax_only_during_indexing(&self, uri: &Uri) {
        if *self.index_state.lock().unwrap() == IndexState::Ready {
            return;
        }
        if !self.open_uris.lock().unwrap().contains(uri) {
            return;
        }
        // Library stubs never publish diagnostics. Skipping here
        // matches the steady-state `consumer_loop` contract (which
        // also publishes an empty vector for library URIs) and
        // prevents a one-shot syntax publish from flashing in the
        // cold-start window, only to be cleared microseconds later.
        if self.library_uris.lock().unwrap().contains(uri) {
            return;
        }
        let diags = {
            let docs = self.documents.lock().unwrap();
            let Some(doc) = docs.get(uri) else {
                return;
            };
            let syntax = diagnostics::collect_diagnostics(
                doc.tree.root_node(),
                doc.text.as_bytes(),
            );
            diagnostics::apply_diagnostic_suppressions(
                doc.tree.root_node(),
                doc.text.as_bytes(),
                syntax,
            )
        };
        lsp_log!(
            "[cold-start] syntax-only publish for {:?}: {} diags",
            uri,
            diags.len()
        );
        self.client.publish_diagnostics(uri.clone(), diags, None).await;
    }
}

/// Run the workspace scan as a background task (spawned from `initialized`).
///
/// Keeping the scan off the `initialized` handler lets tower-lsp resume
/// dispatching subsequent LSP messages (did_open / hover / completion /
/// semantic tokens) while the index is still being built. Handlers that
/// need the global index degrade gracefully during scanning:
///   - `consumer_loop` gates semantic diagnostics on `IndexState::Ready`
///     (already in place).
///   - goto / hover / completion / references return partial results
///     based on whatever is in the index at query time. For a URI not
///     yet scanned they still have the per-file AST from `did_open` and
///     can answer local queries.
///
/// `open_uris` is consulted before each batch's merge step so that a
/// file the client has already `did_open`'d is **not overwritten** by
/// the disk-snapshot version. `did_open`'s `parse_and_store` holds the
/// per-URI `edit_lock` and produces an authoritative (possibly
/// unsaved-buffer-based) `Document` + summary; re-inserting the disk
/// version would clobber the user's edits for the lifetime of the
/// session until the next `did_change`. See the lock-order notes in
/// `docs/performance-analysis.md` (edit_locks → open_uris →
/// documents → index).
async fn run_workspace_scan(
    client: Client,
    roots: Vec<PathBuf>,
    library_roots: Vec<PathBuf>,
    library_file_uris: HashSet<Uri>,
    config: Arc<Mutex<LspConfig>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    open_uris: Arc<Mutex<HashSet<Uri>>>,
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    index_state: Arc<Mutex<IndexState>>,
    started_at: std::time::Instant,
) {
    let (require_config, workspace_config, cache_mode, config_fingerprint, index_mode) = {
        let cfg = config.lock().unwrap();
        (
            cfg.require.clone(),
            cfg.workspace.clone(),
            cfg.index.cache_mode.clone(),
            summary_cache::compute_config_fingerprint(&cfg),
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

    // `library_roots` and `library_file_uris` are resolved
    // synchronously in `initialized` before this scan spawns (see
    // `initialize_library_uris`) so that any `did_open` arriving
    // before the scan makes progress can still observe the library
    // set and apply `is_meta=true` in `parse_and_store_with_old_tree`.
    //
    // Combined roots feed both the `require_map` scan (so
    // `require("string")` inside a user file resolves into the
    // library's `string.lua`) and `collect_lua_files` (so library
    // files get indexed alongside workspace files). We deduplicate
    // library roots that fall under an existing workspace root —
    // otherwise the same file would be read/parsed twice in the
    // parallel batch (once per root path), doubling I/O and merge
    // work for large library trees vendored inside the workspace.
    //
    // `workspace_roots` come from `uri_to_path(folder.uri)` and are
    // NOT canonicalized, while `library_roots` always are (by
    // `resolve_library_roots`). Without also canonicalizing the
    // workspace side, a symlinked workspace (`/Users/me/proj`
    // symlink to `/Users/me/project`) would miss the dedup and
    // double-scan. Fall back to the raw path if canonicalize fails
    // (deleted dir, permissions) so a transiently-unavailable
    // workspace root still contributes to `all_roots`.
    let canonical_roots: Vec<PathBuf> = roots
        .iter()
        .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()))
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
            .map(|r| summary_cache::SummaryCache::new(r, config_fingerprint))
    } else {
        None
    };

    let cached_summaries = Arc::new(cache.as_ref().map_or_else(HashMap::new, |c| c.load_all()));
    let cache_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let require_map =
        workspace_scanner::scan_workspace_lua_files(&all_roots, &require_config, &workspace_config);
    {
        let mut idx = index.lock().unwrap();
        idx.require_aliases = require_config.aliases.clone();
        for (module, uri) in &require_map {
            idx.set_require_mapping(module.clone(), uri.clone());
        }
    }

    let files = workspace_scanner::collect_lua_files(&all_roots, &workspace_config);
    let total = files.len();
    lsp_log!("[mylua-lsp] indexing {} .lua files (parallel)...", total);

    let token = NumberOrString::String("mylua-indexing".to_string());
    let progress = client
        .progress(token, "Indexing Lua workspace")
        .with_percentage(0)
        .with_message(format!("0/{} files", total))
        .begin()
        .await;

    send_index_status(&client, "indexing", 0, total as u64).await;

    // Smaller batch (50) keeps the `open_uris` merge critical section
    // short (~5ms), minimizing the window where concurrent `did_open`
    // is queued behind scan, and also yields per-batch status updates
    // to the client at ~50-file granularity for a smooth progress bar
    // in the VS Code status bar item.
    let batch_size = 50;
    let mut indexed = 0usize;
    let mut skipped_open = 0usize;

    for (batch_idx, chunk) in files.chunks(batch_size).enumerate() {
        let chunk_len = chunk.len();
        let chunk = chunk.to_vec();
        let cached_clone = cached_summaries.clone();
        let hits_clone = cache_hits.clone();

        let library_file_uris_clone = library_file_uris.clone();
        // Per-batch instrumentation: lets us pinpoint whether a
        // stall happens during (a) parallel parse, (b) main-thread
        // merge, or (c) client notification round-trip. Each log
        // is a handful of lines per batch, negligible next to the
        // indexing work itself.
        lsp_log!(
            "[scan] batch #{} start ({} files, indexed so far={})",
            batch_idx,
            chunk_len,
            indexed
        );
        let batch_started = std::time::Instant::now();
        let parsed: Vec<ParsedFile> = tokio::task::spawn_blocking(move || {
            use rayon::prelude::*;
            chunk
                .par_iter()
                .filter_map(|path| {
                    // Per-file SLOW log (> 500 ms): surfaces pathological
                    // files without flooding the log in the common case.
                    // When a file actually hangs, the last "[scan] parsing
                    // <path>" we emitted is usually enough to identify it
                    // from the post-mortem — but we emit that line only
                    // after read_to_string so we don't drown every scan in
                    // per-file output under normal operation. (If a future
                    // hang recurs, flip this back to unconditional "parsing"
                    // / "parsed" pairs around parse + build_summary.)
                    let file_started = std::time::Instant::now();
                    let text = std::fs::read_to_string(path).ok()?;
                    let uri = workspace_scanner::path_to_uri(path)?;
                    let content_hash = content_hash(&text);
                    let is_library = library_file_uris_clone.contains(&uri);

                    if let Some(cached) = cached_clone.get(&uri.to_string()) {
                        if cached.content_hash == content_hash {
                            hits_clone
                                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let mut parser = new_parser();
                            let tree = parser.parse(text.as_bytes(), None)?;
                            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
                            let mut summary = cached.clone();
                            // Force is_meta on library files even if
                            // the cached summary was produced before
                            // the library config existed. Stubs
                            // rarely carry an explicit `---@meta`
                            // header, so we can't rely on the
                            // builder to flag them.
                            if is_library {
                                summary.is_meta = true;
                            }
                            let elapsed_ms = file_started.elapsed().as_millis();
                            if elapsed_ms > 500 {
                                lsp_log!(
                                    "[scan] SLOW (cache-hit) {} ms: {}",
                                    elapsed_ms,
                                    path.display()
                                );
                            }
                            return Some(ParsedFile {
                                uri,
                                text,
                                tree,
                                summary,
                                scope_tree,
                            });
                        }
                    }

                    let mut parser = new_parser();
                    let tree = parser.parse(text.as_bytes(), None)?;
                    let mut summary =
                        summary_builder::build_summary(&uri, &tree, text.as_bytes());
                    if is_library {
                        summary.is_meta = true;
                    }
                    let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
                    let elapsed_ms = file_started.elapsed().as_millis();
                    if elapsed_ms > 500 {
                        lsp_log!(
                            "[scan] SLOW {} ms ({} bytes): {}",
                            elapsed_ms,
                            text.len(),
                            path.display()
                        );
                    }
                    Some(ParsedFile {
                        uri,
                        text,
                        tree,
                        summary,
                        scope_tree,
                    })
                })
                .collect()
        })
        .await
        .unwrap_or_else(|e| {
            lsp_log!("[mylua-lsp] indexing batch failed: {}", e);
            vec![]
        });
        lsp_log!(
            "[scan] batch #{} parsed {} files in {} ms",
            batch_idx,
            parsed.len(),
            batch_started.elapsed().as_millis()
        );

        // Hold `open_uris` for the full merge — any concurrent `did_open`
        // blocks on `open_uris.lock()` at `Backend::did_open` line
        // `self.open_uris.lock().unwrap().insert(uri.clone())`, so the
        // scan and did_open are forced into a strict before/after ordering
        // on a per-URI basis. Without this, `parse_and_store_with_old_tree`
        // uses two *separate* short-held `documents` locks and one
        // independent `index` lock, leaving gaps where scan can interleave
        // and either (a) stomp the buffer version entirely or (b) leave
        // `docs[uri]` and `index.summaries[uri]` in disagreement (buffer
        // vs disk). Holding `open_uris` for the ~50-item merge critical
        // section (all in-memory, typically ~5ms) closes both windows:
        // - URIs already in `open_uris` are skipped → did_open's write wins.
        // - URIs not yet in `open_uris` are written here; a later did_open
        //   for the same URI observes `open_uris` as "not inserted by us"
        //   after we release, takes the lock, and runs to completion
        //   atomically — overwriting our disk version with the buffer
        //   version in its own properly-ordered lock sequence.
        //
        // Lock order: open_uris → documents → index (canonical).
        // No existing handler holds `documents` or `index` while acquiring
        // `open_uris`, so nesting here does not create an inversion.
        let merge_started = std::time::Instant::now();
        {
            let open_held = open_uris.lock().unwrap();
            let mut docs = documents.lock().unwrap();
            let mut idx = index.lock().unwrap();
            for pf in parsed {
                if open_held.contains(&pf.uri) {
                    skipped_open += 1;
                    continue;
                }
                idx.upsert_summary(pf.summary);
                docs.insert(
                    pf.uri,
                    Document {
                        text: pf.text,
                        tree: pf.tree,
                        scope_tree: pf.scope_tree,
                    },
                );
            }
        }
        lsp_log!(
            "[scan] batch #{} merged in {} ms",
            batch_idx,
            merge_started.elapsed().as_millis()
        );

        indexed += chunk_len;
        let pct = ((indexed as u64) * 100 / total.max(1) as u64).min(99) as u32;
        progress.report(pct).await;
        send_index_status(&client, "indexing", indexed as u64, total as u64).await;
        lsp_log!("[mylua-lsp] indexed {}/{}", indexed, total);
    }

    let hits = cache_hits.load(std::sync::atomic::Ordering::Relaxed);
    if hits > 0 {
        lsp_log!("[mylua-lsp] cache hits: {}/{}", hits, total);
    }
    if skipped_open > 0 {
        lsp_log!(
            "[mylua-lsp] scan skipped {} open-file merges (did_open version kept)",
            skipped_open
        );
    }

    *index_state.lock().unwrap() = IndexState::Ready;
    progress.finish().await;
    let elapsed_ms = started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
    send_index_ready(&client, total as u64, total as u64, elapsed_ms).await;
    lsp_log!(
        "[mylua-lsp] workspace indexing complete: {} files (Ready) in {} ms",
        total,
        elapsed_ms
    );

    // Seed the diagnostics scheduler now that `IndexState::Ready` is
    // set — previously this lived directly in `initialized`, but with
    // the scan running in background we must defer it until scanning
    // finishes so `documents` is populated.
    //
    // - Full (default): hot = client-opened URIs, cold = rest of
    //   indexed workspace → consumer drains hot first, then cold.
    // - OpenOnly: only seed hot; closed files get no diagnostics
    //   until the user opens them.
    //
    // Lock acquisitions below are sequential (each released before
    // the next), but the ordering still matches canonical
    // `open_uris → documents → (…)` to make it obvious to future
    // maintainers that widening any of these scopes into a nested
    // hold would stay correct.
    let open: HashSet<Uri> = open_uris.lock().unwrap().clone();
    let all_uris: Vec<Uri> = documents.lock().unwrap().keys().cloned().collect();
    let diag_scope = config.lock().unwrap().diagnostics.scope.clone();
    let (hot, cold): (Vec<_>, Vec<_>) = all_uris.into_iter().partition(|u| open.contains(u));
    scheduler.seed_bulk(hot, diagnostic_scheduler::Priority::Hot);
    if matches!(diag_scope, config::DiagnosticScope::Full) {
        scheduler.seed_bulk(cold, diagnostic_scheduler::Priority::Cold);
    }

    client
        .log_message(MessageType::INFO, "mylua-lsp workspace scan complete")
        .await;

    if let Some(cache) = &cache {
        let summaries = index.lock().unwrap().summaries.clone();
        tokio::task::spawn_blocking({
            let cache_dir = cache.cache_dir().to_path_buf();
            let config_fp = config_fingerprint;
            move || {
                let c = summary_cache::SummaryCache::new_from_dir(cache_dir, config_fp);
                c.save_all(&summaries);
                lsp_log!("[mylua-lsp] saved {} summaries to cache", summaries.len());
            }
        });
    }
}

/// Supervisor for the diagnostic consumer task. Spawns `consumer_loop`
/// and auto-restarts it on panic (logs + 100ms backoff). The internal
/// scheduler state lives behind `Arc`, so a restarted consumer picks up
/// the existing queue without loss.
fn start_diagnostic_consumer(
    scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    library_uris: Arc<Mutex<HashSet<Uri>>>,
    client: Client,
) {
    tokio::spawn(async move {
        loop {
            let s = scheduler.clone();
            let d = documents.clone();
            let i = index.clone();
            let c = config.clone();
            let st = index_state.clone();
            let lu = library_uris.clone();
            let cl = client.clone();

            let handle = tokio::spawn(async move {
                consumer_loop(s, d, i, c, st, lu, cl).await;
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
            if let Some(u) = scheduler.pop() {
                break u;
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
            doc.text.clone()
        };

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

        // Consistency check: if the document's text changed while we
        // were computing (another did_change in flight), skip publish.
        // The newer edit already re-scheduled its own compute.
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

/// Collect URIs of dependent files (from the given candidate set) that
/// depend on `uri` either via `require()` (require_by_return) or via
/// Emmy type references (type_dependants — P1-7). `candidate_uris` is
/// the filter set — typically the whole indexed workspace; **not** to
/// be confused with `Backend.open_uris` (client-opened subset). The
/// scope-based filtering (open-only vs full) happens in the caller's
/// scheduler loop, not here.
///
/// De-duplicates across both sources so a file that both requires
/// `uri` AND references one of its classes only appears once.
fn collect_dependant_uris(
    uri: &Uri,
    idx: &aggregation::WorkspaceAggregation,
    candidate_uris: &std::collections::HashSet<Uri>,
    affected_type_names: &[String],
) -> Vec<Uri> {
    let mut seen: std::collections::HashSet<Uri> = std::collections::HashSet::new();
    let mut result = Vec::new();

    if let Some(deps) = idx.require_by_return.get(uri) {
        for dep in deps {
            if candidate_uris.contains(&dep.source_uri) && seen.insert(dep.source_uri.clone()) {
                result.push(dep.source_uri.clone());
            }
        }
    }

    // Cascade via the reverse type-dependency graph. `affected_type_names`
    // includes BOTH the old summary's type names (so
    // rename/delete still invalidates the abandoned name's
    // dependants) and the new summary's type names (covers
    // add/edit). The lib.rs call site is responsible for snapshotting
    // the old set before `upsert_summary` swaps the summary.
    for type_name in affected_type_names {
        if let Some(uris) = idx.type_dependants.get(type_name) {
            for dep_uri in uris {
                if dep_uri == uri {
                    continue;
                }
                if candidate_uris.contains(dep_uri) && seen.insert(dep_uri.clone()) {
                    result.push(dep_uri.clone());
                }
            }
        }
    }

    result
}

fn content_hash(s: &str) -> u64 {
    util::hash_bytes(s.as_bytes())
}

fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.to_string();
    let path_str = s.strip_prefix("file:///")?;
    let decoded = percent_decode(path_str);
    if cfg!(not(windows)) {
        Some(PathBuf::from(format!("/{}", decoded)))
    } else {
        Some(PathBuf::from(decoded))
    }
}

/// Percent-decode a URI path. Accumulates decoded bytes and interprets the
/// final buffer as UTF-8, so multi-byte encodings (e.g. `%E4%B8%AD` → 中)
/// are decoded correctly. Falls back to lossy decoding if the result is
/// not valid UTF-8.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out)
        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{percent_decode, uri_to_path};
    use tower_lsp_server::ls_types::Uri;

    #[test]
    fn percent_decode_ascii_space() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
    }

    #[test]
    fn percent_decode_utf8_chinese() {
        // %E4%B8%AD = U+4E2D "中"
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        assert_eq!(percent_decode("a/%E4%B8%AD/b.lua"), "a/中/b.lua");
    }

    #[test]
    fn percent_decode_lowercase_hex() {
        assert_eq!(percent_decode("%e4%b8%ad"), "中");
    }

    #[test]
    fn percent_decode_trailing_percent_untouched() {
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("abc%2"), "abc%2");
    }

    #[test]
    #[cfg(not(windows))]
    fn uri_to_path_decodes_utf8_paths() {
        let uri: Uri = "file:///Users/%E4%B8%AD%E6%96%87/x.lua".parse().unwrap();
        let path = uri_to_path(&uri).expect("should decode");
        assert_eq!(path.to_string_lossy(), "/Users/中文/x.lua");
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        let incoming_cfg = params
            .initialization_options
            .map(LspConfig::from_value);

        let mut roots = Vec::new();
        if let Some(folders) = &params.workspace_folders {
            for folder in folders {
                if let Some(path) = uri_to_path(&folder.uri) {
                    roots.push(path);
                }
            }
        }
        if roots.is_empty() {
            #[allow(deprecated)]
            if let Some(uri) = &params.root_uri {
                if let Some(path) = uri_to_path(uri) {
                    roots.push(path);
                }
            }
        }

        // Initialize the file logger as early as possible so every subsequent
        // `lsp_log!` (including the `config:` line below and anything in
        // `run_workspace_scan`) is captured in `.vscode/mylua-lsp.log`.
        if let Some(root) = roots.first() {
            let file_log = incoming_cfg
                .as_ref()
                .map(|c| c.debug.file_log)
                .unwrap_or_else(|| self.config.lock().unwrap().debug.file_log);
            logger::init(root, file_log);
        }

        if let Some(cfg) = incoming_cfg {
            lsp_log!("[mylua-lsp] config: {:?}", cfg);
            *self.config.lock().unwrap() = cfg;
        }

        *self.workspace_roots.lock().unwrap() = roots;

        // Boot the diagnostic scheduler consumer. It waits on an
        // `index_state == Ready` gate internally, so it's safe to
        // start before `initialized` fires / workspace scan completes.
        start_diagnostic_consumer(
            self.scheduler.clone(),
            self.documents.clone(),
            self.index.clone(),
            self.config.clone(),
            self.index_state.clone(),
            self.library_uris.clone(),
            self.client.clone(),
        );

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                declaration_provider: Some(DeclarationCapability::Simple(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![
                        ".".to_string(),
                        ":".to_string(),
                        "@".to_string(),
                        "\"".to_string(),
                        "'".to_string(),
                    ]),
                    resolve_provider: Some(true),
                    all_commit_characters: None,
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                    completion_item: None,
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![",".to_string()]),
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                }),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                })),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                document_link_provider: Some(DocumentLinkOptions {
                    // We resolve the target URI at link-emit time
                    // (require_map already knows it), so no lazy
                    // `documentLink/resolve` is needed.
                    resolve_provider: Some(false),
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                }),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_tokens_legend(),
                            // `delta: true` opts into
                            // `textDocument/semanticTokens/full/delta` so
                            // clients can pull only the changed portions
                            // of the token stream after the initial full
                            // response.
                            full: Some(SemanticTokensFullOptions::Delta { delta: Some(true) }),
                            range: Some(true),
                            work_done_progress_options: WorkDoneProgressOptions {
                                work_done_progress: None,
                            },
                        },
                    ),
                ),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "mylua-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            offset_encoding: None,
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // Capture the wall-clock start as the very first action: the
        // `elapsed_ms` reported to the client on the terminal
        // `mylua/indexStatus { state: "ready" }` notification is
        // measured from this point to the moment
        // `IndexState::Ready` is committed. The extension shows it as
        // a one-shot toast ("MyLua 索引完成，耗时 X.X 秒").
        let started_at = std::time::Instant::now();

        // Resolve library roots + URIs BEFORE the first `.await`.
        // `tower-lsp-server` may poll concurrent notifications (e.g.
        // `did_open`) once this handler yields at an `.await` point,
        // so populating `self.library_uris` up-front guarantees that
        // any cold-start `did_open` observing a library URI applies
        // `is_meta=true` in `parse_and_store_with_old_tree`. See
        // `initialize_library_uris` docstring for background.
        let roots = self.workspace_roots.lock().unwrap().clone();
        let (library_roots, library_file_uris) = self.initialize_library_uris(&roots);

        self.client
            .log_message(
                MessageType::INFO,
                "mylua-lsp initialized, scanning workspace in background...",
            )
            .await;

        // Spawn the workspace scan as a background task so `initialized`
        // returns immediately. tower-lsp dispatches subsequent messages
        // serially on a single task, so blocking here would queue every
        // `did_open` / `hover` / `completion` behind the whole scan.
        //
        // During the scan window `IndexState` remains `Initializing`:
        //   - `consumer_loop` gates semantic diagnostics on Ready.
        //   - goto / hover / completion / references serve per-file
        //     queries from whatever is in `documents` + `index` at
        //     the moment (potentially partial for cross-file lookups).
        //
        // Seed-bulk of the diagnostic scheduler moved into
        // `run_workspace_scan` after the `Ready` transition so that
        // `documents` is populated before we enumerate URIs to seed.
        let client = self.client.clone();
        // `roots`, `library_roots`, `library_file_uris` were resolved
        // at the very top of this handler (before the first `.await`)
        // so that concurrent `did_open` handlers observe a populated
        // `library_uris` set.
        let config = self.config.clone();
        let index = self.index.clone();
        let documents = self.documents.clone();
        let open_uris = self.open_uris.clone();
        let scheduler = self.scheduler.clone();
        let index_state = self.index_state.clone();

        tokio::spawn(async move {
            run_workspace_scan(
                client,
                roots,
                library_roots,
                library_file_uris,
                config,
                index,
                documents,
                open_uris,
                scheduler,
                index_state,
                started_at,
            )
            .await;
        });
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let lock = self.edit_lock_for(&uri);
        let _guard = lock.lock().await;

        // Fast path: skip re-parse / re-build summary when the indexed
        // document already matches the incoming buffer byte-for-byte.
        // After removing the legacy syntax-only spawn in
        // `parse_and_store`, `consumer_loop` is the sole diagnostics
        // publisher and always emits syntax + semantic merged from the
        // current tree. Fast path still schedules Hot to guarantee
        // the consumer processes this URI at least once — important
        // for `scope=openOnly` (where cold-start never seeded the
        // cold queue) and for `scope=Full` URIs whose cold-queue
        // entry hasn't yet been popped. Re-scheduling produces a
        // correct (usually identical, but not necessarily: a
        // cross-file cascade during close could have changed the
        // aggregation layer) `publishDiagnostics` payload, which
        // the client typically renders as a no-op.
        //
        // We intentionally compare *text only*, not version: clients
        // reset `version` to 1 on reopen but content is unchanged, and
        // conversely identical version numbers do not guarantee
        // identical content across clients. Byte-equality is the only
        // safe signal.
        //
        // The previous `is_tracked_open` gate has been removed: it
        // was there to prevent silent diagnostic skipping when the
        // `parse_and_store` syntax spawn was the only immediate
        // publisher. With that spawn gone, consumer_loop is the
        // single source of truth and scheduling Hot here is
        // sufficient.
        let text_matches = {
            let docs = self.documents.lock().unwrap();
            docs.get(&uri)
                .map_or(false, |d| d.text == params.text_document.text)
        };
        if text_matches {
            self.open_uris.lock().unwrap().insert(uri.clone());
            self.scheduler
                .schedule(uri, diagnostic_scheduler::Priority::Hot);
            return;
        }

        // Mark open BEFORE `parse_and_store` so the `scheduler.schedule`
        // call inside sees this URI as "open" and routes to the Hot
        // queue. Otherwise the very first did_open of a fresh URI
        // would route to Cold (steady state after workspace Ready,
        // no seed_bulk tombstone upgrade to save us).
        self.open_uris.lock().unwrap().insert(uri.clone());
        self.parse_and_store(uri.clone(), params.text_document.text);

        // Cold-start syntax-only fast path (no-op once IndexState::Ready).
        // See `publish_syntax_only_during_indexing` for rationale.
        self.publish_syntax_only_during_indexing(&uri).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;

        // Serialize concurrent did_change / did_open for the same URI.
        // The two-phase remove→process→insert in `parse_and_store_with_old_tree`
        // is not safe against interleaving otherwise.
        let lock = self.edit_lock_for(&uri);
        let _guard = lock.lock().await;

        // Apply changes sequentially. For each range-scoped change we
        // patch the stored text, call `tree.edit(&InputEdit)` so tree-sitter
        // can reuse unchanged subtrees, and finally reparse using the
        // edited tree as a base. A full-document change (range = None)
        // restarts from scratch.
        let (final_text, old_tree) = {
            let mut docs = self.documents.lock().unwrap();
            let mut text;
            let mut tree: Option<tree_sitter::Tree>;
            if let Some(doc) = docs.remove(&uri) {
                text = doc.text;
                tree = Some(doc.tree);
            } else {
                text = String::new();
                tree = None;
            }

            for change in params.content_changes {
                match change.range {
                    None => {
                        text = change.text;
                        tree = None;
                    }
                    Some(range) => {
                        let edit = util::apply_text_edit(&mut text, range, &change.text);
                        if let Some(ref mut t) = tree {
                            t.edit(&edit);
                        }
                    }
                }
            }

            (text, tree)
        };

        self.parse_and_store_with_old_tree(uri.clone(), final_text, old_tree);

        // Cold-start syntax-only fast path (no-op once IndexState::Ready).
        // `did_close` and `did_change_watched_files` intentionally do NOT
        // call this — see `publish_syntax_only_during_indexing` docstring
        // for the rationale (closed / watcher-driven events bypass the
        // in-editor cold-start window this path is meant to cover).
        self.publish_syntax_only_during_indexing(&uri).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        // Clear open_uris first — once the client says the file is
        // closed, subsequent scheduler priority decisions for this URI
        // should default to Cold. This happens *before* any fast-path
        // return so tab cycling correctly reflects the close event.
        //
        // Lock-ordering note: this `open_uris.remove` runs BEFORE the
        // `edit_lock` below. That's intentional — concurrent handlers
        // (did_change / did_open) queuing on `edit_lock` observe the
        // close state the moment they unblock, so their scheduling
        // decisions don't use a stale "tracked open" view. Since
        // `open_uris` is a plain `std::sync::Mutex` held only for the
        // `.remove(&uri)` call, it can't race with any other lock in
        // the `edit_lock → open_uris → documents → index` order used
        // elsewhere.
        self.open_uris.lock().unwrap().remove(&uri);
        // The client won't retry a stale `previous_result_id` after
        // closing the file, so drop the cache entry to free memory.
        self.semantic_tokens_cache.lock().unwrap().remove(&uri);

        // Workspace-indexing LSP: diagnostics for closed files remain
        // valid based on the index, so we do NOT clear them. If we
        // cleared, VS Code's preview-mode tab cycling (single-click
        // opens a preview tab that gets auto-closed when the user
        // single-clicks the next file) would fire `didClose` on
        // every tab-switch and silently erase diagnostics for
        // everything the user has ever focused.
        //
        // Instead, re-read the file from disk so any unsaved buffer
        // edits the user just discarded don't leave stale content in
        // the index, then let `parse_and_store` republish syntax +
        // semantic diagnostics and cascade to dependent open files.
        // `edit_locks` / scheduler state entries are kept — they're
        // reused on the next `did_open` for the same URI.
        if let Some(path) = uri_to_path(&uri) {
            // Acquire the lock BEFORE reading disk so a racing
            // `did_change` (unusual after `did_close` but clients
            // have bugs) can't sneak a buffer update in between our
            // read and our parse_and_store — otherwise we'd
            // overwrite fresher buffer content with stale disk.
            let lock = self.edit_lock_for(&uri);
            let _guard = lock.lock().await;
            match tokio::fs::read_to_string(&path).await {
                Ok(text) => {
                    // Fast path: if the current indexed content
                    // already matches disk, this close is just a
                    // clean tab-switch. Skip re-parse and re-publish
                    // entirely so VS Code's Problems panel, file
                    // badges, and squiggles don't flicker on every
                    // preview-mode tab toggle. Only when the buffer
                    // diverged (user edited then discarded) do we
                    // need to reset the index to disk state.
                    let already_matches = {
                        let docs = self.documents.lock().unwrap();
                        docs.get(&uri).map_or(false, |d| d.text == text)
                    };
                    if already_matches {
                        return;
                    }
                    self.parse_and_store(uri, text);
                    return;
                }
                Err(e) => {
                    lsp_log!(
                        "[did_close] fallback-clear {:?}: read failed ({})",
                        uri,
                        e
                    );
                }
            }
        } else {
            lsp_log!("[did_close] fallback-clear {:?}: non-file URI", uri);
        }
        // Non-file URI (e.g. `untitled:`) or the file was deleted on
        // disk between open and close: fall back to clearing. For the
        // deleted case `did_change_watched_files` will also fire a
        // DELETED event that removes the file from the index.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let roots = self.workspace_roots.lock().unwrap().clone();
        let (require_config, workspace_config) = {
            let cfg = self.config.lock().unwrap();
            (cfg.require.clone(), cfg.workspace.clone())
        };
        let filter = workspace_scanner::FileFilter::from_config(&workspace_config);

        for change in params.changes {
            match change.typ {
                FileChangeType::CREATED | FileChangeType::CHANGED => {
                    if let Some(path) = uri_to_path(&change.uri) {
                        if path.extension().map_or(false, |e| e == "lua") {
                            if !workspace_scanner::should_index_path(&path, &roots, &filter) {
                                continue;
                            }
                            self.index_file_from_disk(&path);
                            for root in &roots {
                                if path.starts_with(root) {
                                    let modules = workspace_scanner::file_to_module_paths(
                                        root,
                                        &path,
                                        &require_config.paths,
                                    );
                                    let mut idx = self.index.lock().unwrap();
                                    for module in modules {
                                        idx.set_require_mapping(
                                            module,
                                            change.uri.clone(),
                                        );
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                FileChangeType::DELETED => {
                    self.index.lock().unwrap().remove_file(&change.uri);
                    self.documents.lock().unwrap().remove(&change.uri);
                    self.scheduler.invalidate(&change.uri);
                    self.open_uris.lock().unwrap().remove(&change.uri);
                    self.edit_locks.lock().unwrap().remove(&change.uri);
                    self.semantic_tokens_cache.lock().unwrap().remove(&change.uri);
                    // Also drop from `library_uris` so a deleted
                    // library file doesn't leave a stale entry
                    // behind — otherwise a later file CREATED at
                    // the same path (perhaps of different content)
                    // would still be force-flagged meta just because
                    // its URI was previously registered.
                    self.library_uris.lock().unwrap().remove(&change.uri);
                }
                _ => {}
            }
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let cfg = LspConfig::from_value(params.settings);
        lsp_log!("[mylua-lsp] config updated: {:?}", cfg);
        *self.config.lock().unwrap() = cfg;
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        let idx = self.index.lock().unwrap();
        let summary = idx.summaries.get(&params.text_document.uri);
        let syms = symbols::collect_document_symbols(
            doc.tree.root_node(),
            doc.text.as_bytes(),
            summary,
        );
        Ok(Some(DocumentSymbolResponse::Nested(syms)))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(folding_range::folding_range(doc)))
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> Result<Option<Vec<DocumentLink>>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        let idx = self.index.lock().unwrap();
        Ok(Some(document_link::document_links(
            doc.tree.root_node(),
            doc.text.as_bytes(),
            &idx,
        )))
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&uri) else {
            return Ok(None);
        };
        let idx = self.index.lock().unwrap();
        let items = call_hierarchy::prepare_call_hierarchy(doc, &uri, position, &idx);
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(items))
        }
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let idx = self.index.lock().unwrap();
        let calls = call_hierarchy::incoming_calls(&params.item, &idx);
        Ok(Some(calls))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let idx = self.index.lock().unwrap();
        let calls = call_hierarchy::outgoing_calls(&params.item, &idx);
        Ok(Some(calls))
    }

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let idx = self.index.lock().unwrap();
        let cfg = self.config.lock().unwrap().inlay_hint.clone();
        Ok(Some(inlay_hint::inlay_hints(doc, uri, params.range, &idx, &cfg)))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(Some(selection_range::selection_range(doc, &params.positions)))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        Ok(document_highlight::document_highlight(doc, uri, position))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let mut idx = self.index.lock().unwrap();
        let strategy = self.config.lock().unwrap().goto_definition.strategy.clone();
        Ok(goto::goto_definition(doc, uri, position, &mut idx, &strategy))
    }

    async fn goto_type_definition(
        &self,
        params: request::GotoTypeDefinitionParams,
    ) -> Result<Option<request::GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let mut idx = self.index.lock().unwrap();
        let strategy = self.config.lock().unwrap().goto_definition.strategy.clone();
        Ok(goto::goto_type_definition(doc, uri, position, &mut idx, &strategy))
    }

    /// Lua has no distinct forward-declaration concept: "declaration"
    /// is the same as "definition". Alias to `goto_definition` so
    /// clients that prefer `textDocument/declaration` (e.g. some IDE
    /// refactor tools) get a sensible result.
    async fn goto_declaration(
        &self,
        params: request::GotoDeclarationParams,
    ) -> Result<Option<request::GotoDeclarationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let mut idx = self.index.lock().unwrap();
        let strategy = self.config.lock().unwrap().goto_definition.strategy.clone();
        Ok(goto::goto_definition(doc, uri, position, &mut idx, &strategy))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let mut idx = self.index.lock().unwrap();
        Ok(hover::hover(doc, uri, position, &mut idx, &docs))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let mut idx = self.index.lock().unwrap();
        let items = completion::complete(doc, uri, position, &mut idx);
        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn completion_resolve(
        &self,
        item: CompletionItem,
    ) -> Result<CompletionItem> {
        let idx = self.index.lock().unwrap();
        Ok(completion::resolve_completion(item, &idx))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let idx = self.index.lock().unwrap();
        let ref_strategy = self.config.lock().unwrap().references.strategy.clone();
        Ok(references::find_references(
            doc,
            uri,
            position,
            include_declaration,
            &idx,
            &docs,
            &ref_strategy,
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        Ok(rename::prepare_rename(doc, params.position))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let idx = self.index.lock().unwrap();
        match rename::rename(doc, uri, position, &params.new_name, &idx, &docs) {
            Ok(edit) => Ok(edit),
            Err(msg) => Err(tower_lsp_server::jsonrpc::Error {
                code: tower_lsp_server::jsonrpc::ErrorCode::InvalidParams,
                message: msg.into(),
                data: None,
            }),
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let idx = self.index.lock().unwrap();
        let results = workspace_symbol::search_workspace_symbols(&params.query, &idx);
        Ok(Some(WorkspaceSymbolResponse::Flat(results)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(uri) else {
            return Ok(None);
        };
        let mut idx = self.index.lock().unwrap();
        Ok(signature_help::signature_help(doc, uri, position, &mut idx))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&uri) else {
            return Ok(None);
        };
        let runtime_version = self.config.lock().unwrap().runtime.version.clone();
        let data = semantic_tokens::collect_semantic_tokens_with_version(
            doc.tree.root_node(),
            doc.text.as_bytes(),
            &doc.scope_tree,
            &runtime_version,
        );
        let result_id = self.mint_semantic_token_result_id();
        // Cache the full response so a subsequent `delta` request
        // with `previous_result_id == result_id` can diff against it.
        self.semantic_tokens_cache.lock().unwrap().insert(
            uri.clone(),
            semantic_tokens::TokenCacheEntry {
                result_id: result_id.clone(),
                data: data.clone(),
            },
        );
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: Some(result_id),
            data,
        })))
    }

    async fn semantic_tokens_full_delta(
        &self,
        params: SemanticTokensDeltaParams,
    ) -> Result<Option<SemanticTokensFullDeltaResult>> {
        let uri = params.text_document.uri;
        let previous_result_id = params.previous_result_id;

        // Load the current tokens regardless of cache state.
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&uri) else {
            return Ok(None);
        };
        let runtime_version = self.config.lock().unwrap().runtime.version.clone();
        let new_tokens = semantic_tokens::collect_semantic_tokens_with_version(
            doc.tree.root_node(),
            doc.text.as_bytes(),
            &doc.scope_tree,
            &runtime_version,
        );
        drop(docs);

        // If the cache matches the client's previous_result_id, emit
        // a delta (edits). Otherwise fall back to a full response so
        // the client can re-sync.
        let mut cache = self.semantic_tokens_cache.lock().unwrap();
        let new_result_id = self.mint_semantic_token_result_id();
        let cached_matches = cache
            .get(&uri)
            .map(|c| c.result_id == previous_result_id)
            .unwrap_or(false);
        if cached_matches {
            let old = cache.get(&uri).expect("cached_matches guarded above").data.clone();
            let edits = semantic_tokens::compute_semantic_token_delta(&old, &new_tokens);
            cache.insert(
                uri.clone(),
                semantic_tokens::TokenCacheEntry {
                    result_id: new_result_id.clone(),
                    data: new_tokens,
                },
            );
            Ok(Some(SemanticTokensFullDeltaResult::TokensDelta(
                SemanticTokensDelta {
                    result_id: Some(new_result_id),
                    edits,
                },
            )))
        } else {
            cache.insert(
                uri.clone(),
                semantic_tokens::TokenCacheEntry {
                    result_id: new_result_id.clone(),
                    data: new_tokens.clone(),
                },
            );
            Ok(Some(SemanticTokensFullDeltaResult::Tokens(SemanticTokens {
                result_id: Some(new_result_id),
                data: new_tokens,
            })))
        }
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        let runtime_version = self.config.lock().unwrap().runtime.version.clone();
        let data = semantic_tokens::collect_semantic_tokens_range_with_version(
            doc.tree.root_node(),
            doc.text.as_bytes(),
            &doc.scope_tree,
            params.range,
            &runtime_version,
        );
        Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
}
