//! Standalone CLI tool for profiling Lua file parse performance.
//!
//! Usage:
//!   cargo run --release --bin lua-perf -- [--summary] [--summary-out <dir>] [--summary-stdout] <file1.lua> [file2.lua ...]
//!
//! For each file, it measures:
//!   - Phase 1: tree-sitter parse
//!   - Phase 2: build_file_analysis (summary + scope tree)
//! and prints a detailed breakdown with timing and percentages. With
//! `--summary`, it also writes the DocumentSummary JSON for each input file.

use std::path::{Path, PathBuf};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_usage();
        return;
    }

    let options = match parse_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("[ERROR] {}", message);
            print_usage();
            std::process::exit(1);
        }
    };

    if options.files.is_empty() {
        print_usage();
        std::process::exit(1);
    }
    if matches!(options.summary_output, SummaryOutput::Stdout) && options.files.len() > 1 {
        eprintln!("[ERROR] --summary-stdout supports exactly one input file");
        std::process::exit(1);
    }

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_mylua::LANGUAGE.into())
        .expect("failed to load mylua grammar");

    for path in &options.files {
        run_perf_breakdown(&mut parser, path, &options.summary_output);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliOptions {
    files: Vec<String>,
    summary_output: SummaryOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SummaryOutput {
    None,
    Directory(PathBuf),
    Stdout,
}

fn parse_args(args: Vec<String>) -> Result<CliOptions, String> {
    let mut files = Vec::new();
    let mut summary_output = SummaryOutput::None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--summary" => {
                ensure_summary_output_unset(&summary_output, "--summary")?;
                summary_output = SummaryOutput::Directory(PathBuf::from("target/lua-summary"));
                i += 1;
            }
            "--summary-out" => {
                ensure_summary_output_unset(&summary_output, "--summary-out")?;
                let dir = args
                    .get(i + 1)
                    .ok_or_else(|| "--summary-out requires a directory".to_string())?;
                summary_output = SummaryOutput::Directory(PathBuf::from(dir));
                i += 2;
            }
            "--summary-stdout" => {
                ensure_summary_output_unset(&summary_output, "--summary-stdout")?;
                summary_output = SummaryOutput::Stdout;
                i += 1;
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown option '{}'", arg));
            }
            file => {
                files.push(file.to_string());
                i += 1;
            }
        }
    }

    Ok(CliOptions {
        files,
        summary_output,
    })
}

fn ensure_summary_output_unset(current: &SummaryOutput, option: &str) -> Result<(), String> {
    if matches!(current, SummaryOutput::None) {
        Ok(())
    } else {
        Err(format!(
            "{} cannot be combined with another summary output option",
            option
        ))
    }
}

fn print_usage() {
    eprintln!("Usage: lua-perf [--summary | --summary-out <dir> | --summary-stdout] <file1.lua> [file2.lua ...]");
    eprintln!();
    eprintln!("Profile the parse/analysis performance of Lua files.");
    eprintln!("Use --release for meaningful timings:");
    eprintln!("  cargo run --release --bin lua-perf -- path/to/file.lua");
    eprintln!("  cargo run --release --bin lua-perf -- --summary path/to/file.lua");
    eprintln!(
        "  cargo run --release --bin lua-perf -- --summary-out target/lua-summary path/to/file.lua"
    );
}

fn run_perf_breakdown(
    parser: &mut tree_sitter::Parser,
    path: &str,
    summary_output: &SummaryOutput,
) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[ERROR] cannot read '{}': {}", path, e);
            return;
        }
    };

    let input_path = Path::new(path);
    let filename = input_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());

    eprintln!();
    eprintln!("=== Performance breakdown for {} ===", filename);
    eprintln!("  Path: {}", path);
    eprintln!(
        "  File size: {} bytes, {} lines",
        text.len(),
        text.lines().count()
    );
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
    let uri = match file_uri_for_path(input_path) {
        Some(uri) => uri,
        None => {
            eprintln!("[ERROR] cannot convert '{}' to file URI", path);
            return;
        }
    };
    let t1 = Instant::now();
    let (summary, _scope_tree) = mylua_lsp::summary_builder::build_file_analysis(
        &uri,
        &tree,
        lua_source.source(),
        lua_source.line_index(),
    );
    let analysis_ms = t1.elapsed().as_millis();
    eprintln!("[Phase 2] build_file_analysis:    {} ms", analysis_ms);
    eprintln!(
        "  global_contributions: {}",
        summary.global_contributions.len()
    );
    eprintln!("  type_definitions: {}", summary.type_definitions.len());
    eprintln!("  table_shapes: {}", summary.table_shapes.len());
    eprintln!("  call_sites: {}", summary.call_sites.len());
    eprintln!("  function_summaries: {}", summary.function_summaries.len());
    write_summary_if_requested(&summary, Path::new(path), summary_output);

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

fn write_summary_if_requested(
    summary: &mylua_lsp::summary::DocumentSummary,
    input_path: &Path,
    summary_output: &SummaryOutput,
) {
    match summary_output {
        SummaryOutput::None => {}
        SummaryOutput::Directory(dir) => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!(
                    "[ERROR] cannot create summary output dir '{}': {}",
                    dir.display(),
                    e
                );
                return;
            }
            let output_path = summary_output_path(dir, input_path);
            match serde_json::to_string_pretty(summary) {
                Ok(json) => match std::fs::write(&output_path, json) {
                    Ok(()) => eprintln!("  summary: {}", output_path.display()),
                    Err(e) => eprintln!(
                        "[ERROR] cannot write summary '{}': {}",
                        output_path.display(),
                        e
                    ),
                },
                Err(e) => eprintln!("[ERROR] cannot serialize summary: {}", e),
            }
        }
        SummaryOutput::Stdout => match serde_json::to_string_pretty(summary) {
            Ok(json) => println!("{}", json),
            Err(e) => eprintln!("[ERROR] cannot serialize summary: {}", e),
        },
    }
}

fn summary_output_path(output_dir: &Path, input_path: &Path) -> PathBuf {
    let path_text = input_path.to_string_lossy();
    let path_hash = mylua_lsp::util::hash_bytes(path_text.as_bytes());
    output_dir.join(format!(
        "{}.{:016x}.summary.json",
        sanitize_summary_filename(input_path),
        path_hash
    ))
}

fn file_uri_for_path(path: &Path) -> Option<tower_lsp_server::ls_types::Uri> {
    mylua_lsp::workspace_scanner::path_to_uri(path)
}

fn sanitize_summary_filename(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let sanitized: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '_',
            c => c,
        })
        .collect();

    sanitized.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn parse_args_enables_default_summary_output() {
        let options = parse_args(vec![
            "--summary".to_string(),
            "tests/lua-root/diagnostics.lua".to_string(),
        ])
        .expect("args should parse");

        assert_eq!(options.files, vec!["tests/lua-root/diagnostics.lua"]);
        assert_eq!(
            options.summary_output,
            SummaryOutput::Directory(PathBuf::from("target/lua-summary"))
        );
    }

    #[test]
    fn summary_output_path_sanitizes_input_path() {
        let path = summary_output_path(
            Path::new("target/lua-summary"),
            Path::new("tests/lua-root/diagnostics.lua"),
        );

        let filename = path.file_name().unwrap().to_string_lossy();
        assert!(filename.starts_with("tests_lua-root_diagnostics.lua."));
        assert!(filename.ends_with(".summary.json"));
    }

    #[test]
    fn summary_output_path_avoids_sanitized_name_collisions() {
        let slash_path = summary_output_path(Path::new("target/lua-summary"), Path::new("tests/a.lua"));
        let underscore_path = summary_output_path(Path::new("target/lua-summary"), Path::new("tests_a.lua"));

        assert_ne!(slash_path, underscore_path);
    }

    #[test]
    fn file_uri_for_path_handles_special_characters() {
        let uri = file_uri_for_path(Path::new("lua perf #?.lua")).expect("path should convert to URI");

        assert!(!uri.to_string().contains('#'));
        assert!(!uri.to_string().contains('?'));
    }
}
