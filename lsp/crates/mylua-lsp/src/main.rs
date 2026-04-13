use std::sync::Mutex;

use tower_lsp_server::jsonrpc::Result;
use tower_lsp_server::ls_types::*;
use tower_lsp_server::{Client, LanguageServer, LspService, Server};

struct Backend {
    client: Client,
    parser: Mutex<tree_sitter::Parser>,
}

impl Backend {
    fn parse_document(&self, uri: &Uri, text: &str) {
        let mut parser = self.parser.lock().unwrap();
        match parser.parse(text.as_bytes(), None) {
            Some(tree) => {
                let root = tree.root_node();
                eprintln!(
                    "[mylua-lsp] parsed {:?}: {} nodes, has_error={}",
                    uri,
                    root.descendant_count(),
                    root.has_error()
                );
            }
            None => {
                eprintln!("[mylua-lsp] parse failed for {:?}", uri);
            }
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
        self.parse_document(&params.text_document.uri, &params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.parse_document(&params.text_document.uri, &change.text);
        }
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
        }
    });
    Server::new(stdin, stdout, socket).serve(service).await;
}
