use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::util::{node_text, position_to_byte_offset};
use crate::workspace_index::WorkspaceIndex;

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end",
    "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return",
    "then", "true", "until", "while",
];

pub fn complete(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceIndex,
) -> Vec<CompletionItem> {
    let prefix = get_prefix(doc, position);
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    collect_scope_completions(doc, uri, position, &prefix, &mut items, &mut seen);
    collect_global_completions(index, &prefix, &mut items, &mut seen);
    collect_keyword_completions(&prefix, &mut items, &mut seen);

    items
}

fn get_prefix(doc: &Document, position: Position) -> String {
    let Some(offset) = position_to_byte_offset(&doc.text, position) else {
        return String::new();
    };
    let bytes = doc.text.as_bytes();
    let mut start = offset;
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric() || b == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    String::from_utf8_lossy(&bytes[start..offset]).to_string()
}

fn collect_scope_completions(
    doc: &Document,
    _uri: &Uri,
    position: Position,
    prefix: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    let Some(offset) = position_to_byte_offset(&doc.text, position) else {
        return;
    };
    let source = doc.text.as_bytes();
    let root = doc.tree.root_node();

    let Some(node_at) = root.descendant_for_byte_range(offset, offset) else {
        return;
    };

    let mut current = node_at;
    loop {
        scan_block_locals(current, offset, prefix, source, items, seen);
        if let Some(parent) = current.parent() {
            if parent.kind() == "function_body" {
                scan_params(parent, prefix, source, items, seen);
            }
            current = parent;
        } else {
            break;
        }
    }
}

fn scan_block_locals(
    block: tree_sitter::Node,
    before_offset: usize,
    prefix: &str,
    source: &[u8],
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    let mut cursor = block.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        if child.start_byte() >= before_offset {
            break;
        }
        match child.kind() {
            "local_declaration" => {
                if let Some(names) = child.child_by_field_name("names") {
                    add_identifiers_from(names, prefix, source, items, seen, CompletionItemKind::VARIABLE);
                }
            }
            "local_function_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    add_if_match(name, prefix, source, items, seen, CompletionItemKind::FUNCTION);
                }
            }
            "for_numeric_statement" => {
                if let Some(name) = child.child_by_field_name("name") {
                    add_if_match(name, prefix, source, items, seen, CompletionItemKind::VARIABLE);
                }
            }
            "for_generic_statement" => {
                if let Some(names) = child.child_by_field_name("names") {
                    add_identifiers_from(names, prefix, source, items, seen, CompletionItemKind::VARIABLE);
                }
            }
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn scan_params(
    func_body: tree_sitter::Node,
    prefix: &str,
    source: &[u8],
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    if let Some(params) = func_body.child_by_field_name("parameters") {
        for i in 0..params.named_child_count() {
            if let Some(child) = params.named_child(i as u32) {
                if child.kind() == "identifier" {
                    add_if_match(child, prefix, source, items, seen, CompletionItemKind::VARIABLE);
                } else if child.kind() == "name_list" {
                    add_identifiers_from(child, prefix, source, items, seen, CompletionItemKind::VARIABLE);
                }
            }
        }
    }
}

fn add_identifiers_from(
    node: tree_sitter::Node,
    prefix: &str,
    source: &[u8],
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    kind: CompletionItemKind,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            if child.kind() == "identifier" {
                add_if_match(child, prefix, source, items, seen, kind);
            }
        }
    }
}

fn add_if_match(
    node: tree_sitter::Node,
    prefix: &str,
    source: &[u8],
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
    kind: CompletionItemKind,
) {
    let name = node_text(node, source);
    if name.starts_with(prefix) && !seen.contains(name) {
        seen.insert(name.to_string());
        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(kind),
            ..Default::default()
        });
    }
}

fn collect_global_completions(
    index: &WorkspaceIndex,
    prefix: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    for (name, entries) in &index.globals {
        if name.starts_with(prefix) && !seen.contains(name) {
            seen.insert(name.clone());
            let kind = if entries.iter().any(|e| matches!(e.kind, crate::types::DefKind::GlobalFunction)) {
                CompletionItemKind::FUNCTION
            } else {
                CompletionItemKind::VARIABLE
            };
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(kind),
                ..Default::default()
            });
        }
    }
}

fn collect_keyword_completions(
    prefix: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    for kw in LUA_KEYWORDS {
        if kw.starts_with(prefix) && !seen.contains(*kw) {
            seen.insert(kw.to_string());
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }
}
