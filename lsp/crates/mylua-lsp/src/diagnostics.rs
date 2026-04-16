use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::config::DiagnosticsConfig;
use crate::resolver;
use crate::scope::ScopeTree;
use crate::type_system::{TypeFact, KnownType};
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
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diag_config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    if !diag_config.enable {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    let builtins: HashSet<&str> = LUA_BUILTINS.iter().copied().collect();

    let mut cursor = root.walk();
    if let Some(severity) = diag_config.undefined_global.to_lsp_severity() {
        check_undefined_globals(&mut cursor, source, &builtins, index, scope_tree, &mut diagnostics, severity);
    }
    if let Some(severity) = diag_config.emmy_unknown_field.to_lsp_severity() {
        check_field_access_diagnostics(root, source, uri, index, &mut diagnostics, severity);
    }
    diagnostics
}

fn check_undefined_globals(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    builtins: &HashSet<&str>,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
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
                        severity: Some(severity),
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
            check_undefined_globals(cursor, source, builtins, index, scope_tree, diagnostics, severity);
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

fn check_field_access_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let mut cursor = root.walk();
    collect_field_diagnostics(&mut cursor, source, uri, index, diagnostics, severity);
}

/// Returns true if `node` is on the left-hand side of an assignment statement.
fn is_assignment_target(node: tree_sitter::Node) -> bool {
    if let Some(parent) = node.parent() {
        if parent.kind() == "variable_list" {
            if let Some(grandparent) = parent.parent() {
                if grandparent.kind() == "assignment_statement" {
                    if let Some(left) = grandparent.child_by_field_name("left") {
                        return left.id() == parent.id();
                    }
                }
            }
        }
    }
    false
}

fn collect_field_diagnostics(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let node = cursor.node();

    // Handle both "field_expression" and "variable" with object.field pattern
    let is_dotted = matches!(node.kind(), "field_expression" | "variable")
        && node.child_by_field_name("object").is_some()
        && node.child_by_field_name("field").is_some();

    if is_dotted {
        // Skip assignment targets: `obj.field = value` is a definition, not a read
        if !is_assignment_target(node) {
            if let (Some(object), Some(field)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("field"),
            ) {
                let base_fact = crate::hover::infer_node_type(object, source, uri, index);
                let field_name = node_text(field, source).to_string();

                let resolved_base = resolver::resolve_type(&base_fact, index);
                if let TypeFact::Known(KnownType::EmmyType(type_name)) = &resolved_base.type_fact {
                    let field_resolved = resolver::resolve_field_chain(
                        &resolved_base.type_fact,
                        &[field_name.clone()],
                        index,
                    );
                    if field_resolved.type_fact == TypeFact::Unknown && field_resolved.def_uri.is_none() {
                        let qualified = format!("{}.{}", type_name, field_name);
                        if index.global_shard.get(&qualified).is_none() {
                            diagnostics.push(Diagnostic {
                                range: ts_node_to_range(field),
                                severity: Some(severity),
                                source: Some("mylua".to_string()),
                                message: format!(
                                    "Unknown field '{}' on type '{}'",
                                    field_name, type_name
                                ),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_field_diagnostics(cursor, source, uri, index, diagnostics, severity);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
