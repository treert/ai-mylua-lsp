use tower_lsp_server::ls_types::*;
use crate::scope::ScopeTree;
use crate::type_system::TypeFact;
use crate::util::{node_text, LineIndex};
use crate::aggregation::WorkspaceAggregation;
use super::type_compat::{infer_literal_type, is_type_compatible};

pub(super) fn check_type_mismatch_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let Some(summary) = index.summaries.get(uri).cloned() else { return };

    // Pass 1 — original behaviour: check the initial `local x = <rhs>`
    // assignment against `---@type` declared on the same line.
    for ltf in summary.local_type_facts.values() {
        if ltf.source != crate::summary::TypeFactSource::EmmyAnnotation {
            continue;
        }
        let declared = &ltf.type_fact;
        let actual = find_actual_type_for_local(&ltf.name, &ltf.range, root, source, &summary);
        if actual == TypeFact::Unknown {
            continue;
        }
        if !is_type_compatible(declared, &actual) {
            diagnostics.push(Diagnostic {
                range: line_index.byte_range_to_lsp_range(ltf.range, source),
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
    // Shadowing is handled correctly by `resolve_decl` — a new `local
    // x` inside an inner scope produces a different `decl_byte`, so
    // assignments inside that scope won't be checked against the
    // outer declaration's type.
    check_assignment_type_mismatches(
        root, source, &summary, scope_tree, diagnostics, severity, line_index,
    );
}

/// Walk every `assignment_statement` and, for each simple LHS
/// identifier that resolves to a local whose declaration carries an
/// Emmy type annotation, report mismatches between the declared type
/// and the RHS literal type.
fn check_assignment_type_mismatches(
    root: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let mut cursor = root.walk();
    walk_assignment_nodes(&mut cursor, source, summary, scope_tree, diagnostics, severity, line_index);
}

fn walk_assignment_nodes(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    if node.kind() == "assignment_statement" {
        inspect_assignment_for_mismatch(node, source, summary, scope_tree, diagnostics, severity, line_index);
    }
    if cursor.goto_first_child() {
        loop {
            walk_assignment_nodes(cursor, source, summary, scope_tree, diagnostics, severity, line_index);
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
    summary: &crate::summary::DocumentSummary,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let Some(left) = node.child_by_field_name("left") else { return };
    let Some(right) = node.child_by_field_name("right") else { return };

    // Iterate LHS/RHS pairs. Only single-variable, bare-identifier
    // LHS entries are checked — dotted / subscripted LHS like
    // `t.x = "str"` is a field write, not an assignment to a local
    // with an `@type` annotation, and belongs to field-access
    // diagnostics instead.
    for i in 0..left.named_child_count() {
        let Some(lhs) = left.named_child(i as u32) else { continue };
        // Single-identifier LHS: either a bare `identifier` or a
        // `variable` wrapping exactly one identifier. Skip dotted /
        // subscripted forms (they have an `object` or `index` field).
        let ident_node = if lhs.kind() == "identifier" {
            Some(lhs)
        } else if lhs.kind() == "variable"
            && lhs.child_by_field_name("object").is_none()
            && lhs.child_by_field_name("index").is_none()
        {
            // `variable` with a single identifier child.
            lhs.named_child(0).filter(|c| c.kind() == "identifier")
        } else {
            None
        };
        let Some(ident) = ident_node else { continue };

        let name = node_text(ident, source);
        // Resolve to the declaration site; skip names that don't
        // resolve (globals without an @type fact reach this path).
        let Some(decl) = scope_tree.resolve_decl(ident.start_byte(), name) else {
            continue;
        };

        // The local's declaration site must carry an EmmyAnnotation-
        // sourced type fact — otherwise there's no declared type to
        // compare against. `local_type_facts` is keyed by name; guard
        // against shadowing by matching the decl_byte against the
        // ltf's range start.
        let Some(ltf) = summary.local_type_facts.get(name) else { continue };
        if ltf.source != crate::summary::TypeFactSource::EmmyAnnotation {
            continue;
        }
        // Confirm the ltf corresponds to the resolved declaration: the
        // ltf's range line should match the decl line. tree-sitter
        // byte -> line lookup isn't free; fall back to line comparison
        // via the AST node at decl_byte.
        if !ltf_matches_decl(decl.decl_byte, ltf, source) {
            continue;
        }

        let Some(value_expr) = right.named_child(i as u32) else { continue };
        let actual = infer_literal_type(value_expr, source, summary);
        if actual == TypeFact::Unknown {
            continue;
        }
        if !is_type_compatible(&ltf.type_fact, &actual) {
            diagnostics.push(Diagnostic {
                range: line_index.ts_node_to_range(ident, source),
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!(
                    "Type mismatch on assignment to '{}': declared '{}', got '{}'",
                    name, ltf.type_fact, actual
                ),
                ..Default::default()
            });
        }
    }
}

/// True if the local-type-fact range starts on the same line as
/// `decl_byte`. Used to guard against a same-named later declaration
/// leaking its ltf onto an outer-scope assignment.
fn ltf_matches_decl(
    decl_byte: usize,
    ltf: &crate::summary::LocalTypeFact,
    source: &[u8],
) -> bool {
    // Count line breaks up to decl_byte.
    let mut line: u32 = 0;
    for &b in source.iter().take(decl_byte.min(source.len())) {
        if b == b'\n' {
            line += 1;
        }
    }
    ltf.range.start_row == line
}

fn find_actual_type_for_local(
    name: &str,
    decl_range: &crate::util::ByteRange,
    root: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
) -> TypeFact {
    let target_line = decl_range.start_row;
    find_local_rhs_type(root, name, target_line, summary, source)
}

/// Walk the subtree under `node` looking for a `local_declaration` that
/// declares `name` on line `target_line` and return the inferred literal
/// type of its matching RHS value. Pure recursion (no tree-cursor state)
/// keeps the traversal robust against early returns.
fn find_local_rhs_type(
    node: tree_sitter::Node,
    name: &str,
    target_line: u32,
    summary: &crate::summary::DocumentSummary,
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
                                    return infer_literal_type(val, source, summary);
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
            let result = find_local_rhs_type(child, name, target_line, summary, source);
            if result != TypeFact::Unknown {
                return result;
            }
        }
    }
    TypeFact::Unknown
}
