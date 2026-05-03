//! `textDocument/documentHighlight` implementation.
//!
//! Highlights every occurrence of the identifier at the cursor in the
//! current file. Read/Write classification walks each occurrence's
//! AST ancestors:
//!
//! - **Write** — identifier sits in a declaration slot (the name of a
//!   `local`/`function`/`local function` statement, a for-loop
//!   control variable, a function parameter) or on the left-hand
//!   side of an `assignment_statement`.
//! - **Read** — everything else (expression contexts, RHS of
//!   assignment, function-call argument, etc.).
//!
//! Scope-aware: when the clicked identifier resolves to a local
//! declaration, we only match occurrences that resolve to the *same*
//! declaration (so shadowing in nested scopes and the `local x = x + 1`
//! RHS referring to the outer `x` are handled correctly).

use tower_lsp_server::ls_types::*;

use crate::document::Document;
use crate::util::{find_node_at_position, is_ancestor_or_equal, node_text, LineIndex};

pub fn document_highlight(
    doc: &Document,
    _uri: &Uri,
    position: Position,
) -> Option<Vec<DocumentHighlight>> {
    let byte_offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;
    let clicked = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let source = doc.source();
    let name = node_text(clicked, source);
    if name.is_empty() {
        return None;
    }

    // Resolve the clicked identifier's declaration (if it's a local)
    // so we can distinguish shadowed bindings. `resolve_decl` gives
    // us `decl_byte` directly — avoid a Position round-trip that would
    // silently fail for any future non-ASCII identifier. Global /
    // Emmy-type names have no scope decl and fall back to plain text
    // matching.
    let target_decl_byte = doc
        .scope_tree
        .resolve_decl(byte_offset, name)
        .map(|d| d.decl_byte);

    let mut highlights = Vec::new();
    let root = doc.tree.root_node();
    let mut cursor = root.walk();
    collect_highlights(
        &mut cursor,
        name,
        source,
        &doc.scope_tree,
        target_decl_byte,
        doc.line_index(),
        &mut highlights,
    );
    // `TreeCursor` pre-order traversal visits each node once and in
    // source order, so the collected list is already sorted — no
    // sort/dedup needed.
    Some(highlights)
}

fn collect_highlights(
    cursor: &mut tree_sitter::TreeCursor,
    name: &str,
    source: &[u8],
    scope_tree: &crate::scope::ScopeTree,
    target_decl_byte: Option<usize>,
    line_index: &LineIndex,
    out: &mut Vec<DocumentHighlight>,
) {
    let node = cursor.node();
    if node.kind() == "identifier" && node_text(node, source) == name {
        // Scope filter: when the click resolved to a local, only
        // include occurrences that point to the same declaration.
        let matches_scope = match target_decl_byte {
            Some(target) => scope_tree
                .resolve_decl(node.start_byte(), name)
                .is_some_and(|d| d.decl_byte == target),
            None => true,
        };
        if matches_scope {
            let kind = classify_kind(node);
            out.push(DocumentHighlight {
                range: line_index.ts_node_to_range(node, source),
                kind: Some(kind),
            });
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_highlights(cursor, name, source, scope_tree, target_decl_byte, line_index, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Classify `ident` (an `identifier` node) as Read or Write based on
/// its AST ancestors.
///
/// **Subtlety for `a.b = 1` / `a[k] = v`**: the whole LHS sits in
/// `assignment_statement.left`, but only the *final* slot (`b` / the
/// table cell indexed by `k`) is actually written. The base `a` and
/// any subscript index `k` are READ for indexing. We detect this by
/// noticing when an ancestor `variable` node has an `object` or
/// `index` field that matches the current walk frame — that means the
/// identifier we came from is being read to compute the target, not
/// written to.
fn classify_kind(ident: tree_sitter::Node) -> DocumentHighlightKind {
    let mut current = ident;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            // `variable` with field / subscript form: `object`
            // (required for nested access) or `index` (subscript form)
            // represents a READ of the current frame, regardless of
            // any outer assignment putting the whole thing on the LHS.
            "variable" => {
                if let Some(obj) = parent.child_by_field_name("object") {
                    if obj.id() == current.id() {
                        return DocumentHighlightKind::READ;
                    }
                }
                if let Some(idx) = parent.child_by_field_name("index") {
                    if idx.id() == current.id() {
                        return DocumentHighlightKind::READ;
                    }
                }
                // Bare-identifier form (`variable -> identifier`) or
                // we're the outer wrapper being looked at from below:
                // keep walking.
            }
            "assignment_statement" => {
                if let Some(lhs) = parent.child_by_field_name("left") {
                    if is_ancestor_or_equal(lhs, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            "local_declaration" => {
                if let Some(names) = parent.child_by_field_name("names") {
                    if is_ancestor_or_equal(names, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            "local_function_declaration" | "function_declaration" => {
                if let Some(name) = parent.child_by_field_name("name") {
                    if is_ancestor_or_equal(name, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            "for_numeric_statement" => {
                if let Some(name) = parent.child_by_field_name("name") {
                    if is_ancestor_or_equal(name, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            "for_generic_statement" => {
                if let Some(names) = parent.child_by_field_name("names") {
                    if is_ancestor_or_equal(names, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            "parameter_list" => return DocumentHighlightKind::WRITE,
            _ => {}
        }
        current = parent;
    }
    DocumentHighlightKind::READ
}

