//! `textDocument/foldingRange` implementation.
//!
//! Walks the tree-sitter tree and emits one `FoldingRange` per
//! foldable construct:
//!
//! - **Region** folds for block-structured nodes: `function ... end`,
//!   `local function ... end`, `function() ... end`, `do ... end`,
//!   `while ... do ... end`, `for ... do ... end`,
//!   `repeat ... until ...`, `if ... [elseif...][else...] end`, and
//!   multi-line table constructors `{ ... }`.
//! - **Comment** folds for multi-line block comments (`--[[ ... ]]`,
//!   `--[=[ ... ]=]`, ...) and for runs of adjacent `---@tag` lines.
//!   The grammar rule `emmy_comment: prec.left(repeat1($.emmy_line))`
//!   merges consecutive `---` lines into a single `emmy_comment` node
//!   spanning multiple rows, but the aggregation pass below still
//!   collects row-by-row and merges adjacent rows. That way the
//!   implementation also handles the "one emmy_comment per line"
//!   shape (e.g. if the grammar later changes or if two `---` runs
//!   are separated by a gap line).
//!
//! End-line convention:
//!
//! - Block-structured Region folds use `end_line = end_row - 1` so
//!   the closing keyword (`end` / `until` / `}`) remains visible when
//!   the range is folded.
//! - Comment folds use `end_line = end_row` so the whole block
//!   (including the last comment line) collapses into a single line.
//!
//! Single-line constructs are skipped; block folds require at least
//! one row of body between opener and closer.

use tower_lsp_server::ls_types::{FoldingRange, FoldingRangeKind};

use crate::document::Document;

pub fn folding_range(doc: &Document) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    let source = doc.source();
    let root = doc.tree.root_node();

    collect_block_and_block_comment_folds(root, source, &mut out);
    collect_emmy_comment_runs(root, &mut out);

    out
}

/// First pass: walk every node emitting folds for block-structured
/// constructs and for multi-line `--[[ ... ]]` block comments. Single
/// `emmy_comment` nodes are skipped here — they're handled by the
/// second pass which aggregates consecutive lines.
fn collect_block_and_block_comment_folds(
    root: tree_sitter::Node,
    source: &[u8],
    out: &mut Vec<FoldingRange>,
) {
    let mut cursor = root.walk();
    walk_for_blocks(root, source, &mut cursor, out);
}

fn walk_for_blocks<'t>(
    node: tree_sitter::Node<'t>,
    source: &[u8],
    cursor: &mut tree_sitter::TreeCursor<'t>,
    out: &mut Vec<FoldingRange>,
) {
    if let Some(fold) = fold_for_block_node(node, source) {
        out.push(fold);
    }
    // Per-branch folds for `if ... elseif ... else ... end`. The
    // outer `if_statement` fold (emitted above) covers the whole
    // construct; this pass adds a dedicated fold for the leading
    // `if` branch so each branch has its own collapse affordance
    // alongside `elseif_clause` / `else_clause` folds which match
    // below as their own kinds.
    if node.kind() == "if_statement" {
        if let Some(fold) = fold_for_if_branch(node) {
            out.push(fold);
        }
    }
    if cursor.goto_first_child() {
        loop {
            walk_for_blocks(cursor.node(), source, cursor, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Emit a fold covering just the `if cond then ... body` portion
/// of an `if_statement` — i.e. from the `if` line up to the row
/// before the first `elseif`/`else` clause. Returns `None` when the
/// `if_statement` has no clauses (no branching to fold independently
/// from the outer block) or when the if-branch body is a single line.
fn fold_for_if_branch(if_stmt: tree_sitter::Node) -> Option<FoldingRange> {
    let start_row = if_stmt.start_position().row as u32;
    // First elseif/else clause (in source order). They live directly
    // on the `if_statement` node.
    let mut first_clause_row: Option<u32> = None;
    for i in 0..if_stmt.named_child_count() {
        let Some(child) = if_stmt.named_child(i as u32) else { continue };
        if matches!(child.kind(), "elseif_clause" | "else_clause") {
            first_clause_row = Some(child.start_position().row as u32);
            break;
        }
    }
    let end_row = first_clause_row?;
    if end_row <= start_row + 1 {
        return None; // single-line if-branch
    }
    Some(FoldingRange {
        start_line: start_row,
        end_line: end_row - 1,
        start_character: None,
        end_character: None,
        kind: Some(FoldingRangeKind::Region),
        collapsed_text: None,
    })
}

fn fold_for_block_node(node: tree_sitter::Node, source: &[u8]) -> Option<FoldingRange> {
    let start_row = node.start_position().row as u32;
    let end_row = node.end_position().row as u32;

    match node.kind() {
        "function_declaration"
        | "local_function_declaration"
        | "function_definition"
        | "do_statement"
        | "while_statement"
        | "repeat_statement"
        | "if_statement"
        | "for_numeric_statement"
        | "for_generic_statement"
        | "table_constructor" => {
            if end_row > start_row + 1 {
                Some(FoldingRange {
                    start_line: start_row,
                    end_line: end_row - 1,
                    start_character: None,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: None,
                })
            } else {
                None
            }
        }

        // Per-branch folds for `elseif` / `else`. Tree-sitter ends
        // the clause node at its last statement, **not** at the
        // next sibling / `end` keyword. A naive `end_row - 1` would
        // leave the last body row visible when folded. Extend the
        // fold to the row **before** the next sibling (either
        // another clause or the `end` keyword) so every body line
        // of this branch collapses and only the clause opener stays
        // visible.
        "elseif_clause" | "else_clause" => {
            let next_row = node
                .next_sibling()
                .map(|n| n.start_position().row as u32)
                .unwrap_or(end_row + 1);
            if next_row > start_row + 1 {
                Some(FoldingRange {
                    start_line: start_row,
                    end_line: next_row - 1,
                    start_character: None,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Region),
                    collapsed_text: None,
                })
            } else {
                None
            }
        }

        "comment" => {
            if end_row > start_row && is_block_comment(node, source) {
                Some(FoldingRange {
                    start_line: start_row,
                    end_line: end_row,
                    start_character: None,
                    end_character: None,
                    kind: Some(FoldingRangeKind::Comment),
                    collapsed_text: None,
                })
            } else {
                None
            }
        }

        _ => None,
    }
}

/// Second pass: collect every `emmy_comment` node (the grammar emits
/// one per `---@tag` line) and group consecutive rows into a single
/// Comment fold. Skips singletons.
fn collect_emmy_comment_runs(root: tree_sitter::Node, out: &mut Vec<FoldingRange>) {
    let mut rows: Vec<u32> = Vec::new();
    let mut cursor = root.walk();
    collect_emmy_rows(root, &mut cursor, &mut rows);

    // Each `emmy_comment` in the current grammar wraps exactly one
    // `emmy_line` and spans a single row — so we work with row numbers
    // directly, sort, and merge adjacent ones.
    rows.sort_unstable();
    rows.dedup();

    let mut i = 0;
    while i < rows.len() {
        let mut j = i;
        while j + 1 < rows.len() && rows[j + 1] == rows[j] + 1 {
            j += 1;
        }
        if j > i {
            out.push(FoldingRange {
                start_line: rows[i],
                end_line: rows[j],
                start_character: None,
                end_character: None,
                kind: Some(FoldingRangeKind::Comment),
                collapsed_text: None,
            });
        }
        i = j + 1;
    }
}

fn collect_emmy_rows<'t>(
    node: tree_sitter::Node<'t>,
    cursor: &mut tree_sitter::TreeCursor<'t>,
    rows: &mut Vec<u32>,
) {
    if node.kind() == "emmy_comment" {
        let sr = node.start_position().row as u32;
        let er = node.end_position().row as u32;
        for r in sr..=er {
            rows.push(r);
        }
        // No point in descending into `emmy_line` children — they
        // carry no extra rows beyond what the parent already covers.
        return;
    }
    if cursor.goto_first_child() {
        loop {
            collect_emmy_rows(cursor.node(), cursor, rows);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// True when the `comment` node's first few bytes open a long-bracket
/// block comment (`--[[` or `--[=[`, `--[==[`, ...).
fn is_block_comment(node: tree_sitter::Node, source: &[u8]) -> bool {
    let start = node.start_byte();
    let end = node.end_byte().min(source.len());
    if end <= start {
        return false;
    }
    is_long_bracket_open_prefix(&source[start..end])
}

/// True when `bytes` starts with `--[` optionally followed by any
/// number of `=` then `[` (Lua long-bracket block comment opener).
/// Exposed `pub(crate)` so the tests below exercise the same code
/// the runtime uses, avoiding a mirror implementation.
pub(crate) fn is_long_bracket_open_prefix(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || !bytes.starts_with(b"--[") {
        return false;
    }
    let mut i = 3;
    while i < bytes.len() && bytes[i] == b'=' {
        i += 1;
    }
    i < bytes.len() && bytes[i] == b'['
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_comment_prefix_detection() {
        assert!(is_long_bracket_open_prefix(b"--[[ multi\nline ]]"));
        assert!(is_long_bracket_open_prefix(b"--[==[ nested\nwith ==] ]==]"));
        assert!(!is_long_bracket_open_prefix(b"-- single line"));
        assert!(!is_long_bracket_open_prefix(b"local x = 1"));
        assert!(!is_long_bracket_open_prefix(b"--"));
        assert!(!is_long_bracket_open_prefix(b"--["));
        // Mismatched — `--[=` without second `[` is not a block comment.
        assert!(!is_long_bracket_open_prefix(b"--[="));
    }
}
