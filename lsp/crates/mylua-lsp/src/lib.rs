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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use aggregation::WorkspaceAggregation;
use config::LspConfig;
use document::Document;

const DIAGNOSTIC_DEBOUNCE_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Initializing,
    Ready,
}

pub struct Backend {
    client: Client,
    parser: Mutex<tree_sitter::Parser>,
    documents: Arc<Mutex<HashMap<Uri, Document>>>,
    index: Arc<Mutex<WorkspaceAggregation>>,
    workspace_roots: Mutex<Vec<PathBuf>>,
    config: Arc<Mutex<LspConfig>>,
    index_state: Arc<Mutex<IndexState>>,
    diag_gen: Arc<Mutex<HashMap<Uri, u64>>>,
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
            diag_gen: Arc::new(Mutex::new(HashMap::new())),
            edit_locks: Arc::new(Mutex::new(HashMap::new())),
            semantic_tokens_cache: Arc::new(Mutex::new(HashMap::new())),
            semantic_tokens_counter: Arc::new(Mutex::new(0)),
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

    /// Fetch (or create) the per-URI async edit lock. Callers `.await` its
    /// `lock()` to serialize document mutations for a single URI.
    fn edit_lock_for(&self, uri: &Uri) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.edit_locks.lock().unwrap();
        locks
            .entry(uri.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn parse_and_store(&self, uri: Uri, text: String, version: Option<i32>) {
        self.parse_and_store_with_old_tree(uri, text, version, None);
    }

    /// Parse `text` optionally reusing an `old_tree` (already `.edit()`-ed
    /// to reflect the delta). When `old_tree` is provided, tree-sitter
    /// will incrementally reparse — only the changed regions get new
    /// nodes, everything else is reused.
    fn parse_and_store_with_old_tree(
        &self,
        uri: Uri,
        text: String,
        version: Option<i32>,
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
            let syntax_diags =
                diagnostics::collect_diagnostics(tree.root_node(), text.as_bytes());

            let summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
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

            // Snapshot open URIs BEFORE locking index, to avoid lock-order
            // inversion with schedule_semantic_diagnostics (which locks
            // documents then index).
            let open_uris: std::collections::HashSet<Uri> =
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
                    collect_dependant_uris(&uri, &idx, &open_uris, &affected)
                } else {
                    Vec::new()
                }
            };

            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());

            self.documents.lock().unwrap().insert(
                uri.clone(),
                Document { text, tree, scope_tree },
            );

            let client = self.client.clone();
            let uri_for_syntax = uri.clone();
            tokio::spawn(async move {
                client
                    .publish_diagnostics(uri_for_syntax, syntax_diags, version)
                    .await;
            });

            self.schedule_semantic_diagnostics(uri, version);

            for dep_uri in dependant_uris {
                self.schedule_semantic_diagnostics(dep_uri, None);
            }
        }
    }

    fn schedule_semantic_diagnostics(&self, uri: Uri, version: Option<i32>) {
        let gen = {
            let mut gens = self.diag_gen.lock().unwrap();
            let entry = gens.entry(uri.clone()).or_insert(0);
            *entry += 1;
            *entry
        };

        let diag_gen = self.diag_gen.clone();
        let documents = self.documents.clone();
        let index = self.index.clone();
        let config = self.config.clone();
        let index_state = self.index_state.clone();
        let client = self.client.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(DIAGNOSTIC_DEBOUNCE_MS)).await;

            let current_gen = diag_gen.lock().unwrap().get(&uri).copied().unwrap_or(0);
            if current_gen != gen {
                return;
            }

            if *index_state.lock().unwrap() != IndexState::Ready {
                return;
            }

            let diags = {
                let docs = documents.lock().unwrap();
                let Some(doc) = docs.get(&uri) else {
                    return;
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
                // `---@diagnostic disable-*` post-processing: filter
                // out suppressed entries and stamp the surviving ones
                // with a stable `code` slug for client display.
                diagnostics::apply_diagnostic_suppressions(
                    doc.tree.root_node(),
                    doc.text.as_bytes(),
                    syntax,
                )
            };

            client.publish_diagnostics(uri, diags, version).await;
        });
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
            let summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
            self.index.lock().unwrap().upsert_summary(summary);
            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
            self.documents
                .lock()
                .unwrap()
                .insert(uri, Document { text, tree, scope_tree });
        }
    }

    async fn scan_workspace_parallel(&self) {
        let roots = self.workspace_roots.lock().unwrap().clone();
        let (require_config, workspace_config, cache_mode, config_fingerprint, index_mode) = {
            let cfg = self.config.lock().unwrap();
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

        let require_map = workspace_scanner::scan_workspace_lua_files(&roots, &require_config, &workspace_config);
        {
            let mut idx = self.index.lock().unwrap();
            idx.require_aliases = require_config.aliases.clone();
            for (module, uri) in &require_map {
                idx.set_require_mapping(module.clone(), uri.clone());
            }
        }

        let files = workspace_scanner::collect_lua_files(&roots, &workspace_config);
        let total = files.len();
        lsp_log!("[mylua-lsp] indexing {} .lua files (parallel)...", total);

        let token = NumberOrString::String("mylua-indexing".to_string());
        let progress = self
            .client
            .progress(token, "Indexing Lua workspace")
            .with_percentage(0)
            .with_message(format!("0/{} files", total))
            .begin()
            .await;

        let batch_size = 200;
        let mut indexed = 0usize;

        for chunk in files.chunks(batch_size) {
            let chunk_len = chunk.len();
            let chunk = chunk.to_vec();
            let cached_clone = cached_summaries.clone();
            let hits_clone = cache_hits.clone();

            let parsed: Vec<ParsedFile> = tokio::task::spawn_blocking(move || {
                use rayon::prelude::*;
                chunk
                    .par_iter()
                    .filter_map(|path| {
                        let text = std::fs::read_to_string(path).ok()?;
                        let uri = workspace_scanner::path_to_uri(path)?;
                        let content_hash = content_hash(&text);

                        if let Some(cached) = cached_clone.get(&uri.to_string()) {
                            if cached.content_hash == content_hash {
                                hits_clone
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                let mut parser = new_parser();
                                let tree = parser.parse(text.as_bytes(), None)?;
                                let scope_tree =
                                    scope::build_scope_tree(&tree, text.as_bytes());
                                return Some(ParsedFile {
                                    uri,
                                    text,
                                    tree,
                                    summary: cached.clone(),
                                    scope_tree,
                                });
                            }
                        }

                        let mut parser = new_parser();
                        let tree = parser.parse(text.as_bytes(), None)?;
                        let summary =
                            summary_builder::build_summary(&uri, &tree, text.as_bytes());
                        let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
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

            {
                let mut docs = self.documents.lock().unwrap();
                let mut idx = self.index.lock().unwrap();
                for pf in parsed {
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

            indexed += chunk_len;
            let pct = ((indexed as u64) * 100 / total.max(1) as u64).min(99) as u32;
            progress.report(pct).await;
            lsp_log!("[mylua-lsp] indexed {}/{}", indexed, total);
        }

        let hits = cache_hits.load(std::sync::atomic::Ordering::Relaxed);
        if hits > 0 {
            lsp_log!("[mylua-lsp] cache hits: {}/{}", hits, total);
        }

        *self.index_state.lock().unwrap() = IndexState::Ready;
        progress.finish().await;
        lsp_log!(
            "[mylua-lsp] workspace indexing complete: {} files (Ready)",
            total
        );

        if let Some(cache) = &cache {
            let summaries = self.index.lock().unwrap().summaries.clone();
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

    fn publish_diagnostics_for_open_files(&self) {
        let uris: Vec<Uri> = self.documents.lock().unwrap().keys().cloned().collect();
        let (cfg, runtime_version) = {
            let c = self.config.lock().unwrap();
            (c.diagnostics.clone(), c.runtime.version.clone())
        };

        for uri in uris {
            let diags = {
                let docs = self.documents.lock().unwrap();
                let Some(doc) = docs.get(&uri) else {
                    continue;
                };
                let mut diags =
                    diagnostics::collect_diagnostics(doc.tree.root_node(), doc.text.as_bytes());
                let mut idx = self.index.lock().unwrap();
                let semantic = diagnostics::collect_semantic_diagnostics_with_version(
                    doc.tree.root_node(),
                    doc.text.as_bytes(),
                    &uri,
                    &mut idx,
                    &doc.scope_tree,
                    &cfg,
                    &runtime_version,
                );
                diags.extend(semantic);
                diagnostics::apply_diagnostic_suppressions(
                    doc.tree.root_node(),
                    doc.text.as_bytes(),
                    diags,
                )
            };

            let client = self.client.clone();
            let uri_clone = uri.clone();
            tokio::spawn(async move {
                client.publish_diagnostics(uri_clone, diags, None).await;
            });
        }
    }
}

/// Collect URIs of open files that depend on `uri` either via
/// `require()` (require_by_return) or via Emmy type references
/// (type_dependants — P1-7). Takes a pre-collected set of open
/// URIs and the set of type names this edit may have touched
/// (union of old-summary and new-summary type definitions — the
/// old names cover rename/delete, the new names cover add/edit).
///
/// De-duplicates across both sources so a file that both requires
/// `uri` AND references one of its classes only appears once.
fn collect_dependant_uris(
    uri: &Uri,
    idx: &aggregation::WorkspaceAggregation,
    open_uris: &std::collections::HashSet<Uri>,
    affected_type_names: &[String],
) -> Vec<Uri> {
    let mut seen: std::collections::HashSet<Uri> = std::collections::HashSet::new();
    let mut result = Vec::new();

    if let Some(deps) = idx.require_by_return.get(uri) {
        for dep in deps {
            if open_uris.contains(&dep.source_uri) && seen.insert(dep.source_uri.clone()) {
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
                if open_uris.contains(dep_uri) && seen.insert(dep_uri.clone()) {
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
        // `scan_workspace_parallel`) is captured in `.vscode/mylua-lsp.log`.
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
        self.client
            .log_message(
                MessageType::INFO,
                "mylua-lsp initialized, scanning workspace...",
            )
            .await;
        self.scan_workspace_parallel().await;
        self.publish_diagnostics_for_open_files();
        self.client
            .log_message(MessageType::INFO, "mylua-lsp workspace scan complete")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let lock = self.edit_lock_for(&uri);
        let _guard = lock.lock().await;

        // Fast path (symmetric to `did_close`): if the indexed document
        // text already matches the incoming buffer byte-for-byte, skip
        // re-parse / re-build-summary / re-publish entirely. Workspace
        // scan inserts every file into `documents` at startup, so the
        // very first user `did_open` of an unmodified file also hits
        // this path — not just reopens after a close.
        //
        // We intentionally compare *text only*, not version: clients
        // reset `version` to 1 on reopen but content is unchanged, and
        // conversely identical version numbers do not guarantee identical
        // content across clients. Byte-equality is the only safe signal.
        //
        // Invariant: fast path does NOT bump `diag_gen`. If a 300ms-
        // debounced semantic diagnostics task from before is still in
        // flight and ends up publishing, the result matches the current
        // `documents[uri]` content (which this check just proved equal
        // to the incoming buffer), so the publish is still correct.
        {
            let docs = self.documents.lock().unwrap();
            if let Some(doc) = docs.get(&uri) {
                if doc.text == params.text_document.text {
                    return;
                }
            }
        }

        self.parse_and_store(
            uri,
            params.text_document.text,
            Some(params.text_document.version),
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = Some(params.text_document.version);

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

        self.parse_and_store_with_old_tree(uri, final_text, version, old_tree);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
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
        // `edit_locks` / `diag_gen` entries are kept — they're reused
        // on the next `did_open` for the same URI.
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
                    self.parse_and_store(uri, text, None);
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
                    self.diag_gen.lock().unwrap().remove(&change.uri);
                    self.edit_locks.lock().unwrap().remove(&change.uri);
                    self.semantic_tokens_cache.lock().unwrap().remove(&change.uri);
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
