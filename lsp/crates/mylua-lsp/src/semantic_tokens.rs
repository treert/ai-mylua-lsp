use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::util::node_text;

const TT_VARIABLE: u32 = 0;
const TM_DEFAULT_LIBRARY: u32 = 1 << 0;

pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![SemanticTokenType::VARIABLE],
        token_modifiers: vec![SemanticTokenModifier::DEFAULT_LIBRARY],
    }
}

pub fn collect_semantic_tokens(root: tree_sitter::Node, source: &[u8]) -> Vec<SemanticToken> {
    let locals = collect_all_locals(root, source);
    let mut raw: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut cursor = root.walk();
    collect_variable_tokens(&mut cursor, source, &locals, &mut raw);

    raw.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut result = Vec::with_capacity(raw.len());
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;
    for &(line, col, length, modifiers) in &raw {
        let delta_line = line - prev_line;
        let delta_start = if delta_line == 0 { col - prev_start } else { col };
        result.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: TT_VARIABLE,
            token_modifiers_bitset: modifiers,
        });
        prev_line = line;
        prev_start = col;
    }
    result
}

/// Emit a semantic token for every identifier that is a variable reference
/// (not a field access or method name). Globals get `defaultLibrary`, locals get 0.
fn collect_variable_tokens(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    locals: &HashSet<String>,
    tokens: &mut Vec<(u32, u32, u32, u32)>,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && !is_field_or_method(node) {
        let name = node_text(node, source);
        let modifiers = if name == "self" {
            if is_inside_colon_method(node) {
                return; // implicit local in : method, let TextMate handle
            }
            TM_DEFAULT_LIBRARY
        } else if locals.contains(name) {
            0
        } else {
            TM_DEFAULT_LIBRARY
        };
        let start = node.start_position();
        let end = node.end_position();
        if start.row == end.row {
            let length = (end.column - start.column) as u32;
            if length > 0 {
                tokens.push((start.row as u32, start.column as u32, length, modifiers));
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_variable_tokens(cursor, source, locals, tokens);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Returns true if this identifier is a field access (obj.field),
/// method name (obj:method), or non-root part of function_name (function a.b:c → b, c).
fn is_field_or_method(node: tree_sitter::Node) -> bool {
    if let Some(parent) = node.parent() {
        match parent.kind() {
            "variable" => {
                parent.child_by_field_name("field").map(|n| n.id()) == Some(node.id())
            }
            "function_call" => {
                parent.child_by_field_name("method").map(|n| n.id()) == Some(node.id())
            }
            "function_name" => {
                parent.child(0).map(|n| n.id()) != Some(node.id())
            }
            _ => false,
        }
    } else {
        false
    }
}

/// Check if this node is inside a `:` method body (where `self` is an implicit local).
fn is_inside_colon_method(node: tree_sitter::Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "function_body" {
            if let Some(gp) = parent.parent() {
                if gp.kind() == "function_declaration" {
                    if let Some(fname) = gp.child_by_field_name("name") {
                        return fname.child_by_field_name("method").is_some();
                    }
                }
            }
            return false;
        }
        current = parent;
    }
    false
}

// ---------------------------------------------------------------------------
// Local name collection
// ---------------------------------------------------------------------------

fn collect_all_locals(root: tree_sitter::Node, source: &[u8]) -> HashSet<String> {
    let mut locals = HashSet::new();
    let mut cursor = root.walk();
    collect_locals_recursive(&mut cursor, source, &mut locals);
    locals
}

fn collect_locals_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    locals: &mut HashSet<String>,
) {
    let node = cursor.node();
    match node.kind() {
        "local_declaration" => {
            if let Some(names) = node.child_by_field_name("names") {
                collect_identifiers_in(names, source, locals);
            }
        }
        "local_function_declaration" => {
            if let Some(name) = node.child_by_field_name("name") {
                locals.insert(node_text(name, source).to_string());
            }
        }
        "for_numeric_statement" => {
            if let Some(name) = node.child_by_field_name("name") {
                locals.insert(node_text(name, source).to_string());
            }
        }
        "for_generic_statement" => {
            if let Some(names) = node.child_by_field_name("names") {
                collect_identifiers_in(names, source, locals);
            }
        }
        "function_body" => {
            if let Some(params) = node.child_by_field_name("parameters") {
                collect_identifiers_in(params, source, locals);
            }
        }
        _ => {}
    }

    if cursor.goto_first_child() {
        loop {
            collect_locals_recursive(cursor, source, locals);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Recursively collect all identifier names within a node subtree.
fn collect_identifiers_in(node: tree_sitter::Node, source: &[u8], locals: &mut HashSet<String>) {
    if node.kind() == "identifier" {
        locals.insert(node_text(node, source).to_string());
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_identifiers_in(child, source, locals);
        }
    }
}
