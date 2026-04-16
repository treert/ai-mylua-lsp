use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::scope::ScopeTree;
use crate::util::{ts_node_to_range, node_text, truncate};
use crate::aggregation::WorkspaceAggregation;

const LUA_BUILTINS: &[&str] = &[
    "print", "type", "tostring", "tonumber", "error", "assert", "pcall", "xpcall",
    "pairs", "ipairs", "next", "select", "require", "dofile", "loadfile", "load",
    "rawget", "rawset", "rawequal", "rawlen", "setmetatable", "getmetatable",
    "collectgarbage", "unpack", "table", "string", "math", "io", "os", "debug",
    "coroutine", "package", "utf8", "arg", "_G", "_ENV", "_VERSION",
    "self", "true", "false", "nil",
];

pub fn collect_diagnostics(root: tree_sitter::Node, source: &[u8]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut cursor = root.walk();
    collect_errors_recursive(&mut cursor, source, &mut diagnostics);
    diagnostics
}

pub fn collect_semantic_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let builtins: HashSet<&str> = LUA_BUILTINS.iter().copied().collect();

    let mut cursor = root.walk();
    check_undefined_globals(&mut cursor, source, &builtins, index, scope_tree, &mut diagnostics);
    diagnostics
}

fn check_undefined_globals(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    builtins: &HashSet<&str>,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let node = cursor.node();

    if node.kind() == "identifier" {
        if let Some(parent) = node.parent() {
            let is_bare_var = parent.kind() == "variable" && parent.child_count() == 1;
            let is_definition = matches!(
                parent.kind(),
                "function_name" | "attribute_name_list" | "name_list" | "label_statement"
            );
            if is_bare_var && !is_definition {
                let name = node_text(node, source);
                let byte_offset = node.start_byte();
                let is_local = scope_tree.resolve_decl(byte_offset, name).is_some();
                if !is_local
                    && !builtins.contains(name)
                    && !index.globals.contains_key(name)
                {
                    diagnostics.push(Diagnostic {
                        range: ts_node_to_range(node),
                        severity: Some(DiagnosticSeverity::WARNING),
                        source: Some("mylua".to_string()),
                        message: format!("Undefined global '{}'", name),
                        ..Default::default()
                    });
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            check_undefined_globals(cursor, source, builtins, index, scope_tree, diagnostics);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
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
