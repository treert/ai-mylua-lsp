#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;

use mylua_lsp::aggregation::WorkspaceAggregation;
use mylua_lsp::config::{RequireConfig, WorkspaceConfig};
use mylua_lsp::document::Document;
use mylua_lsp::summary::DocumentSummary;
use mylua_lsp::summary_builder;
pub use mylua_lsp::uri_id::{intern, UriId};
use mylua_lsp::util::LuaSource;
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
    let lua_source = LuaSource::new(text.to_string());
    let uri: Uri = "file:///test/parse_doc.lua".parse().unwrap();
    let (_, scope_tree) = summary_builder::build_file_analysis(
        &uri,
        &tree,
        lua_source.source(),
        lua_source.line_index(),
    );
    Document {
        lua_source,
        tree,
        scope_tree,
    }
}

/// Build a fake `file:///` URI from a human-readable name (e.g. "hover1.lua").
pub fn make_uri(name: &str) -> Uri {
    format!("file:///test/{}", name)
        .parse()
        .expect("invalid URI")
}

pub fn summary_by_uri<'a>(
    agg: &'a WorkspaceAggregation,
    uri: &Uri,
) -> Option<&'a DocumentSummary> {
    agg.summary_by_id(intern(uri.clone()))
}

pub fn summary_id_by_uri(agg: &WorkspaceAggregation, uri: &Uri) -> UriId {
    let uri_id = intern(uri.clone());
    assert!(
        agg.summary_by_id(uri_id).is_some(),
        "summary for URI {:?} should be indexed",
        uri
    );
    uri_id
}

/// Convenience: `Position { line, character }` (both 0-based).
pub fn pos(line: u32, character: u32) -> Position {
    Position { line, character }
}

/// Build an empty documents map for tests.
/// The `br_to_range` fallback uses row/col directly, which is correct
/// for ASCII-only test fixtures (byte col == UTF-16 col).
pub fn empty_docs() -> HashMap<UriId, Document> {
    HashMap::new()
}

/// Set up a single-file workspace: parse the file, build its summary, upsert
/// into the aggregation, and return everything needed to call LSP handlers.
pub fn setup_single_file(source: &str, filename: &str) -> (Document, Uri, WorkspaceAggregation) {
    let mut parser = new_parser();
    let doc = parse_doc(&mut parser, source);
    let uri = make_uri(filename);
    let uri_id = intern(uri.clone());
    let mut agg = WorkspaceAggregation::new();
    let summary =
        summary_builder::build_file_analysis(&uri, &doc.tree, doc.source(), doc.line_index()).0;
    // Register module mapping so resolve_module_to_id works.
    if let Some(module_name) = workspace_scanner::uri_to_module_name(&uri) {
        agg.set_require_mapping(module_name, uri_id);
    }
    agg.upsert_summary(uri_id, summary);
    (doc, uri, agg)
}

/// Set up a multi-file workspace from `(filename, source)` pairs.
/// Returns documents map, aggregation, and the parser (in case you need more parsing).
pub fn setup_workspace(
    files: &[(&str, &str)],
) -> (
    HashMap<UriId, Document>,
    WorkspaceAggregation,
    tree_sitter::Parser,
) {
    let mut parser = new_parser();
    let mut docs = HashMap::new();
    let mut agg = WorkspaceAggregation::new();

    for (filename, source) in files {
        let uri = make_uri(filename);
        let uri_id = intern(uri.clone());
        let doc = parse_doc(&mut parser, source);
        let summary =
            summary_builder::build_file_analysis(&uri, &doc.tree, doc.source(), doc.line_index()).0;
        // Register module mapping so resolve_module_to_id works.
        if let Some(module_name) = workspace_scanner::uri_to_module_name(&uri) {
            agg.set_require_mapping(module_name, uri_id);
        }
        agg.upsert_summary(uri_id, summary);
        docs.insert(uri_id, doc);
    }

    (docs, agg, parser)
}

/// Set up a workspace by scanning a real directory of Lua fixtures.
/// This mimics what the LSP does on `initialized`.
pub fn setup_workspace_from_dir(
    dir_relative: &str,
) -> (
    HashMap<UriId, Document>,
    WorkspaceAggregation,
    tree_sitter::Parser,
) {
    let dir = fixture_path(dir_relative);
    let mut parser = new_parser();
    let mut docs = HashMap::new();
    let mut agg = WorkspaceAggregation::new();

    let roots = vec![dir.clone()];
    let module_entries = workspace_scanner::scan_workspace_lua_files(
        &roots,
        &RequireConfig::default(),
        &WorkspaceConfig::default(),
    );
    for (module, uri) in &module_entries {
        let uri_id = intern(uri.clone());
        agg.set_require_mapping(module.clone(), uri_id);
    }

    let files = workspace_scanner::collect_lua_files(&roots, &WorkspaceConfig::default());
    for file in &files {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let uri = match workspace_scanner::path_to_uri(file) {
            Some(u) => u,
            None => continue,
        };
        let uri_id = intern(uri.clone());
        let tree = parser.parse(text.as_bytes(), None);
        if let Some(tree) = tree {
            let lua_source = LuaSource::new(text);
            let (summary, scope_tree) = summary_builder::build_file_analysis(
                &uri,
                &tree,
                lua_source.source(),
                lua_source.line_index(),
            );
            agg.upsert_summary(uri_id, summary);
            docs.insert(
                uri_id,
                Document {
                    lua_source,
                    tree,
                    scope_tree,
                },
            );
        }
    }

    (docs, agg, parser)
}

/// Set up a workspace with one or more library roots scanned alongside
/// workspace files. Mimics the production `run_workspace_scan` path:
/// library file URIs are force-flagged `is_meta=true` on their
/// summaries, and caller receives the resolved library URI set for
/// assertions. `workspace_files` may be empty (pure library scenario).
pub fn setup_workspace_with_library(
    workspace_files: &[(&str, &str)],
    library_roots_absolute: &[PathBuf],
) -> (
    HashMap<UriId, Document>,
    WorkspaceAggregation,
    tree_sitter::Parser,
    std::collections::HashSet<Uri>,
) {
    use std::collections::HashSet;
    let mut parser = new_parser();
    let mut docs = HashMap::new();
    let mut agg = WorkspaceAggregation::new();

    for (filename, source) in workspace_files {
        let uri = make_uri(filename);
        let uri_id = intern(uri.clone());
        let doc = parse_doc(&mut parser, source);
        let summary =
            summary_builder::build_file_analysis(&uri, &doc.tree, doc.source(), doc.line_index()).0;
        // Register module mapping so resolve_module_to_id works.
        if let Some(module_name) = workspace_scanner::uri_to_module_name(&uri) {
            agg.set_require_mapping(module_name, uri_id);
        }
        agg.upsert_summary(uri_id, summary);
        docs.insert(uri_id, doc);
    }

    let ws_config = WorkspaceConfig::default();
    let require_config = RequireConfig::default();

    // Library files — enumerate for URI set, then build summaries.
    // `require_map` also gets library entries so `require("string")`
    // works from workspace files.
    let library_files = workspace_scanner::collect_lua_files(library_roots_absolute, &ws_config);
    let library_uris: HashSet<Uri> = library_files
        .iter()
        .filter_map(|p| workspace_scanner::path_to_uri(p))
        .collect();

    let module_entries = workspace_scanner::scan_workspace_lua_files(
        library_roots_absolute,
        &require_config,
        &ws_config,
    );
    for (module, uri) in &module_entries {
        let uri_id = intern(uri.clone());
        agg.set_require_mapping(module.clone(), uri_id);
    }

    for file in &library_files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Some(uri) = workspace_scanner::path_to_uri(file) else {
            continue;
        };
        let uri_id = intern(uri.clone());
        let Some(tree) = parser.parse(text.as_bytes(), None) else {
            continue;
        };
        let lua_source = LuaSource::new(text);
        let (mut summary, scope_tree) = summary_builder::build_file_analysis(
            &uri,
            &tree,
            lua_source.source(),
            lua_source.line_index(),
        );
        // Production `run_workspace_scan` does this override for any
        // URI originating from a library root; tests mirror the same
        // contract.
        summary.is_meta = true;
        agg.upsert_summary(uri_id, summary);
        docs.insert(
            uri_id,
            Document {
                lua_source,
                tree,
                scope_tree,
            },
        );
    }

    (docs, agg, parser, library_uris)
}

/// Absolute path to the bundled Lua 5.4 stdlib stubs inside the VS Code
/// extension asset tree. Used by library-related tests to avoid
/// hard-coding per-machine paths.
pub fn bundled_lua54_library_path() -> PathBuf {
    repo_root()
        .join("vscode-extension")
        .join("assets")
        .join("lua5.4")
}
