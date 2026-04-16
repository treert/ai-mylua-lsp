use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::scope::ScopeTree;
use crate::util::node_text;

const TT_VARIABLE: u32 = 0;
const TM_DEFAULT_LIBRARY: u32 = 1 << 0;

const LUA_BUILTINS: &[&str] = &[
    "print", "type", "tostring", "tonumber", "error", "assert", "pcall", "xpcall",
    "pairs", "ipairs", "next", "select", "require", "dofile", "loadfile", "load",
    "rawget", "rawset", "rawequal", "rawlen", "setmetatable", "getmetatable",
    "collectgarbage", "unpack", "table", "string", "math", "io", "os", "debug",
    "coroutine", "package", "utf8", "arg", "_G", "_ENV", "_VERSION",
];

pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![SemanticTokenType::VARIABLE],
        token_modifiers: vec![SemanticTokenModifier::DEFAULT_LIBRARY],
    }
}

pub fn collect_semantic_tokens(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
) -> Vec<SemanticToken> {
    let builtins: HashSet<&str> = LUA_BUILTINS.iter().copied().collect();
    let mut raw: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut cursor = root.walk();
    collect_variable_tokens(&mut cursor, source, scope_tree, &builtins, &mut raw);

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

fn collect_variable_tokens(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    scope_tree: &ScopeTree,
    builtins: &HashSet<&str>,
    tokens: &mut Vec<(u32, u32, u32, u32)>,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && !is_field_or_method(node) {
        let name = node_text(node, source);
        let byte_offset = node.start_byte();
        let is_local = scope_tree.resolve_decl(byte_offset, name).is_some();
        let modifiers = if is_local {
            0
        } else if builtins.contains(name) {
            TM_DEFAULT_LIBRARY
        } else {
            0
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
            collect_variable_tokens(cursor, source, scope_tree, builtins, tokens);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

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
