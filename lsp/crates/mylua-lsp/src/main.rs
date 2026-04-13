use std::collections::HashMap;
use std::sync::Mutex;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

struct Document {
    text: String,
    tree: tree_sitter::Tree,
}

struct Backend {
    client: Client,
    parser: Mutex<tree_sitter::Parser>,
    documents: Mutex<HashMap<Uri, Document>>,
}

fn ts_point_to_position(point: tree_sitter::Point) -> Position {
    Position {
        line: point.row as u32,
        character: point.column as u32,
    }
}

fn ts_node_to_range(node: tree_sitter::Node) -> Range {
    Range {
        start: ts_point_to_position(node.start_position()),
        end: ts_point_to_position(node.end_position()),
    }
}

fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("<error>")
}

// ---------------------------------------------------------------------------
// Diagnostics: collect ERROR / MISSING nodes from the parse tree
// ---------------------------------------------------------------------------

fn collect_diagnostics(root: tree_sitter::Node, source: &[u8]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut cursor = root.walk();
    collect_errors_recursive(&mut cursor, source, &mut diagnostics);
    diagnostics
}

fn collect_errors_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let node = cursor.node();
    if node.is_error() {
        diagnostics.push(Diagnostic {
            range: ts_node_to_range(node),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Syntax error near '{}'", truncate(node_text(node, source), 40)),
            ..Default::default()
        });
    } else if node.is_missing() {
        diagnostics.push(Diagnostic {
            range: ts_node_to_range(node),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Missing '{}'", node.kind()),
            ..Default::default()
        });
    }

    if node.has_error() && cursor.goto_first_child() {
        loop {
            collect_errors_recursive(cursor, source, diagnostics);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.replace('\n', "\\n")
    } else {
        format!("{}...", &s[..max].replace('\n', "\\n"))
    }
}

// ---------------------------------------------------------------------------
// Document symbols: top-level functions, locals, assignments
// ---------------------------------------------------------------------------

fn collect_document_symbols(root: tree_sitter::Node, source: &[u8]) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    if !cursor.goto_first_child() {
        return symbols;
    }

    loop {
        let node = cursor.node();
        match node.kind() {
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    #[allow(deprecated)]
                    symbols.push(DocumentSymbol {
                        name,
                        detail: None,
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        range: ts_node_to_range(node),
                        selection_range: ts_node_to_range(name_node),
                        children: None,
                    });
                }
            }
            "local_function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    #[allow(deprecated)]
                    symbols.push(DocumentSymbol {
                        name,
                        detail: Some("local".to_string()),
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        range: ts_node_to_range(node),
                        selection_range: ts_node_to_range(name_node),
                        children: None,
                    });
                }
            }
            "local_declaration" => {
                if let Some(names_node) = node.child_by_field_name("names") {
                    for i in 0..names_node.named_child_count() {
                        if let Some(id_node) = names_node.named_child(i as u32) {
                            if id_node.kind() == "identifier" {
                                let name = node_text(id_node, source).to_string();
                                #[allow(deprecated)]
                                symbols.push(DocumentSymbol {
                                    name,
                                    detail: Some("local".to_string()),
                                    kind: SymbolKind::VARIABLE,
                                    tags: None,
                                    deprecated: None,
                                    range: ts_node_to_range(node),
                                    selection_range: ts_node_to_range(id_node),
                                    children: None,
                                });
                            }
                        }
                    }
                }
            }
            "assignment_statement" => {
                if let Some(left_node) = node.child_by_field_name("left") {
                    if let Some(first_var) = left_node.named_child(0) {
                        let name = node_text(first_var, source).to_string();
                        #[allow(deprecated)]
                        symbols.push(DocumentSymbol {
                            name,
                            detail: None,
                            kind: SymbolKind::VARIABLE,
                            tags: None,
                            deprecated: None,
                            range: ts_node_to_range(node),
                            selection_range: ts_node_to_range(first_var),
                            children: None,
                        });
                    }
                }
            }
            _ => {}
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }

    symbols
}

// ---------------------------------------------------------------------------
// Semantic tokens legend (stub — no tokens produced yet)
// ---------------------------------------------------------------------------

fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![
            SemanticTokenType::FUNCTION,
            SemanticTokenType::VARIABLE,
            SemanticTokenType::PARAMETER,
            SemanticTokenType::KEYWORD,
            SemanticTokenType::STRING,
            SemanticTokenType::NUMBER,
            SemanticTokenType::COMMENT,
            SemanticTokenType::OPERATOR,
        ],
        token_modifiers: vec![
            SemanticTokenModifier::DECLARATION,
            SemanticTokenModifier::DEFINITION,
            SemanticTokenModifier::READONLY,
        ],
    }
}

// ---------------------------------------------------------------------------
// Backend implementation
// ---------------------------------------------------------------------------

impl Backend {
    fn parse_and_store(&self, uri: Uri, text: String, version: Option<i32>) {
        let tree = {
            let mut parser = self.parser.lock().unwrap();
            parser.parse(text.as_bytes(), None)
        };

        if let Some(tree) = tree {
            let diagnostics = collect_diagnostics(tree.root_node(), text.as_bytes());

            self.documents.lock().unwrap().insert(
                uri.clone(),
                Document {
                    text,
                    tree,
                },
            );

            let client = self.client.clone();
            tokio::spawn(async move {
                client.publish_diagnostics(uri, diagnostics, version).await;
            });
        }
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
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
        self.client
            .log_message(MessageType::INFO, "mylua-lsp initialized")
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

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let docs = self.documents.lock().unwrap();
        let Some(doc) = docs.get(&params.text_document.uri) else {
            return Ok(None);
        };
        let symbols = collect_document_symbols(doc.tree.root_node(), doc.text.as_bytes());
        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn semantic_tokens_full(
        &self,
        _params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: vec![],
        })))
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_mylua::LANGUAGE.into())
            .expect("failed to load mylua grammar");
        Backend {
            client,
            parser: Mutex::new(parser),
            documents: Mutex::new(HashMap::new()),
        }
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
