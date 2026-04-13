use tower_lsp_server::ls_types::*;
use crate::util::node_text;

// Legend indices must match the order in semantic_tokens_legend() in main.rs
const TT_FUNCTION: u32 = 0;
const TT_VARIABLE: u32 = 1;
const TT_PARAMETER: u32 = 2;
const TT_KEYWORD: u32 = 3;
const TT_STRING: u32 = 4;
const TT_NUMBER: u32 = 5;
const TT_COMMENT: u32 = 6;
const TT_OPERATOR: u32 = 7;

const TM_DECLARATION: u32 = 1 << 0;
const TM_DEFINITION: u32 = 1 << 1;
#[allow(dead_code)]
const TM_READONLY: u32 = 1 << 2;

struct TokenCollector {
    tokens: Vec<SemanticToken>,
    prev_line: u32,
    prev_start: u32,
}

impl TokenCollector {
    fn new() -> Self {
        Self {
            tokens: Vec::new(),
            prev_line: 0,
            prev_start: 0,
        }
    }

    fn push(&mut self, line: u32, start: u32, length: u32, token_type: u32, modifiers: u32) {
        let delta_line = line - self.prev_line;
        let delta_start = if delta_line == 0 {
            start - self.prev_start
        } else {
            start
        };
        self.tokens.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: modifiers,
        });
        self.prev_line = line;
        self.prev_start = start;
    }
}

pub fn collect_semantic_tokens(root: tree_sitter::Node, source: &[u8]) -> Vec<SemanticToken> {
    let mut collector = TokenCollector::new();
    let mut cursor = root.walk();
    visit_node(&mut cursor, source, &mut collector);
    collector.tokens
}

fn visit_node(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    collector: &mut TokenCollector,
) {
    let node = cursor.node();
    let kind = node.kind();

    match kind {
        "comment" | "emmy_line" => {
            emit_node(node, TT_COMMENT, 0, collector);
        }
        "emmy_comment" => {
            // Recurse into emmy_line children
        }
        "number" => {
            emit_node(node, TT_NUMBER, 0, collector);
        }
        "short_string" | "long_string" | "string" => {
            if kind == "string" {
                // recurse into children
            } else {
                emit_node(node, TT_STRING, 0, collector);
                return;
            }
        }
        "short_string_content_double" | "short_string_content_single" | "long_string_content" => {
            return;
        }
        "nil" | "true" | "false" | "vararg_expression" => {
            emit_node(node, TT_KEYWORD, 0, collector);
        }
        "break_statement" => {
            emit_node(node, TT_KEYWORD, 0, collector);
            return;
        }
        "identifier" => {
            if let Some(parent) = node.parent() {
                let (tt, tm) = classify_identifier(node, parent, source);
                emit_node(node, tt, tm, collector);
            }
            return;
        }
        _ => {
            emit_keywords_in_node(node, source, collector);
        }
    }

    if cursor.goto_first_child() {
        loop {
            visit_node(cursor, source, collector);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn classify_identifier(
    node: tree_sitter::Node,
    parent: tree_sitter::Node,
    _source: &[u8],
) -> (u32, u32) {
    let parent_kind = parent.kind();
    match parent_kind {
        "function_name" => (TT_FUNCTION, TM_DEFINITION),
        "function_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                (TT_FUNCTION, TM_DEFINITION)
            } else {
                (TT_VARIABLE, 0)
            }
        }
        "local_function_declaration" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                (TT_FUNCTION, TM_DECLARATION | TM_DEFINITION)
            } else {
                (TT_VARIABLE, 0)
            }
        }
        "local_declaration" | "attribute_name_list" => (TT_VARIABLE, TM_DECLARATION),
        "for_numeric_statement" => {
            if parent.child_by_field_name("name").map(|n| n.id()) == Some(node.id()) {
                (TT_VARIABLE, TM_DECLARATION)
            } else {
                (TT_VARIABLE, 0)
            }
        }
        "name_list" => {
            if let Some(grandparent) = parent.parent() {
                match grandparent.kind() {
                    "for_generic_statement" => (TT_VARIABLE, TM_DECLARATION),
                    "parameter_list" | "_parameter_list_content" => (TT_PARAMETER, TM_DECLARATION),
                    _ => (TT_VARIABLE, 0),
                }
            } else {
                (TT_VARIABLE, 0)
            }
        }
        "parameter_list" | "_parameter_list_content" => (TT_PARAMETER, TM_DECLARATION),
        "function_call" => {
            if parent.child_by_field_name("callee").map(|n| n.id()) == Some(node.id()) {
                (TT_FUNCTION, 0)
            } else if parent.child_by_field_name("method").map(|n| n.id()) == Some(node.id()) {
                (TT_FUNCTION, 0)
            } else {
                (TT_VARIABLE, 0)
            }
        }
        "goto_statement" | "label_statement" => (TT_VARIABLE, 0),
        _ => (TT_VARIABLE, 0),
    }
}

fn emit_node(node: tree_sitter::Node, token_type: u32, modifiers: u32, collector: &mut TokenCollector) {
    let start = node.start_position();
    let end_pos = node.end_position();

    if start.row == end_pos.row {
        let length = (end_pos.column - start.column) as u32;
        if length > 0 {
            collector.push(start.row as u32, start.column as u32, length, token_type, modifiers);
        }
    } else {
        // Multi-line: just mark the first line
        let first_line_end = node.utf8_text(&[]).map(|t| {
            t.find('\n').unwrap_or(t.len())
        }).unwrap_or(0);
        if first_line_end > 0 {
            collector.push(start.row as u32, start.column as u32, first_line_end as u32, token_type, modifiers);
        }
    }
}

fn emit_keywords_in_node(
    node: tree_sitter::Node,
    source: &[u8],
    collector: &mut TokenCollector,
) {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if !child.is_named() {
                let text = node_text(child, source);
                if is_lua_keyword(text) {
                    emit_node(child, TT_KEYWORD, 0, collector);
                } else if is_lua_operator(text) {
                    emit_node(child, TT_OPERATOR, 0, collector);
                }
            }
        }
    }
}

fn is_lua_keyword(s: &str) -> bool {
    matches!(
        s,
        "and" | "break" | "do" | "else" | "elseif" | "end" | "false" | "for"
            | "function" | "goto" | "if" | "in" | "local" | "nil" | "not" | "or"
            | "repeat" | "return" | "then" | "true" | "until" | "while"
    )
}

fn is_lua_operator(s: &str) -> bool {
    matches!(
        s,
        "+" | "-" | "*" | "/" | "//" | "%" | "^" | "#" | "&" | "|" | "~" | ">>"
            | "<<" | ".." | "==" | "~=" | "<" | "<=" | ">" | ">=" | "=" | "..."
    )
}
