use tower_lsp_server::ls_types::*;
use crate::scope::ScopeTree;
use crate::type_system::TypeFact;
use crate::util::{node_text, LineIndex};
use super::type_compat::{infer_literal_type, is_type_compatible};

pub(super) fn check_type_mismatch_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    // Pass 1 — check the initial `local x = <rhs>` assignment against
    // `---@type` declared on the same line. Uses scope_tree declarations
    // filtered to `is_emmy_annotated`.
    for decl in scope_tree.all_declarations() {
        if !decl.is_emmy_annotated {
            continue;
        }
        let Some(ref declared) = decl.type_fact else { continue };
        let actual = find_actual_type_for_local(&decl.name, &decl.range, root, source, scope_tree);
        if actual == TypeFact::Unknown {
            continue;
        }
        if !is_type_compatible(declared, &actual) {
            diagnostics.push(Diagnostic {
                range: line_index.byte_range_to_lsp_range(decl.range),
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!(
                    "Type mismatch: declared '{}', got '{}'",
                    declared, actual
                ),
                ..Default::default()
            });
        }
    }

    // Pass 2 — follow-up `x = <rhs>` assignments to locals previously
    // declared with `---@type T`. Walks every `assignment_statement`,
    // resolves the LHS identifier via `scope_tree` back to its
    // declaration site, and (if the decl site carries an Emmy type
    // fact) compares RHS literal type against the declared type.
    check_assignment_type_mismatches(
        root, source, scope_tree, diagnostics, severity, line_index,
    );
}

fn check_assignment_type_mismatches(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let mut cursor = root.walk();
    walk_assignment_nodes(&mut cursor, source, scope_tree, diagnostics, severity, line_index);
}

fn walk_assignment_nodes(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    if node.kind() == "assignment_statement" {
        inspect_assignment_for_mismatch(node, source, scope_tree, diagnostics, severity, line_index);
    }
    if cursor.goto_first_child() {
        loop {
            walk_assignment_nodes(cursor, source, scope_tree, diagnostics, severity, line_index);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn inspect_assignment_for_mismatch(
    node: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let Some(left) = node.child_by_field_name("left") else { return };
    let Some(right) = node.child_by_field_name("right") else { return };

    for i in 0..left.named_child_count() {
        let Some(lhs) = left.named_child(i as u32) else { continue };
        let ident_node = if lhs.kind() == "identifier" {
            Some(lhs)
        } else if lhs.kind() == "variable"
            && lhs.child_by_field_name("object").is_none()
            && lhs.child_by_field_name("index").is_none()
        {
            lhs.named_child(0).filter(|c| c.kind() == "identifier")
        } else {
            None
        };
        let Some(ident) = ident_node else { continue };

        let name = node_text(ident, source);
        let Some(decl) = scope_tree.resolve_decl(ident.start_byte(), name) else {
            continue;
        };

        // The local's declaration site must carry an Emmy annotation.
        if !decl.is_emmy_annotated {
            continue;
        }
        let Some(ref declared_type) = decl.type_fact else { continue };

        let Some(value_expr) = right.named_child(i as u32) else { continue };
        let actual = infer_literal_type(value_expr, source, scope_tree);
        if actual == TypeFact::Unknown {
            continue;
        }
        if !is_type_compatible(declared_type, &actual) {
            diagnostics.push(Diagnostic {
                range: line_index.ts_node_to_range(ident, source),
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!(
                    "Type mismatch on assignment to '{}': declared '{}', got '{}'",
                    name, declared_type, actual
                ),
                ..Default::default()
            });
        }
    }
}

fn find_actual_type_for_local(
    name: &str,
    decl_range: &crate::util::ByteRange,
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &crate::scope::ScopeTree,
) -> TypeFact {
    let target_line = decl_range.start_row;
    find_local_rhs_type(root, name, target_line, scope_tree, source)
}

fn find_local_rhs_type(
    node: tree_sitter::Node,
    name: &str,
    target_line: u32,
    scope_tree: &crate::scope::ScopeTree,
    source: &[u8],
) -> TypeFact {
    if node.kind() == "local_declaration" {
        if let Some(names) = node.child_by_field_name("names") {
            for i in 0..names.named_child_count() {
                if let Some(n) = names.named_child(i as u32) {
                    if n.kind() == "identifier" && node_text(n, source) == name {
                        let node_line = n.start_position().row as u32;
                        if node_line == target_line {
                            if let Some(values) = node.child_by_field_name("values") {
                                if let Some(val) = values.named_child(i as u32) {
                                    return infer_literal_type(val, source, scope_tree);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            let result = find_local_rhs_type(child, name, target_line, scope_tree, source);
            if result != TypeFact::Unknown {
                return result;
            }
        }
    }
    TypeFact::Unknown
}
