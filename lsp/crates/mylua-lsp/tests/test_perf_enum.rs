mod test_helpers;

use test_helpers::*;
use mylua_lsp::scope;
use mylua_lsp::summary_builder;

/// Benchmark test: measure time spent in each phase for a large enum file.
///
/// Run with: cargo test --release test_perf_large_enum_file -- --nocapture
#[test]
fn test_perf_large_enum_file() {
    // Try to read the large UE enum file; skip if not present.
    let path = "/Users/zhuguosen/MyTmp/mylua/test-mylua-many/LuaComment/ue-lua-enum.lua";
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("[SKIP] file not found: {}", path);
            return;
        }
    };

    eprintln!("\n=== Performance breakdown for ue-lua-enum.lua ===");
    eprintln!("File size: {} bytes, {} lines", text.len(), text.lines().count());

    // Phase 1: tree-sitter parse
    let mut parser = new_parser();
    let t0 = std::time::Instant::now();
    let tree = parser.parse(text.as_bytes(), None).expect("parse failed");
    let parse_ms = t0.elapsed().as_millis();
    eprintln!("[Phase 1] tree-sitter parse:   {} ms", parse_ms);

    let root = tree.root_node();
    eprintln!("  root node children: {}", root.child_count());
    eprintln!("  root named children: {}", root.named_child_count());
    eprintln!("  has_error: {}", root.has_error());

    let lua_source = mylua_lsp::util::LuaSource::new(text);

    // Phase 2: build_summary
    let uri = make_uri("ue-lua-enum.lua");
    let t1 = std::time::Instant::now();
    let summary = summary_builder::build_summary(&uri, &tree, lua_source.source(), lua_source.line_index());
    let summary_ms = t1.elapsed().as_millis();
    eprintln!("[Phase 2] build_summary:       {} ms", summary_ms);
    eprintln!("  global_contributions: {}", summary.global_contributions.len());
    eprintln!("  type_definitions: {}", summary.type_definitions.len());
    eprintln!("  table_shapes: {}", summary.table_shapes.len());
    eprintln!("  call_sites: {}", summary.call_sites.len());
    eprintln!("  function_summaries: {}", summary.function_summaries.len());

    // Phase 3: build_scope_tree
    let t2 = std::time::Instant::now();
    let _scope_tree = scope::build_scope_tree(&tree, lua_source.source(), lua_source.line_index());
    let scope_ms = t2.elapsed().as_millis();
    eprintln!("[Phase 3] build_scope_tree:    {} ms", scope_ms);

    let total_ms = parse_ms + summary_ms + scope_ms;
    eprintln!("\n[Total]                        {} ms", total_ms);
    eprintln!("  parse:   {:.1}%", parse_ms as f64 / total_ms as f64 * 100.0);
    eprintln!("  summary: {:.1}%", summary_ms as f64 / total_ms as f64 * 100.0);
    eprintln!("  scope:   {:.1}%", scope_ms as f64 / total_ms as f64 * 100.0);
    eprintln!("=== End performance breakdown ===\n");
}
