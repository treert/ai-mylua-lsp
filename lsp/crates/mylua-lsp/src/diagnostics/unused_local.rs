use tower_lsp_server::ls_types::*;
use crate::scope::{ScopeDecl, ScopeTree};
use crate::types::DefKind;
use crate::util::node_text;

/// Warn on locals that are declared but never referenced. Uses the
/// `ScopeTree` to find every declaration, then scans the file for
/// matching identifier usages (excluding the declaration site
/// itself). `_` / `_*` names are conventionally "intentionally
/// discarded" and don't trigger the warning.
pub(super) fn check_unused_locals(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    // Count references per (name, decl_byte) by walking the tree
    // and resolving each identifier through the scope tree.
    let mut ref_count: std::collections::HashMap<(String, usize), usize> =
        std::collections::HashMap::new();
    let mut cursor = root.walk();
    count_identifier_references(&mut cursor, source, scope_tree, &mut ref_count);

    // Any declaration whose ref_count is zero is unused.
    for decl in scope_tree.all_declarations() {
        // Convention: `_` or `_something` indicate intentional discard.
        if decl.name == "_" || decl.name.starts_with('_') {
            continue;
        }
        if is_implicit_method_self_decl(decl, source) {
            continue;
        }
        let key = (decl.name.clone(), decl.decl_byte);
        if ref_count.get(&key).copied().unwrap_or(0) == 0 {
            diagnostics.push(Diagnostic {
                range: decl.selection_range.into(),
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!("Unused local '{}'", decl.name),
                ..Default::default()
            });
        }
    }
}

fn is_implicit_method_self_decl(decl: &ScopeDecl, source: &[u8]) -> bool {
    if decl.kind != DefKind::Parameter || decl.name != "self" {
        return false;
    }

    // Colon-method `self` is synthesized by scope building and has no
    // identifier span in the source. Explicit `function f(self)` params
    // still have selection text "self" and should be diagnosed normally.
    let range = decl.selection_range;
    source.get(range.start_byte..range.end_byte) != Some(b"self")
}

fn count_identifier_references(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    scope_tree: &ScopeTree,
    ref_count: &mut std::collections::HashMap<(String, usize), usize>,
) {
    let node = cursor.node();
    if matches!(node.kind(), "identifier" | "varargs" | "vararg_expression") {
        let name = if matches!(node.kind(), "varargs" | "vararg_expression") {
            "..."
        } else {
            node_text(node, source)
        };
        let byte = node.start_byte();
        if let Some(decl) = scope_tree.resolve_decl(byte, name) {
            // Skip if this identifier IS the declaration itself —
            // decl.decl_byte's occurrence is the binding, not a use.
            if byte != decl.decl_byte {
                *ref_count.entry((name.to_string(), decl.decl_byte)).or_insert(0) += 1;
            }
        }
    }
    if cursor.goto_first_child() {
        loop {
            // Skip bracket-key-only table constructors — they contain
            // no identifiers, only literal key-value pairs.
            let child = cursor.node();
            if child.kind() == "table_constructor"
                && crate::util::is_bracket_key_only_table(child)
            {
                if !cursor.goto_next_sibling() {
                    break;
                }
                continue;
            }
            count_identifier_references(cursor, source, scope_tree, ref_count);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
