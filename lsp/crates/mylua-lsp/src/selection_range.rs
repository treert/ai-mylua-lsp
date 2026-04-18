//! `textDocument/selectionRange` — VS Code's "smart expand selection".
//!
//! For each input `Position`, returns a linked list of progressively
//! larger AST ranges starting at the smallest named descendant
//! containing the cursor and walking up to the root. The client
//! (e.g. VS Code with `⌃⇧→` / `Cmd+Shift+→`) cycles through these
//! ranges to grow the current selection along syntactic boundaries.
//!
//! We intentionally traverse only **named** nodes — unnamed tokens
//! like `(`, `,` and keyword terminals would produce many
//! single-byte links that feel jittery in the client. We also
//! deduplicate adjacent ranges (a parent that shares the child's
//! exact span is collapsed) so the returned chain is strictly
//! monotonic in span size.

use tower_lsp_server::ls_types::{Position, Range, SelectionRange};

use crate::document::Document;
use crate::util::{position_to_byte_offset, ts_node_to_range};

pub fn selection_range(doc: &Document, positions: &[Position]) -> Vec<SelectionRange> {
    let root = doc.tree.root_node();
    let source = doc.text.as_bytes();
    positions
        .iter()
        .filter_map(|pos| build_chain(root, source, &doc.text, *pos))
        .collect()
}

fn build_chain(
    root: tree_sitter::Node,
    source: &[u8],
    text: &str,
    position: Position,
) -> Option<SelectionRange> {
    let offset = position_to_byte_offset(text, position)?;
    let innermost = root.descendant_for_byte_range(offset, offset)?;

    // Collect ranges from innermost up to root, dedup adjacent
    // identical ranges, skip unnamed nodes.
    let mut ranges: Vec<Range> = Vec::new();
    let mut current = Some(innermost);
    let mut last: Option<Range> = None;
    while let Some(n) = current {
        if n.is_named() {
            let r = ts_node_to_range(n, source);
            if Some(r) != last {
                ranges.push(r);
                last = Some(r);
            }
        }
        current = n.parent();
    }

    if ranges.is_empty() {
        // descendant_for_byte_range landed on an unnamed token whose
        // parent chain is all unnamed (extremely rare — empty file
        // with a shebang, etc.). Fall back to the root range.
        return Some(SelectionRange {
            range: ts_node_to_range(root, source),
            parent: None,
        });
    }

    // Fold from outermost back to innermost so parent pointers thread
    // the right way.
    let mut chain: Option<Box<SelectionRange>> = None;
    for r in ranges.into_iter().rev() {
        chain = Some(Box::new(SelectionRange {
            range: r,
            parent: chain,
        }));
    }

    // The `SelectionRange` returned to the client is the *innermost*
    // node, with `.parent` chaining out to the root.
    chain.map(|b| *b)
}
