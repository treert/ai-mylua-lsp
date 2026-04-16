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
pub mod workspace_scanner;
pub mod workspace_symbol;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer};

use aggregation::WorkspaceAggregation;
use config::LspConfig;
use document::Document;

pub struct Backend {
    client: Client,
    parser: Mutex<tree_sitter::Parser>,
    documents: Mutex<HashMap<Uri, Document>>,
    index: Mutex<WorkspaceAggregation>,
    workspace_roots: Mutex<Vec<PathBuf>>,
    config: Mutex<LspConfig>,
}

fn semantic_tokens_legend() -> SemanticTokensLegend {
    semantic_tokens::semantic_tokens_legend()
}

impl Backend {
    pub fn new(client: Client) -> Self {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_mylua::LANGUAGE.into())
            .expect("failed to load mylua grammar");
        Backend {
            client,
            parser: Mutex::new(parser),
            documents: Mutex::new(HashMap::new()),
            index: Mutex::new(WorkspaceAggregation::new()),
            workspace_roots: Mutex::new(Vec::new()),
            config: Mutex::new(LspConfig::default()),
        }
    }

    fn parse_and_store(&self, uri: Uri, text: String, version: Option<i32>) {
        let tree = {
            let mut parser = self.parser.lock().unwrap();
            parser.parse(text.as_bytes(), None)
        };

        if let Some(tree) = tree {
            let mut diags = diagnostics::collect_diagnostics(tree.root_node(), text.as_bytes());

            let summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
            lsp_log!("[index] summary for {:?}: locals={:?} types={:?} globals={}",
                uri,
                summary.local_type_facts.keys().collect::<Vec<_>>(),
                summary.type_definitions.iter().map(|t| &t.name).collect::<Vec<_>>(),
                summary.global_contributions.len(),
            );
            self.index.lock().unwrap().upsert_summary(summary);

            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());

            {
                let mut idx = self.index.lock().unwrap();
                let semantic = diagnostics::collect_semantic_diagnostics(
                    tree.root_node(),
                    text.as_bytes(),
                    &uri,
                    &mut idx,
                    &scope_tree,
                );
                diags.extend(semantic);
            }

            self.documents.lock().unwrap().insert(
                uri.clone(),
                Document { text, tree, scope_tree },
            );

            let client = self.client.clone();
            tokio::spawn(async move {
                client.publish_diagnostics(uri, diags, version).await;
            });
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
            let summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
            self.index.lock().unwrap().upsert_summary(summary);
            let scope_tree = scope::build_scope_tree(&tree, text.as_bytes());
            self.documents.lock().unwrap().insert(uri, Document { text, tree, scope_tree });
        }
    }

    fn scan_workspace(&self) {
        let roots = self.workspace_roots.lock().unwrap().clone();

        let require_map = workspace_scanner::scan_workspace_lua_files(&roots);
        {
            let mut idx = self.index.lock().unwrap();
            for (module, uri) in &require_map {
                idx.set_require_mapping(module.clone(), uri.clone());
            }
        }

        let files = workspace_scanner::collect_lua_files(&roots);
        let total = files.len();
        eprintln!("[mylua-lsp] indexing {} .lua files...", total);

        for (i, file) in files.iter().enumerate() {
            self.index_file_from_disk(file);
            if (i + 1) % 500 == 0 || i + 1 == total {
                eprintln!("[mylua-lsp] indexed {}/{}", i + 1, total);
            }
        }

        eprintln!("[mylua-lsp] workspace indexing complete: {} files", total);
    }
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
                    work_done_progress_options: WorkDoneProgressOptions { work_done_progress: None },
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
            .log_message(MessageType::INFO, "mylua-lsp initialized, scanning workspace...")
            .await;
        self.scan_workspace();
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
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            match change.typ {
                FileChangeType::CREATED | FileChangeType::CHANGED => {
                    if let Some(path) = uri_to_path(&change.uri) {
                        if path.extension().map_or(false, |e| e == "lua") {
                            self.index_file_from_disk(&path);
                            let roots = self.workspace_roots.lock().unwrap().clone();
                            for root in &roots {
                                if let Some(module) =
                                    path.strip_prefix(root).ok().and_then(|rel| {
                                        let stem = rel.with_extension("");
                                        Some(stem.to_string_lossy().replace('\\', ".").replace('/', "."))
                                    })
                                {
                                    self.index.lock().unwrap().set_require_mapping(
                                        module,
                                        change.uri.clone(),
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }
                FileChangeType::DELETED => {
                    self.index.lock().unwrap().remove_file(&change.uri);
                    self.documents.lock().unwrap().remove(&change.uri);
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
        Ok(goto::goto_definition(doc, uri, position, &mut idx))
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
        Ok(references::find_references(
            doc,
            uri,
            position,
            include_declaration,
            &idx,
            &docs,
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
        Ok(rename::rename(doc, uri, position, &params.new_name, &idx, &docs))
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
