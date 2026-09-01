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
//!
//! Environment-aware: a free name `x` is `_ENV.x` (§1.3), so in a file
//! that redirects `_ENV` two occurrences of the same text can be
//! different variables at run time. Those files route the comparison
//! through `name_resolution` (§1.6) — the same layer goto / references
//! use — instead of matching text. Files that never rebind `_ENV`, the
//! overwhelming majority, skip that work entirely: this request fires on
//! every cursor move, so it must not pay for an index query per
//! occurrence without reason.

use crate::syntax_kind::{field, kind, NodeKindExt};
use tower_lsp_server::ls_types::*;

use crate::aggregation::WorkspaceAggregation;
use crate::document::Document;
use crate::name_resolution::{self, BareName};
use crate::resolver::ResolvedLocation;
use crate::scope::ScopeTree;
use crate::uri_id::UriId;
use crate::util::{find_node_at_position, is_ancestor_or_equal, node_text, LineIndex};

/// What the clicked *free* name denotes, when the file redirects `_ENV` and the
/// answer therefore cannot be read off the text alone.
#[derive(Debug, Clone, Copy)]
enum EnvTarget {
    /// A field of a redirected `_ENV`, identified by its definition site — the
    /// same identity `references::Identity::EnvField` uses.
    Field(ResolvedLocation),
    /// An ordinary global: either no redirection is in effect at the cursor, or
    /// the environment routes the name to the global table by the
    /// `{ __index = _G }` convention (§1.3).
    Global,
}

/// Everything the occurrence filter needs, so the recursive walk keeps a
/// readable signature.
struct MatchCtx<'a> {
    name: &'a str,
    source: &'a [u8],
    scope_tree: &'a ScopeTree,
    /// `Some` when the click resolved to a local: only occurrences resolving to
    /// the same declaration match.
    target_decl_byte: Option<usize>,
    /// `Some` when the clicked name is a free name whose meaning depends on the
    /// environment in effect. `None` keeps the plain text match.
    env_target: Option<EnvTarget>,
    uri_id: UriId,
    index: &'a WorkspaceAggregation,
    line_index: &'a LineIndex,
}

pub fn document_highlight(
    doc: &Document,
    uri_id: UriId,
    position: Position,
    index: &WorkspaceAggregation,
) -> Option<Vec<DocumentHighlight>> {
    let byte_offset = doc
        .line_index()
        .position_to_byte_offset(doc.source(), position)?;
    let clicked = find_node_at_position(doc.root_node()?, byte_offset)?;
    let source = doc.source();
    let name = node_text(clicked, source);
    if name.is_empty() {
        return None;
    }

    // Resolve the clicked identifier's declaration (if it's a local)
    // so we can distinguish shadowed bindings. `resolve_decl` gives
    // us `decl_byte` directly — avoid a Position round-trip that would
    // silently fail for any future non-ASCII identifier. Global /
    // Emmy-type names have no scope decl and fall back to the free-name
    // handling below.
    let target_decl_byte = doc
        .scope_tree
        .resolve_decl(byte_offset, name)
        .map(|d| d.decl_byte);

    let ctx = MatchCtx {
        name,
        source,
        scope_tree: &doc.scope_tree,
        target_decl_byte,
        env_target: match target_decl_byte {
            Some(_) => None,
            None => resolve_env_target(clicked, source, uri_id, &doc.scope_tree, index),
        },
        uri_id,
        index,
        line_index: doc.line_index(),
    };

    let mut highlights = Vec::new();
    let root = doc.root_node()?;
    let mut cursor = root.walk();
    collect_highlights(&mut cursor, &ctx, &mut highlights);
    // `TreeCursor` pre-order traversal visits each node once and in
    // source order, so the collected list is already sorted — no
    // sort/dedup needed.
    Some(highlights)
}

/// Classify the clicked free name, or `None` to stay on the text-matching path.
///
/// `None` is returned — deliberately, as the wider answer — whenever we have
/// nothing better to compare against:
///
/// - the file never rebinds `_ENV`, so no occurrence can be an environment
///   field and text matching is already correct;
/// - the cursor is not on an identifier, or is on one that is not a free name
///   (`a.b`'s `b`, a table key, a label). Such a token is not resolved by §1.6
///   at all; note that an environment field *defined* as a table key lands here,
///   which is why [`matches_env_target`] accepts the definition site explicitly
///   when coming from the other direction;
/// - the environment answers the name but cannot place it (an environment of
///   unknown shape). There is no definition site to key an identity off.
fn resolve_env_target(
    clicked: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<EnvTarget> {
    if !name_resolution::file_redirects_env(scope_tree) {
        return None;
    }
    if !clicked.is_kind(kind::IDENTIFIER) || crate::references::is_non_reference_position(clicked) {
        return None;
    }
    match name_resolution::env_field_at(clicked, source, uri_id, scope_tree, index) {
        Some(BareName::EnvField {
            location: Some(location),
            ..
        }) => Some(EnvTarget::Field(location)),
        Some(_) => None,
        None => Some(EnvTarget::Global),
    }
}

/// Whether `node` is an occurrence of the same symbol as the clicked free name.
fn matches_env_target(target: EnvTarget, node: tree_sitter::Node, ctx: &MatchCtx) -> bool {
    match target {
        EnvTarget::Field(location) => {
            // The definition site itself. It is often a table-constructor key,
            // which re-resolving would not classify as a free name at all.
            if location.uri_id == ctx.uri_id && location.range.start_byte == node.start_byte() {
                return true;
            }
            // Re-resolve at this occurrence's own offset — that is what
            // separates the pre- and post-redirect environments (§1.3).
            name_resolution::env_field_location(
                node,
                ctx.source,
                ctx.uri_id,
                ctx.scope_tree,
                ctx.index,
            ) == Some(location)
        }
        // Exactly the question `references` asks of its own candidate sites, so
        // the two capabilities cannot disagree about where a global occurs.
        EnvTarget::Global => crate::references::verify_global(
            node,
            ctx.name,
            ctx.uri_id,
            ctx.scope_tree,
            ctx.index,
        ),
    }
}

fn collect_highlights(
    cursor: &mut tree_sitter::TreeCursor,
    ctx: &MatchCtx,
    out: &mut Vec<DocumentHighlight>,
) {
    let node = cursor.node();
    if node.is_kind(kind::IDENTIFIER) && node_text(node, ctx.source) == ctx.name {
        let matches = match (ctx.target_decl_byte, ctx.env_target) {
            // Scope filter: the click resolved to a local, so only
            // occurrences pointing at the same declaration match.
            (Some(target), _) => ctx
                .scope_tree
                .resolve_decl(node.start_byte(), ctx.name)
                .is_some_and(|d| d.decl_byte == target),
            (None, Some(target)) => matches_env_target(target, node, ctx),
            (None, None) => true,
        };
        if matches {
            let kind = classify_kind(node);
            out.push(DocumentHighlight {
                range: ctx.line_index.ts_node_to_range(node, ctx.source),
                kind: Some(kind),
            });
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_highlights(cursor, ctx, out);
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
        match parent.syntax_kind() {
            // `variable` with field / subscript form: `object`
            // (required for nested access) or `index` (subscript form)
            // represents a READ of the current frame, regardless of
            // any outer assignment putting the whole thing on the LHS.
            kind::VARIABLE => {
                if let Some(obj) = parent.child_by_field(field::OBJECT) {
                    if obj.id() == current.id() {
                        return DocumentHighlightKind::READ;
                    }
                }
                if let Some(idx) = parent.child_by_field(field::INDEX) {
                    if idx.id() == current.id() {
                        return DocumentHighlightKind::READ;
                    }
                }
                // Bare-identifier form (`variable -> identifier`) or
                // we're the outer wrapper being looked at from below:
                // keep walking.
            }
            kind::ASSIGNMENT_STATEMENT => {
                if let Some(lhs) = parent.child_by_field(field::LEFT) {
                    if is_ancestor_or_equal(lhs, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            kind::LOCAL_DECLARATION => {
                if let Some(names) = parent.child_by_field(field::NAMES) {
                    if is_ancestor_or_equal(names, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            kind::LOCAL_FUNCTION_DECLARATION | kind::FUNCTION_DECLARATION => {
                if let Some(name) = parent.child_by_field(field::NAME) {
                    if is_ancestor_or_equal(name, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            kind::FOR_NUMERIC_STATEMENT => {
                if let Some(name) = parent.child_by_field(field::NAME) {
                    if is_ancestor_or_equal(name, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            kind::FOR_GENERIC_STATEMENT => {
                if let Some(names) = parent.child_by_field(field::NAMES) {
                    if is_ancestor_or_equal(names, ident) {
                        return DocumentHighlightKind::WRITE;
                    }
                }
                return DocumentHighlightKind::READ;
            }
            kind::PARAMETER_LIST => return DocumentHighlightKind::WRITE,
            _ => {}
        }
        current = parent;
    }
    DocumentHighlightKind::READ
}
