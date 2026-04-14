#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use mylua_lsp::aggregation::WorkspaceAggregation;
use mylua_lsp::document::Document;
use mylua_lsp::summary_builder;
use mylua_lsp::workspace_scanner;
use tower_lsp_server::ls_types::{Position, Uri};

/// Project root: `F:\MyGit\ai-mylua-lsp` (or wherever the repo lives).
/// Tests reference Lua fixtures relative to the repo root under `tests/`.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // crates/mylua-lsp -> lsp -> repo root
    manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Absolute path to a test fixture file under `tests/`.
pub fn fixture_path(relative: &str) -> PathBuf {
    repo_root().join("tests").join(relative)
}

/// Read a fixture file's contents.
pub fn read_fixture(relative: &str) -> String {
    let path = fixture_path(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

/// Create a new tree-sitter parser configured for mylua.
pub fn new_parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_mylua::LANGUAGE.into())
        .expect("failed to load mylua grammar");
    parser
}

/// Parse source text into a `Document`.
pub fn parse_doc(parser: &mut tree_sitter::Parser, text: &str) -> Document {
    let tree = parser
        .parse(text.as_bytes(), None)
        .expect("parse returned None");
    Document {
        text: text.to_string(),
        tree,
    }
}

/// Build a fake `file:///` URI from a human-readable name (e.g. "hover1.lua").
pub fn make_uri(name: &str) -> Uri {
    format!("file:///test/{}", name)
        .parse()
        .expect("invalid URI")
}

/// Convenience: `Position { line, character }` (both 0-based).
pub fn pos(line: u32, character: u32) -> Position {
    Position { line, character }
}

/// Set up a single-file workspace: parse the file, build its summary, upsert
/// into the aggregation, and return everything needed to call LSP handlers.
pub fn setup_single_file(
    source: &str,
    filename: &str,
) -> (Document, Uri, WorkspaceAggregation) {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, source);
    let uri = make_uri(filename);
    let mut agg = WorkspaceAggregation::new();
    let summary = summary_builder::build_summary(&uri, &doc.tree, source.as_bytes());
    agg.upsert_summary(summary);
    (doc, uri, agg)
}

/// Set up a multi-file workspace from `(filename, source)` pairs.
/// Returns documents map, aggregation, and the parser (in case you need more parsing).
pub fn setup_workspace(
    files: &[(&str, &str)],
) -> (HashMap<Uri, Document>, WorkspaceAggregation, tree_sitter::Parser) {
    let mut parser = new_parser();
    let mut docs = HashMap::new();
    let mut agg = WorkspaceAggregation::new();

    for (filename, source) in files {
        let uri = make_uri(filename);
        let doc = parse_doc(&mut parser, source);
        let summary = summary_builder::build_summary(&uri, &doc.tree, source.as_bytes());
        agg.upsert_summary(summary);
        docs.insert(uri, doc);
    }

    (docs, agg, parser)
}

/// Set up a workspace by scanning a real directory of Lua fixtures.
/// This mimics what the LSP does on `initialized`.
pub fn setup_workspace_from_dir(
    dir_relative: &str,
) -> (HashMap<Uri, Document>, WorkspaceAggregation, tree_sitter::Parser) {
    let dir = fixture_path(dir_relative);
    let mut parser = new_parser();
    let mut docs = HashMap::new();
    let mut agg = WorkspaceAggregation::new();

    let roots = vec![dir.clone()];
    let require_map = workspace_scanner::scan_workspace_lua_files(&roots);
    for (module, uri) in &require_map {
        agg.set_require_mapping(module.clone(), uri.clone());
    }

    let files = workspace_scanner::collect_lua_files(&roots);
    for file in &files {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let uri = match workspace_scanner::path_to_uri(file) {
            Some(u) => u,
            None => continue,
        };
        let tree = parser.parse(text.as_bytes(), None);
        if let Some(tree) = tree {
            let summary = summary_builder::build_summary(&uri, &tree, text.as_bytes());
            agg.upsert_summary(summary);
            docs.insert(uri, Document { text, tree });
        }
    }

    (docs, agg, parser)
}
