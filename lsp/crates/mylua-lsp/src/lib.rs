#[macro_use]
pub mod logger;
pub mod aggregation;
pub mod completion;
pub mod config;
pub mod diagnostics;
pub mod document;
pub mod emmy;
pub mod goto;
pub mod hover;
pub mod references;
pub mod rename;
pub mod resolver;
pub mod scope;
pub mod semantic_tokens;
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
        }
    }

    fn parse_and_store(&self, uri: Uri, text: String, version: Option<i32>) {
        let tree = {
            let mut parser = self.parser.lock().unwrap();
            parser.parse(text.as_bytes(), None)
        };

        if let Some(tree) = tree {
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
                idx.upsert_summary(summary);
                if old_fp.map_or(false, |old| old != new_fp) {
                    collect_dependant_uris(&uri, &idx, &open_uris)
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
                let semantic = diagnostics::collect_semantic_diagnostics(
                    doc.tree.root_node(),
                    doc.text.as_bytes(),
                    &uri,
                    &mut idx,
                    &doc.scope_tree,
                    &cfg.diagnostics,
                );
                syntax.extend(semantic);
                syntax
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
            eprintln!(
                "[mylua-lsp] WARNING: indexMode 'isolated' is not yet implemented; \
                 falling back to 'merged' for {} workspace roots",
                roots.len()
            );
        }

        let use_disk_cache = cache_mode == config::CacheMode::Summary;
        let cache = if use_disk_cache {
            roots
                .first()
                .and_then(|r| summary_cache::SummaryCache::new(r, config_fingerprint))
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
        eprintln!("[mylua-lsp] indexing {} .lua files (parallel)...", total);

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
                eprintln!("[mylua-lsp] indexing batch failed: {}", e);
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
            eprintln!("[mylua-lsp] indexed {}/{}", indexed, total);
        }

        let hits = cache_hits.load(std::sync::atomic::Ordering::Relaxed);
        if hits > 0 {
            eprintln!("[mylua-lsp] cache hits: {}/{}", hits, total);
        }

        *self.index_state.lock().unwrap() = IndexState::Ready;
        progress.finish().await;
        eprintln!(
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
                    eprintln!("[mylua-lsp] saved {} summaries to cache", summaries.len());
                }
            });
        }
    }

    fn publish_diagnostics_for_open_files(&self) {
        let uris: Vec<Uri> = self.documents.lock().unwrap().keys().cloned().collect();
        let cfg = self.config.lock().unwrap().diagnostics.clone();

        for uri in uris {
            let diags = {
                let docs = self.documents.lock().unwrap();
                let Some(doc) = docs.get(&uri) else {
                    continue;
                };
                let mut diags =
                    diagnostics::collect_diagnostics(doc.tree.root_node(), doc.text.as_bytes());
                let mut idx = self.index.lock().unwrap();
                let semantic = diagnostics::collect_semantic_diagnostics(
                    doc.tree.root_node(),
                    doc.text.as_bytes(),
                    &uri,
                    &mut idx,
                    &doc.scope_tree,
                    &cfg,
                );
                diags.extend(semantic);
                diags
            };

            let client = self.client.clone();
            let uri_clone = uri.clone();
            tokio::spawn(async move {
                client.publish_diagnostics(uri_clone, diags, None).await;
            });
        }
    }
}

/// Collect URIs of open files that depend on the given URI via require.
/// Takes a pre-collected set of open URIs to avoid locking documents
/// while the index lock is held.
fn collect_dependant_uris(
    uri: &Uri,
    idx: &aggregation::WorkspaceAggregation,
    open_uris: &std::collections::HashSet<Uri>,
) -> Vec<Uri> {
    let mut result = Vec::new();
    if let Some(deps) = idx.require_by_return.get(uri) {
        for dep in deps {
            if open_uris.contains(&dep.source_uri) {
                result.push(dep.source_uri.clone());
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

fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let hi = chars.next().and_then(|c| hex_val(c));
            let lo = chars.next().and_then(|c| hex_val(c));
            if let (Some(h), Some(l)) = (hi, lo) {
                result.push((h << 4 | l) as char);
            }
        } else {
            result.push(b as char);
        }
    }
    result
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        if let Some(opts) = params.initialization_options {
            let cfg = LspConfig::from_value(opts);
            eprintln!("[mylua-lsp] config: {:?}", cfg);
            *self.config.lock().unwrap() = cfg;
        }

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
        *self.workspace_roots.lock().unwrap() = roots;

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions::default()),
                references_provider: Some(OneOf::Left(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: None,
                    },
                })),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: semantic_tokens_legend(),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
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
        {
            let roots = self.workspace_roots.lock().unwrap();
            let cfg = self.config.lock().unwrap();
            if let Some(root) = roots.first() {
                logger::init(root, cfg.debug.file_log);
            }
        }
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
        self.parse_and_store(
            params.text_document.uri,
            params.text_document.text,
            Some(params.text_document.version),
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.parse_and_store(
                params.text_document.uri,
                change.text,
                Some(params.text_document.version),
            );
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.diag_gen
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
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
                }
                _ => {}
            }
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let cfg = LspConfig::from_value(params.settings);
        eprintln!("[mylua-lsp] config updated: {:?}", cfg);
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
        let syms = symbols::collect_document_symbols(doc.tree.root_node(), doc.text.as_bytes());
        Ok(Some(DocumentSymbolResponse::Nested(syms)))
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
        Ok(rename::rename(
            doc,
            uri,
            position,
            &params.new_name,
            &idx,
            &docs,
        ))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let idx = self.index.lock().unwrap();
        let results = workspace_symbol::search_workspace_symbols(&params.query, &idx);
        Ok(Some(WorkspaceSymbolResponse::Flat(results)))
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        let data = semantic_tokens::collect_semantic_tokens(
            doc.tree.root_node(),
            doc.text.as_bytes(),
            &doc.scope_tree,
        );
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
}
