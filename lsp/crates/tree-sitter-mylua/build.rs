use std::path::Path;

fn main() {
    let grammar_src = Path::new("../../../grammar/src");

    let parser_c = grammar_src.join("parser.c");
    let scanner_c = grammar_src.join("scanner.c");

    if !parser_c.exists() {
        panic!(
            "grammar/src/parser.c not found — run `npx tree-sitter generate` in grammar/ first"
        );
    }

    cc::Build::new()
        .include(grammar_src)
        .file(&parser_c)
        .file(&scanner_c)
        .warnings(false)
        .compile("tree_sitter_mylua");

    println!("cargo:rerun-if-changed={}", parser_c.display());
    println!("cargo:rerun-if-changed={}", scanner_c.display());
}
