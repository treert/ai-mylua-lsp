use tower_lsp_server::ls_types::*;
use crate::util::{node_text, truncate, LineIndex};

pub(super) fn collect_errors_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    if node.is_error() {
        diagnostics.push(Diagnostic {
            range: line_index.ts_node_to_range(node, source),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Syntax error near '{}'", truncate(node_text(node, source), 40)),
            ..Default::default()
        });
    } else if node.is_missing() {
        diagnostics.push(Diagnostic {
            range: line_index.ts_node_to_range(node, source),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Missing '{}'", node.kind()),
            ..Default::default()
        });
    }

    if node.has_error() && cursor.goto_first_child() {
        loop {
            collect_errors_recursive(cursor, source, diagnostics, line_index);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
