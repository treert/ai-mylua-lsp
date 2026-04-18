use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::scope::ScopeTree;
use crate::util::{node_text, ts_point_to_position};

const TT_VARIABLE: u32 = 0;
const TM_DEFAULT_LIBRARY: u32 = 1 << 0; // bit 0
const TM_GLOBAL: u32 = 1 << 1; // bit 1

// Built-in identifiers now come from `lua_builtins::builtins_for(version)`.
// `collect_semantic_tokens` and friends default to Lua 5.3 for backward
// compatibility; the `_with_version` variants let callers thread the
// configured runtime through.

pub fn semantic_tokens_legend() -> SemanticTokensLegend {
    SemanticTokensLegend {
        token_types: vec![SemanticTokenType::VARIABLE],
        token_modifiers: vec![
            SemanticTokenModifier::DEFAULT_LIBRARY,       // bit 0
            SemanticTokenModifier::new("global"),          // bit 1
        ],
    }
}

pub fn collect_semantic_tokens(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
) -> Vec<SemanticToken> {
    collect_semantic_tokens_with_version(root, source, scope_tree, "5.3")
}

pub fn collect_semantic_tokens_with_version(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    runtime_version: &str,
) -> Vec<SemanticToken> {
    collect_tokens_filtered(root, source, scope_tree, None, runtime_version)
}

/// `textDocument/semanticTokens/range` — return tokens that overlap
/// `range` only. The delta encoding is recomputed from (0, 0)
/// against the filtered set (LSP spec requires that), so clients
/// can apply the result independent of any earlier `full` result.
pub fn collect_semantic_tokens_range(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    range: Range,
) -> Vec<SemanticToken> {
    collect_semantic_tokens_range_with_version(root, source, scope_tree, range, "5.3")
}

pub fn collect_semantic_tokens_range_with_version(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    range: Range,
    runtime_version: &str,
) -> Vec<SemanticToken> {
    collect_tokens_filtered(root, source, scope_tree, Some(range), runtime_version)
}

fn collect_tokens_filtered(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    range: Option<Range>,
    runtime_version: &str,
) -> Vec<SemanticToken> {
    let builtins: HashSet<&str> = crate::lua_builtins::builtins_for(runtime_version)
        .into_iter()
        .collect();
    let mut raw: Vec<(u32, u32, u32, u32)> = Vec::new();
    let mut cursor = root.walk();
    collect_variable_tokens(&mut cursor, source, scope_tree, &builtins, &mut raw);

    // Line-based range filtering: keep tokens whose start line is
    // inside `range.start.line..=range.end.line` inclusive. Column
    // precision isn't necessary for semantic tokens — clients always
    // request full-line viewports in practice.
    if let Some(r) = range {
        raw.retain(|&(line, _col, _len, _mod)| {
            line >= r.start.line && line <= r.end.line
        });
    }

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
            TM_DEFAULT_LIBRARY | TM_GLOBAL
        } else {
            TM_GLOBAL
        };
        let start = node.start_position();
        let end = node.end_position();
        if start.row == end.row {
            // Convert tree-sitter byte columns to LSP UTF-16 code-unit
            // columns so non-ASCII lines (Chinese identifiers / comments
            // preceding the token) align correctly in the client.
            let start_pos = ts_point_to_position(start, source);
            let end_pos = ts_point_to_position(end, source);
            let length = end_pos.character.saturating_sub(start_pos.character);
            if length > 0 {
                tokens.push((start_pos.line, start_pos.character, length, modifiers));
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

// ---------------------------------------------------------------------------
// `textDocument/semanticTokens/full/delta` — edit-based diff against a
// previously-returned token stream.
// ---------------------------------------------------------------------------

/// Per-URI cache entry. The server keeps this around after each
/// `full` response so that a subsequent `delta` request with the
/// matching `previous_result_id` can compute a compact edit set.
#[derive(Debug, Clone)]
pub struct TokenCacheEntry {
    pub result_id: String,
    pub data: Vec<SemanticToken>,
}

/// Compute a minimal edit set bringing `old` to `new`. The returned
/// edits target the encoded `data` array in LSP semantics (each
/// `SemanticToken` occupies 5 consecutive `uinteger`s), so `start`
/// and `delete_count` are expressed in multiples of 5. We always
/// emit **at most one** edit — find the longest common prefix and
/// suffix, and the middle section becomes the single edit. This is
/// the simplest encoding that still respects the LSP contract and
/// provides meaningful bandwidth savings for typical edits.
///
/// When `old == new`, returns an empty Vec (no changes).
pub fn compute_semantic_token_delta(
    old: &[SemanticToken],
    new: &[SemanticToken],
) -> Vec<SemanticTokensEdit> {
    // Longest common prefix.
    let mut prefix = 0usize;
    while prefix < old.len() && prefix < new.len() && tokens_equal(&old[prefix], &new[prefix]) {
        prefix += 1;
    }
    // If the whole arrays match, no edits.
    if prefix == old.len() && prefix == new.len() {
        return Vec::new();
    }
    // Longest common suffix, bounded so we don't overlap the prefix.
    let mut suffix = 0usize;
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    while suffix < max_suffix
        && tokens_equal(
            &old[old.len() - 1 - suffix],
            &new[new.len() - 1 - suffix],
        )
    {
        suffix += 1;
    }

    let delete_tokens = old.len() - prefix - suffix;
    let insert: Vec<SemanticToken> = new[prefix..new.len() - suffix].to_vec();

    vec![SemanticTokensEdit {
        // LSP encodes start/delete_count in `uinteger`s; each token
        // is 5 uints (delta_line, delta_start, length, token_type,
        // token_modifiers).
        start: (prefix * 5) as u32,
        delete_count: (delete_tokens * 5) as u32,
        // tower-lsp-server's typed `SemanticTokensEdit.data` accepts
        // an optional `Vec<SemanticToken>` which serializes as 5×N
        // uints. `None` vs `Some(empty)` both mean "delete only".
        data: if insert.is_empty() { None } else { Some(insert) },
    }]
}

fn tokens_equal(a: &SemanticToken, b: &SemanticToken) -> bool {
    a.delta_line == b.delta_line
        && a.delta_start == b.delta_start
        && a.length == b.length
        && a.token_type == b.token_type
        && a.token_modifiers_bitset == b.token_modifiers_bitset
}

#[cfg(test)]
mod delta_tests {
    use super::*;

    fn tok(delta_line: u32, delta_start: u32, length: u32, modifiers: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type: 0,
            token_modifiers_bitset: modifiers,
        }
    }

    #[test]
    fn delta_identical_returns_empty() {
        let a = vec![tok(0, 0, 3, 0), tok(1, 2, 4, 1)];
        let b = a.clone();
        assert!(compute_semantic_token_delta(&a, &b).is_empty());
    }

    #[test]
    fn delta_append_is_single_edit() {
        let a = vec![tok(0, 0, 3, 0)];
        let b = vec![tok(0, 0, 3, 0), tok(1, 0, 4, 1)];
        let edits = compute_semantic_token_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5, "appending after 1 token starts at u32 index 5");
        assert_eq!(edits[0].delete_count, 0);
        assert_eq!(edits[0].data.as_ref().map(|v| v.len()).unwrap_or(0), 1);
    }

    #[test]
    fn delta_delete_is_zero_insert() {
        let a = vec![tok(0, 0, 3, 0), tok(1, 0, 4, 1)];
        let b = vec![tok(0, 0, 3, 0)];
        let edits = compute_semantic_token_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].start, 5);
        assert_eq!(edits[0].delete_count, 5, "one token = 5 u32s deleted");
        assert!(edits[0].data.is_none() || edits[0].data.as_ref().unwrap().is_empty());
    }

    #[test]
    fn delta_middle_change_prefix_suffix_preserved() {
        let a = vec![tok(0, 0, 3, 0), tok(1, 0, 4, 0), tok(1, 0, 5, 0)];
        let b = vec![tok(0, 0, 3, 0), tok(1, 0, 9, 9), tok(1, 0, 5, 0)];
        let edits = compute_semantic_token_delta(&a, &b);
        assert_eq!(edits.len(), 1);
        // prefix = 1 token, suffix = 1 token → edit covers middle.
        assert_eq!(edits[0].start, 5, "prefix 1 token × 5 u32");
        assert_eq!(edits[0].delete_count, 5, "middle 1 token × 5 u32");
        let inserted = edits[0].data.as_ref().unwrap();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].length, 9);
    }
}
