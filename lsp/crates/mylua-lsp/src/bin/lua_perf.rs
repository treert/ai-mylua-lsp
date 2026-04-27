//! Standalone CLI tool for profiling Lua file parse performance.
//!
//! Usage:
//!   cargo run --release --bin lua-perf -- <file1.lua> [file2.lua ...]
//!
//! For each file, it measures:
//!   - Phase 1: tree-sitter parse
//!   - Phase 2: build_file_analysis (summary + scope tree)
//! and prints a detailed breakdown with timing and percentages.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        eprintln!("Usage: lua-perf <file1.lua> [file2.lua ...]");
        eprintln!();
        eprintln!("Profile the parse/analysis performance of Lua files.");
        eprintln!("Use --release for meaningful timings:");
        eprintln!("  cargo run --release --bin lua-perf -- path/to/file.lua");
        std::process::exit(1);
    }

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_mylua::LANGUAGE.into())
        .expect("failed to load mylua grammar");

    for path in &args {
        run_perf_breakdown(&mut parser, path);
    }
}

fn run_perf_breakdown(parser: &mut tree_sitter::Parser, path: &str) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[ERROR] cannot read '{}': {}", path, e);
            return;
        }
    };

    let filename = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());

    eprintln!();
    eprintln!("=== Performance breakdown for {} ===", filename);
    eprintln!("  Path: {}", path);
    eprintln!("  File size: {} bytes, {} lines", text.len(), text.lines().count());
    eprintln!();

    // Phase 1: tree-sitter parse
    let t0 = Instant::now();
    let tree = parser.parse(text.as_bytes(), None).expect("parse failed");
    let parse_ms = t0.elapsed().as_millis();
    eprintln!("[Phase 1] tree-sitter parse:      {} ms", parse_ms);

    let root = tree.root_node();
    eprintln!("  root node children: {}", root.child_count());
    eprintln!("  root named children: {}", root.named_child_count());
    eprintln!("  has_error: {}", root.has_error());

    let lua_source = mylua_lsp::util::LuaSource::new(text);

    // Phase 2: build_file_analysis (summary + scope tree in one pass)
    let uri: tower_lsp_server::ls_types::Uri = format!("file:///perf/{}", filename)
        .parse()
        .expect("invalid URI");
    let t1 = Instant::now();
    let (summary, _scope_tree) = mylua_lsp::summary_builder::build_file_analysis(
        &uri,
        &tree,
        lua_source.source(),
        lua_source.line_index(),
    );
    let analysis_ms = t1.elapsed().as_millis();
    eprintln!("[Phase 2] build_file_analysis:    {} ms", analysis_ms);
    eprintln!("  global_contributions: {}", summary.global_contributions.len());
    eprintln!("  type_definitions: {}", summary.type_definitions.len());
    eprintln!("  table_shapes: {}", summary.table_shapes.len());
    eprintln!("  call_sites: {}", summary.call_sites.len());
    eprintln!("  function_summaries: {}", summary.function_summaries.len());

    let total_ms = parse_ms + analysis_ms;
    eprintln!();
    eprintln!("[Total]                           {} ms", total_ms);
    if total_ms > 0 {
        eprintln!(
            "  parse:    {:.1}%",
            parse_ms as f64 / total_ms as f64 * 100.0
        );
        eprintln!(
            "  analysis: {:.1}%",
            analysis_ms as f64 / total_ms as f64 * 100.0
        );
    }
    eprintln!("=== End ===");
    eprintln!();
}
