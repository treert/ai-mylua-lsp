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
pub mod type_inference;
pub mod type_system;
pub mod types;
pub mod uri_id;
pub mod util;
pub mod summary_cache;
pub mod workspace_scanner;
pub mod workspace_symbol;
mod handlers;
pub(crate) mod indexing;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Position encoding negotiation
// ---------------------------------------------------------------------------

/// Wire-format column encoding negotiated during `initialize`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColEncoding {
    /// UTF-16 code-unit offsets (LSP default, VS Code).
    Utf16 = 0,
    /// Byte (UTF-8) offsets.
    Utf8 = 1,
}

/// Negotiated position encoding for column offsets stored in `ByteRange`.
///
/// Set once during `initialize` based on client capability negotiation.
/// All `ByteRange` construction sites read this to decide how to encode
/// `start_col` / `end_col`.
pub(crate) static POSITION_ENCODING: AtomicU8 = AtomicU8::new(ColEncoding::Utf16 as u8);

/// Returns `true` when the negotiated position encoding is UTF-8
/// (i.e. column offsets in `ByteRange` are byte columns).
/// When `false` (the default), columns are UTF-16 code-unit offsets.
#[inline]
pub fn position_encoding_is_utf8() -> bool {
    POSITION_ENCODING.load(Ordering::Relaxed) == ColEncoding::Utf8 as u8
}

use tower_lsp_server::ls_types::notification::Notification;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::Client;

use aggregation::WorkspaceAggregation;
use config::LspConfig;
use document::Document;
use uri_id::{UriId, UriInterner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Initializing,
    /// Module index (require_map) is populated — `document_link` and
    /// `require` path completion work, but full semantic features
    /// (goto, hover, diagnostics) are not yet available.
    ModuleMapReady,
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
    /// Current indexing phase: "scanning", "parsing", "merging".
    /// Only present when `state == "indexing"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Human-readable message for the current phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Remaining files awaiting background diagnostics.
    /// Only present when `state == "diagnosing"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
}

pub enum IndexStatusNotification {}

impl Notification for IndexStatusNotification {
    type Params = IndexStatusParams;
    const METHOD: &'static str = "mylua/indexStatus";
}

pub struct Backend {
    pub(crate) client: Client,
    pub(crate) parser: Mutex<tree_sitter::Parser>,
    pub(crate) documents: Arc<Mutex<HashMap<Uri, Document>>>,
    pub(crate) index: Arc<Mutex<WorkspaceAggregation>>,
    pub(crate) workspace_roots: Mutex<Vec<PathBuf>>,
    pub(crate) config: Arc<Mutex<LspConfig>>,
    pub(crate) index_state: Arc<Mutex<IndexState>>,
    /// Per-URI async serialization for document-mutating handlers
    /// (`did_open` / `did_change`). Without this, two concurrent
    /// `did_change` for the same URI could both `.remove(&uri)` from the
    /// documents map and race on the re-insert, corrupting text state.
    /// The outer `std::sync::Mutex` only guards the HashMap itself; the
    /// inner `tokio::sync::Mutex` is awaited while parsing/applying edits.
    pub(crate) edit_locks: Arc<Mutex<HashMap<UriId, Arc<tokio::sync::Mutex<()>>>>>,
    /// Per-URI semantic-tokens delta cache: stores the token data
    /// last returned for each URI so `semanticTokens/full/delta` can
    /// compute a compact edit set. Keyed by session-local `UriId`;
    /// `result_id` itself comes from a global monotonic counter.
    pub(crate) semantic_tokens_cache: Arc<Mutex<HashMap<UriId, semantic_tokens::TokenCacheEntry>>>,
    /// Monotonic counter used to mint unique `result_id`s.
    pub(crate) semantic_tokens_counter: Arc<Mutex<u64>>,
    /// URIs currently in LSP `did_open` state (not yet `did_close`d).
    /// Used by:
    ///   - T1-1 fast path guard in `did_open` (skip parse only if already
    ///     open AND text matches)
    ///   - Diagnostic scheduler priority decision (Hot vs Cold)
    ///   - Cold-start seed routing (`initialized` splits documents into
    ///     Hot/Cold based on this set)
    pub(crate) open_uris: Arc<Mutex<HashSet<UriId>>>,
    /// URIs indexed via `config.workspace.library` (stdlib stubs and
    /// other external annotation packages). Populated by
    /// `run_workspace_scan` after library roots are resolved. Used by
    /// `consumer_loop` to publish an empty diagnostic set for these
    /// files — library stubs exist only to contribute type facts, so
    /// they should never clutter the client's Problems panel even if
    /// a stub file happens to contain tree-sitter ERROR nodes or
    /// shape-level warnings.
    pub(crate) library_uris: Arc<Mutex<HashSet<UriId>>>,
    /// Unified semantic diagnostics scheduler (priority queue + single
    /// consumer). Replaces the per-URI `schedule_semantic_diagnostics`
    /// spawns and the cold-start `publish_diagnostics_for_open_files`.
    pub(crate) scheduler: Arc<diagnostic_scheduler::DiagnosticScheduler>,
    /// Session-local URI interner used while gradually migrating
    /// hot internal paths from full `Uri` keys to compact `UriId` keys.
    pub(crate) uri_interner: Arc<UriInterner>,
}

pub(crate) struct ParsedFile {
    pub(crate) uri: Uri,
    pub(crate) lua_source: util::LuaSource,
    pub(crate) tree: tree_sitter::Tree,
    pub(crate) summary: summary::DocumentSummary,
    pub(crate) scope_tree: scope::ScopeTree,
}

pub(crate) fn semantic_tokens_legend() -> SemanticTokensLegend {
    semantic_tokens::semantic_tokens_legend()
}

pub(crate) fn new_parser() -> tree_sitter::Parser {
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
            uri_interner: Arc::new(UriInterner::new()),
        }
    }

    /// Mint a new semantic-tokens `result_id` string. Monotonic u64
    /// counter suffixed to `mylua-stk-` keeps IDs unique across the
    /// whole session (client-facing).
    pub(crate) fn mint_semantic_token_result_id(&self) -> String {
        let mut c = self.semantic_tokens_counter.lock().unwrap();
        *c += 1;
        format!("mylua-stk-{}", *c)
    }

    /// Resolve `config.workspace.library` into absolute roots and
    /// the corresponding URI set, write the UriId set into
    /// `self.library_uris`, and return both for downstream use by
    /// `run_workspace_scan`. Called from `initialized` BEFORE the
    /// handler's first `.await` so that tower-lsp-server cannot
    /// interleave a concurrent `did_open` / `did_change` on a
    /// library URI before `self.library_uris` is populated —
    /// otherwise `parse_and_store_with_old_tree` would fail to
    /// force `is_meta=true` on library URIs during the cold-start
    /// race window, leaving stubs flagged as regular user code and
    /// drowning in `undefinedGlobal` warnings.
    pub(crate) fn initialize_library_uris(
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
            *self.library_uris.lock().unwrap() = library_file_uris
                .iter()
                .cloned()
                .map(|uri| self.uri_interner.intern(uri))
                .collect();
            lsp_log!(
                "[mylua-lsp] library files to index: {}",
                library_file_uris.len()
            );
        }
        (library_roots, library_file_uris)
    }

    /// Fetch (or create) the per-URI async edit lock. Callers `.await` its
    /// `lock()` to serialize document mutations for a single URI.
    pub(crate) fn edit_lock_for(&self, uri: &Uri) -> Arc<tokio::sync::Mutex<()>> {
        let uri_id = self.uri_interner.intern(uri.clone());
        let mut locks = self.edit_locks.lock().unwrap();
        locks
            .entry(uri_id)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    pub(crate) fn parse_and_store(&self, uri: Uri, text: String) {
        self.parse_and_store_with_old_tree(uri, text, None);
    }

    /// Parse `text` optionally reusing an `old_tree` (already `.edit()`-ed
    /// to reflect the delta). When `old_tree` is provided, tree-sitter
    /// will incrementally reparse — only the changed regions get new
    /// nodes, everything else is reused.
    pub(crate) fn parse_and_store_with_old_tree(
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
                    let lua_source = util::LuaSource::new(text);
                    let (_, scope_tree) = summary_builder::build_file_analysis(&uri, &old, lua_source.source(), lua_source.line_index());
                    self.documents.lock().unwrap().insert(
                        uri,
                        Document { lua_source, tree: old, scope_tree },
                    );
                }
                return;
            }
        };

        {
            let lua_source = util::LuaSource::new(text);
            let (mut summary, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
            let uri_id = self.uri_interner.intern(uri.clone());
            // Library stubs retain their meta treatment across edits.
            // `summary_builder::build_file_analysis` infers `is_meta` from
            // `---@meta` headers, and bundled stdlib stubs typically
            // don't carry that header — without the override here, a
            // user navigating to `print`'s definition and editing the
            // stub would flip the flag back to false and start
            // triggering `undefinedGlobal` inside the library file.
            if self.library_uris.lock().unwrap().contains(&uri_id) {
                summary.is_meta = true;
            }

            let should_cascade = {
                let mut idx = self.index.lock().unwrap();
                let old_fp = idx.summary(&uri).map(|s| s.signature_fingerprint);
                let new_fp = summary.signature_fingerprint;
                idx.upsert_summary(summary);
                old_fp.is_some_and(|old| old != new_fp)
            };

            self.documents.lock().unwrap().insert(
                uri.clone(),
                Document { lua_source, tree, scope_tree },
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
            let is_open = self.open_uris.lock().unwrap().contains(&uri_id);
            let pri = if is_open {
                diagnostic_scheduler::Priority::Hot
            } else {
                diagnostic_scheduler::Priority::Cold
            };
            self.scheduler.schedule(uri_id, pri);

            // Cascade: signature-fingerprint change → re-diagnose other
            // files. Scope config (Full | OpenOnly) decides the set.
            if should_cascade {
                let diag_cfg = self.config.lock().unwrap().diagnostics.clone();
                if diag_cfg.enable {
                    let open: HashSet<UriId> = self.open_uris.lock().unwrap().clone();
                    match diag_cfg.scope {
                        config::DiagnosticScope::OpenOnly => {
                            for dep_uri_id in &open {
                                if *dep_uri_id != uri_id {
                                    self.scheduler.schedule(*dep_uri_id, diagnostic_scheduler::Priority::Hot);
                                }
                            }
                        }
                        config::DiagnosticScope::Full => {
                            let all_uris: Vec<Uri> = self.documents.lock().unwrap().keys().cloned().collect();
                            for dep_uri in all_uris {
                                if dep_uri == uri {
                                    continue;
                                }
                                let dep_uri_id = self.uri_interner.intern(dep_uri);
                                let pri = if open.contains(&dep_uri_id) {
                                    diagnostic_scheduler::Priority::Hot
                                } else {
                                    diagnostic_scheduler::Priority::Cold
                                };
                                self.scheduler.schedule(dep_uri_id, pri);
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn index_file_from_disk(&self, path: &std::path::Path) {
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
            let lua_source = util::LuaSource::new(text);
            let (mut summary, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
            // Keep library files flagged `is_meta=true` across
            // watcher-driven re-indexes. `summary_builder` infers
            // `is_meta` from an explicit `---@meta` header which
            // bundled stdlib stubs rarely carry, so without this
            // override a `didChangeWatchedFiles(CHANGED)` on a
            // library file (happens only when the user configured a
            // library path inside the workspace tree) would flip the
            // flag back to false and surface `undefinedGlobal`
            // warnings inside the stub file.
            let uri_id = self.uri_interner.intern(uri.clone());
            if self.library_uris.lock().unwrap().contains(&uri_id) {
                summary.is_meta = true;
            }
            self.index.lock().unwrap().upsert_summary(summary);
            self.documents
                .lock()
                .unwrap()
                .insert(uri, Document { lua_source, tree, scope_tree });
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
    pub(crate) async fn publish_syntax_only_during_indexing(&self, uri: &Uri) {
        if *self.index_state.lock().unwrap() == IndexState::Ready {
            return;
        }
        let Some(uri_id) = self.uri_interner.get(uri) else {
            return;
        };
        if !self.open_uris.lock().unwrap().contains(&uri_id) {
            return;
        }
        // Library stubs never publish diagnostics. Skipping here
        // matches the steady-state `consumer_loop` contract (which
        // also publishes an empty vector for library URIs) and
        // prevents a one-shot syntax publish from flashing in the
        // cold-start window, only to be cleared microseconds later.
        if self.library_uris.lock().unwrap().contains(&uri_id) {
            return;
        }
        let diags = {
            let docs = self.documents.lock().unwrap();
            let Some(doc) = docs.get(uri) else {
                return;
            };
            let syntax = diagnostics::collect_diagnostics(
                doc.tree.root_node(),
                doc.source(),
                doc.line_index(),
            );
            diagnostics::apply_diagnostic_suppressions(
                doc.tree.root_node(),
                doc.source(),
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

// Re-export `start_diagnostic_consumer` so `handlers.rs` can call it
// without reaching into `indexing::` directly.
pub(crate) use indexing::start_diagnostic_consumer;


pub(crate) fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.to_string();
    let path_str = s.strip_prefix("file:///")?;
    let decoded = percent_decode(path_str);
    if cfg!(not(windows)) {
        Some(PathBuf::from(format!("/{}", decoded)))
    } else {
        Some(PathBuf::from(decoded))
    }
}

/// Percent-decode a URI path — delegates to `util::percent_decode`.
fn percent_decode(s: &str) -> String {
    util::percent_decode(s)
}

#[cfg(test)]
mod tests {
    use super::percent_decode;
    #[cfg(not(windows))]
    use super::uri_to_path;
    #[cfg(not(windows))]
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

#[cfg(test)]
mod uri_id_tests {
    use crate::uri_id::{UriId, UriInterner};
    use tower_lsp_server::ls_types::Uri;

    #[test]
    fn intern_returns_stable_positive_ids() {
        let interner = UriInterner::new();
        let first: Uri = "file:///tmp/a.lua".parse().unwrap();
        let second: Uri = "file:///tmp/b.lua".parse().unwrap();

        let first_id = interner.intern(first.clone());
        let first_again = interner.intern(first);
        let second_id = interner.intern(second);

        assert_eq!(first_id, first_again);
        assert_ne!(first_id, second_id);
        assert_eq!(first_id.raw(), 1);
        assert_eq!(second_id.raw(), 2);
    }

    #[test]
    fn resolve_returns_original_uri() {
        let interner = UriInterner::new();
        let uri: Uri = "file:///tmp/a.lua".parse().unwrap();
        let id = interner.intern(uri.clone());

        assert_eq!(interner.resolve(id), Some(uri));
    }

    #[test]
    #[should_panic(expected = "UriId must be positive")]
    fn uri_id_rejects_zero() {
        let _ = UriId::new(0);
    }

    #[test]
    #[should_panic(expected = "UriId must be positive")]
    fn uri_id_rejects_negative_values() {
        let _ = UriId::new(-1);
    }

    #[test]
    fn interner_allocates_i32_max_then_panics_on_next_id() {
        let interner = UriInterner::for_test_next_id(i32::MAX);
        let first: Uri = "file:///tmp/a.lua".parse().unwrap();
        let second: Uri = "file:///tmp/b.lua".parse().unwrap();

        assert_eq!(interner.intern(first).raw(), i32::MAX);
        let panic = std::panic::catch_unwind(|| {
            let _ = interner.intern(second);
        });

        assert!(panic.is_err());
        let message = panic
            .unwrap_err()
            .downcast::<String>()
            .map(|s| *s)
            .or_else(|payload| payload.downcast::<&'static str>().map(|s| s.to_string()))
            .expect("panic payload should be a string");
        assert_eq!(message, "UriId exhausted");
    }
}

