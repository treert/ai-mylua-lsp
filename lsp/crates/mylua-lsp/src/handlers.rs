//! LSP protocol handler implementations for [`Backend`].
//!
//! This module contains the [`LanguageServer`] trait implementation,
//! mapping each LSP request/notification to the corresponding feature
//! module (goto, hover, completion, diagnostics, etc.).

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::LanguageServer;

use crate::call_hierarchy;
use crate::completion;
use crate::config::LspConfig;
use crate::diagnostic_scheduler;
use crate::document_highlight;
use crate::document_link;
use crate::folding_range;
use crate::goto;
use crate::hover;
use crate::inlay_hint;
use crate::logger;
use crate::references;
use crate::rename;
use crate::selection_range;
use crate::semantic_tokens;
use crate::signature_help;
use crate::symbols;
use crate::workspace_scanner;
use crate::workspace_symbol;
use crate::{
    indexing, semantic_tokens_legend, start_diagnostic_consumer, uri_to_path, Backend,
    POSITION_ENCODING,
};
use std::sync::atomic::Ordering;

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
            // Apply top_keyword setting to the scanner's global default
            // BEFORE any parser is created. When `top_keyword = true`,
            // column-0 keywords emit TOP_WORD_* for error front-loading;
            // when `false` (default), they emit normal WORD_*.
            tree_sitter_mylua::set_top_keyword_default_disabled(!cfg.runtime.top_keyword);
            *self.config.lock().unwrap() = cfg;
        }

        *self.workspace_roots.lock().unwrap() = roots;

        // ---------------------------------------------------------------
        // Position encoding negotiation (LSP 3.17)
        // ---------------------------------------------------------------
        // Prefer UTF-8 if the client advertises support; otherwise
        // fall back to UTF-16 (the mandatory LSP default).
        let negotiated_encoding = params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_ref())
            .and_then(|encs| {
                if encs.iter().any(|e| *e == PositionEncodingKind::UTF8) {
                    Some(PositionEncodingKind::UTF8)
                } else {
                    None
                }
            })
            .unwrap_or(PositionEncodingKind::UTF16);

        let is_utf8 = negotiated_encoding == PositionEncodingKind::UTF8;
        POSITION_ENCODING.store(if is_utf8 { 1 } else { 0 }, Ordering::Relaxed);
        lsp_log!(
            "[mylua-lsp] position encoding: {}",
            negotiated_encoding.as_str()
        );

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
                position_encoding: Some(negotiated_encoding),
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
            indexing::run_workspace_scan(
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
                .is_some_and(|d| d.text() == params.text_document.text)
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
                text = doc.lua_source.into_text();
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
                        let edit = crate::util::apply_text_edit(&mut text, range, &change.text);
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
                        docs.get(&uri).is_some_and(|d| d.text() == text)
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
        let workspace_config = {
            let cfg = self.config.lock().unwrap();
            cfg.workspace.clone()
        };
        let filter = workspace_scanner::FileFilter::from_config(&workspace_config);

        for change in params.changes {
            match change.typ {
                FileChangeType::CREATED | FileChangeType::CHANGED => {
                    if let Some(path) = uri_to_path(&change.uri) {
                        if path.extension().is_some_and(|e| e == "lua") {
                            if !workspace_scanner::should_index_path(&path, &roots, &filter) {
                                continue;
                            }
                            self.index_file_from_disk(&path);
                            if let Some(module_name) = workspace_scanner::file_path_to_module_name(&path) {
                                let mut idx = self.index.lock().unwrap();
                                idx.set_require_mapping(
                                    module_name,
                                    change.uri.clone(),
                                );
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
        // Update the scanner global so newly created parsers pick up
        // the changed setting. Already-parsed documents keep their
        // existing scanner state; a full re-index (restart) is needed
        // for the change to take effect on all files.
        tree_sitter_mylua::set_top_keyword_default_disabled(!cfg.runtime.top_keyword);
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
            doc.source(),
            summary,
            doc.line_index(),
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
            doc.source(),
            &idx,
            doc.line_index(),
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
        let items = call_hierarchy::prepare_call_hierarchy(doc, &uri, position, &idx, &docs);
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
        let docs = self.documents.lock().unwrap();
        let idx = self.index.lock().unwrap();
        let calls = call_hierarchy::incoming_calls(&params.item, &idx, &docs);
        Ok(Some(calls))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let docs = self.documents.lock().unwrap();
        let idx = self.index.lock().unwrap();
        let calls = call_hierarchy::outgoing_calls(&params.item, &idx, &docs);
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
        let mut idx = self.index.lock().unwrap();
        let cfg = self.config.lock().unwrap().inlay_hint.clone();
        Ok(Some(inlay_hint::inlay_hints(doc, uri, params.range, &mut idx, &cfg)))
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
        let result = goto::goto_definition(doc, uri, position, &mut idx, &strategy, &docs);
        match &result {
            Some(GotoDefinitionResponse::Scalar(loc)) => {
                lsp_log!("[goto] result: {:?} {}:{}-{}:{}", loc.uri, loc.range.start.line, loc.range.start.character, loc.range.end.line, loc.range.end.character);
            }
            Some(GotoDefinitionResponse::Array(locs)) => {
                for (i, loc) in locs.iter().enumerate() {
                    lsp_log!("[goto] result[{}]: {:?} {}:{}-{}:{}", i, loc.uri, loc.range.start.line, loc.range.start.character, loc.range.end.line, loc.range.end.character);
                }
            }
            Some(GotoDefinitionResponse::Link(_)) => {
                lsp_log!("[goto] result: Link");
            }
            None => {
                lsp_log!("[goto] result: None");
            }
        }
        Ok(result)
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
        Ok(goto::goto_type_definition(doc, uri, position, &mut idx, &strategy, &docs))
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
        Ok(goto::goto_definition(doc, uri, position, &mut idx, &strategy, &docs))
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
        let docs = self.documents.lock().unwrap();
        Ok(completion::resolve_completion(item, &idx, &docs))
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
        let docs = self.documents.lock().unwrap();
        let results = workspace_symbol::search_workspace_symbols(&params.query, &idx, &docs);
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
            doc.source(),
            &doc.scope_tree,
            &runtime_version,
            doc.line_index(),
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
            doc.source(),
            &doc.scope_tree,
            &runtime_version,
            doc.line_index(),
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
            doc.source(),
            &doc.scope_tree,
            params.range,
            &runtime_version,
            doc.line_index(),
        );
        Ok(Some(SemanticTokensRangeResult::Tokens(SemanticTokens {
            result_id: None,
            data,
        })))
    }
}
