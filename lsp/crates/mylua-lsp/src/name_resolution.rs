//! §1.6 name resolution — the single implementation of "what does this bare
//! identifier denote", shared by `goto`, `hover` and `references`.
//!
//! # Why this module exists
//!
//! `docs/lsp-semantic-spec.md` §1.6 defines one resolution order for bare
//! names. It used to be aspirational: `goto`, `hover` and `references` each
//! re-implemented that order inline, so a rule added to one silently failed in
//! the others. `_ENV` redirection was exactly that failure mode — `goto`
//! learned that a sandboxed free name is a field of the environment table,
//! `hover` returned nothing at all, and `references` fell back to whole-file
//! text matching that could not tell the pre- and post-redirect `g` apart
//! (contradicting a guarantee §1.3 had documented for a long time).
//!
//! [`resolve_bare_name`] is now that single implementation. Adding a rule here
//! reaches all three capabilities at once.
//!
//! # Scope: *bare* names only
//!
//! This layer covers the unqualified case — steps 1/3/4 of §1.6 plus the `_ENV`
//! redirection between them. Deliberately **not** included:
//!
//! - **dotted / method field access** (`a.b.c`, `obj:m()`): already shared, via
//!   `resolver::resolve_field_chain`. What differs between capabilities there
//!   is presentation, not resolution.
//! - **`require` bindings and `goto` labels**: position-specialized branches
//!   that exist in `goto` only. Folding them in would mean modelling
//!   capability-specific concerns in a shared type for no gain.
//!
//! # The resolution order
//!
//! 1. **lexical scope** — a visible `local` / parameter wins over everything.
//!    Includes the implicit `_ENV` declaration (§1.3), so `_ENV` itself lands
//!    here.
//! 2. **field of a redirected `_ENV`** — a free name `x` is `_ENV.x`; when
//!    `_ENV` points somewhere other than the global environment, `x` is a field
//!    of *that* table and must not be looked up as a global. Keyed off
//!    `type_inference::env_field_base_fact_in_scope`, the single query-side
//!    entry point for the free-name rules.
//! 3. **Emmy type name** — `---@class` / `---@alias` names.
//! 4. **global** — the fallback, and the only step that can produce multiple
//!    candidates (hence `GotoStrategy` / `ReferencesStrategy`).

use crate::aggregation::WorkspaceAggregation;
use crate::resolver::{self, ResolvedLocation};
use crate::scope::ScopeTree;
use crate::types::Definition;
use crate::uri_id::UriId;
use crate::util::node_text;

/// What a bare identifier at the cursor denotes.
///
/// Ordered as resolved: earlier variants shadow later ones.
#[derive(Debug, Clone)]
pub(crate) enum BareName {
    /// A visible `local` / parameter (including the implicit `_ENV`).
    Local {
        def: Definition,
        decl_byte: usize,
    },
    /// A field of a redirected `_ENV` — sandboxed code.
    ///
    /// `location` is the field's definition site when the environment's shape
    /// records one. It is `None` for a field we cannot place (an environment of
    /// unknown shape, or a name absent from a known one); callers must then stay
    /// silent rather than fall through to the global namespace, since the name
    /// is definitively *not* a global.
    EnvField {
        name: String,
        location: Option<ResolvedLocation>,
        type_fact: crate::type_system::TypeFact,
    },
    /// An Emmy type name.
    TypeName { name: String },
    /// An ordinary global.
    Global { name: String },
}

/// Resolve the bare identifier at `ident_node` per §1.6.
///
/// `ident_node` must be an `identifier` in a bare (unqualified) position;
/// callers handle dotted / method positions before reaching here.
pub(crate) fn resolve_bare_name(
    ident_node: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> BareName {
    let name = node_text(ident_node, source);
    let byte_offset = ident_node.start_byte();

    // 1. Lexical scope.
    if let Some(decl_byte) = scope_tree
        .resolve_decl(byte_offset, name)
        .map(|decl| decl.decl_byte)
    {
        if let Some(def) = scope_tree.resolve_id(byte_offset, name, uri_id) {
            return BareName::Local { def, decl_byte };
        }
    }

    // 2. Field of a redirected `_ENV`.
    if let Some(target) = env_field_at(ident_node, source, uri_id, scope_tree, index) {
        return target;
    }

    // 3. Emmy type name.
    if index.contains_type(name) {
        return BareName::TypeName {
            name: name.to_string(),
        };
    }

    // 4. Global.
    BareName::Global {
        name: name.to_string(),
    }
}

/// The `EnvField` case on its own, for callers that only need to know whether
/// the environment is redirected here (and for reference verification, which
/// re-resolves candidate occurrences one by one).
///
/// Returns `None` when `_ENV` denotes the global environment — the ordinary
/// path — so a `Some` result always means "this name is not a global".
pub(crate) fn env_field_at(
    ident_node: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<BareName> {
    let name = node_text(ident_node, source);
    crate::type_inference::env_field_base_fact_in_scope(name, ident_node.start_byte(), scope_tree)?;
    let fact = crate::type_inference::infer_node_type_in_file_id(
        ident_node, source, uri_id, scope_tree, index,
    );
    let resolved = resolver::resolve_type(uri_id, &fact, index);
    Some(BareName::EnvField {
        name: name.to_string(),
        location: resolved.def_location,
        type_fact: resolved.type_fact,
    })
}

/// Definition site of the redirected-environment field at `ident_node`, if any.
///
/// Used by `references` to check whether a candidate occurrence denotes the
/// *same* environment field as the clicked one. Because the fact is rebuilt at
/// each occurrence's own byte offset, the position sensitivity of
/// `_ENV = expr` (§1.3) carries over for free: `g` before the write resolves
/// against the old environment and `g` after it against the new one, so the two
/// yield different locations and never merge.
pub(crate) fn env_field_location(
    ident_node: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<ResolvedLocation> {
    match env_field_at(ident_node, source, uri_id, scope_tree, index)? {
        BareName::EnvField { location, .. } => location,
        _ => None,
    }
}
