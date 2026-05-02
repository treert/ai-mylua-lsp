use std::collections::HashSet;

use tower_lsp_server::ls_types::*;

use crate::emmy::{parse_emmy_comments, EmmyAnnotation};
use crate::util::{node_text, LineIndex};

pub(super) fn check_param_annotation_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    line_index: &LineIndex,
) {
    let mut functions = Vec::new();
    collect_function_like_nodes(root, &mut functions);

    for fun in functions {
        let Some(mut lua_params) = collect_lua_param_names(fun, source) else {
            continue;
        };
        if is_colon_method(fun, source) {
            lua_params.insert("self".to_string());
        }
        let anchor = match fun.kind() {
            "function_definition" => {
                crate::summary_builder::enclosing_statement_for_function_expr(fun).unwrap_or(fun)
            }
            _ => fun,
        };
        for line in collect_preceding_emmy_lines(anchor, source, line_index) {
            for ann in parse_emmy_comments(&line.text) {
                let EmmyAnnotation::Param { name, .. } = ann else {
                    continue;
                };
                if lua_params.contains(&name) {
                    continue;
                }
                diagnostics.push(Diagnostic {
                    range: line.range,
                    severity: Some(DiagnosticSeverity::WARNING),
                    source: Some("mylua".to_string()),
                    message: format!("@param '{}' does not match any Lua parameter", name,),
                    ..Default::default()
                });
            }
        }
    }
}

fn collect_function_like_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if matches!(
        node.kind(),
        "function_declaration" | "local_function_declaration" | "function_definition"
    ) {
        out.push(node);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_function_like_nodes(child, out);
        }
    }
}

fn collect_lua_param_names(fun: tree_sitter::Node, source: &[u8]) -> Option<HashSet<String>> {
    let body = fun.child_by_field_name("body")?;
    let param_list = body.child_by_field_name("parameters")?;
    let mut names = HashSet::new();

    for i in 0..param_list.child_count() {
        let Some(child) = param_list.child(i as u32) else {
            continue;
        };
        match child.kind() {
            "identifier" => {
                names.insert(node_text(child, source).to_string());
            }
            "name_list" => {
                for j in 0..child.named_child_count() {
                    let Some(id) = child.named_child(j as u32) else {
                        continue;
                    };
                    if id.kind() == "identifier" {
                        names.insert(node_text(id, source).to_string());
                    }
                }
            }
            "varargs" => {
                names.insert("...".to_string());
            }
            _ => {
                if !child.is_named() && node_text(child, source) == "..." {
                    names.insert("...".to_string());
                }
            }
        }
    }

    Some(names)
}

fn is_colon_method(fun: tree_sitter::Node, source: &[u8]) -> bool {
    fun.child_by_field_name("name")
        .is_some_and(|name| node_text(name, source).contains(':'))
}

struct AnnotationLine {
    text: String,
    range: Range,
}

fn collect_preceding_emmy_lines(
    node: tree_sitter::Node,
    source: &[u8],
    line_index: &LineIndex,
) -> Vec<AnnotationLine> {
    let mut lines = Vec::new();
    let mut sibling = node.prev_sibling();
    let mut next_start_row = node.start_position().row;

    while let Some(prev) = sibling {
        let prev_end_row = prev.end_position().row;
        if next_start_row > prev_end_row + 1 {
            break;
        }

        match prev.kind() {
            "emmy_comment" => {
                for i in 0..prev.named_child_count() {
                    let Some(line) = prev.named_child(i as u32) else {
                        continue;
                    };
                    if line.kind() == "emmy_line" {
                        lines.push(AnnotationLine {
                            text: node_text(line, source).to_string(),
                            range: line_index.ts_node_to_range(line, source),
                        });
                    }
                }
                next_start_row = prev.start_position().row;
                sibling = prev.prev_sibling();
                continue;
            }
            "comment" => {
                let text = node_text(prev, source);
                if text.starts_with("---") {
                    if text.starts_with("---@") {
                        lines.push(AnnotationLine {
                            text: text.to_string(),
                            range: line_index.ts_node_to_range(prev, source),
                        });
                    }
                    next_start_row = prev.start_position().row;
                    sibling = prev.prev_sibling();
                    continue;
                }
                if let Some(block_lines) = extract_block_comment_lines(text) {
                    let range = line_index.ts_node_to_range(prev, source);
                    for line in block_lines {
                        lines.push(AnnotationLine { text: line, range });
                    }
                    next_start_row = prev.start_position().row;
                    sibling = prev.prev_sibling();
                    continue;
                }
            }
            _ => {}
        }
        break;
    }

    lines
}

fn extract_block_comment_lines(text: &str) -> Option<Vec<String>> {
    if !text.starts_with("--[") {
        return None;
    }
    let bytes = text.as_bytes();
    let mut i = 3;
    while i < bytes.len() && bytes[i] == b'=' {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'[' {
        return None;
    }
    let eq_count = i - 3;
    let start = i + 1;
    let closing = format!("]{}]", "=".repeat(eq_count));
    let end = text[start..].find(&closing).map(|pos| start + pos)?;
    let lines = text[start..end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| format!("--- {}", line))
        .collect();
    Some(lines)
}
