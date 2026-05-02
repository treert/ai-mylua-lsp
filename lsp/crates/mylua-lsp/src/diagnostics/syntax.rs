use crate::util::{byte_col_to_utf16_col, node_text, truncate, LineIndex};
use tower_lsp_server::ls_types::*;

pub(super) fn collect_errors_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    if node.is_error() {
        let excerpt = syntax_error_excerpt(node, source);
        diagnostics.push(Diagnostic {
            range: syntax_error_range(node, source, line_index),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Syntax error near '{}'", truncate(&excerpt, 40)),
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

fn syntax_error_range(node: tree_sitter::Node, source: &[u8], line_index: &LineIndex) -> Range {
    let mut range = line_index.ts_node_to_range(node, source);
    if range.start.line == range.end.line {
        return range;
    }

    // Tree-sitter recovery ERROR nodes can absorb following valid
    // statements. Keep the squiggle on the line where recovery started
    // so later code does not appear to be part of the syntax error.
    let start_row = node.start_position().row;
    let line_bytes = line_index.line_bytes_for_row(source, start_row);
    range.end = Position {
        line: range.start.line,
        character: byte_col_to_utf16_col(line_bytes, line_bytes.len()),
    };
    range
}

fn syntax_error_excerpt(node: tree_sitter::Node, source: &[u8]) -> String {
    let text = node_text(node, source);
    if node.start_position().row == node.end_position().row {
        return text.to_string();
    }
    text.lines().next().unwrap_or(text).to_string()
}
