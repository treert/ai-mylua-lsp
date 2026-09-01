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
/// the name is answered by the environment (and for reference verification,
/// which re-resolves candidate occurrences one by one).
///
/// Returns `None` — meaning "treat this as an ordinary global" — in two cases:
///
/// 1. `_ENV` denotes the global environment (the ordinary path);
/// 2. the environment does not describe its field set *and* does not itself have
///    this field. Such an environment is assumed to route missing names to the
///    global table (§1.3), so falling through is the right answer.
///
/// A `Some` result therefore means "the environment answers this name", which is
/// also exactly the condition under which `undefinedGlobal` must stay quiet.
pub(crate) fn env_field_at(
    ident_node: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<BareName> {
    let name = node_text(ident_node, source);
    let env_fact = crate::type_inference::env_field_base_fact_in_scope(
        name,
        ident_node.start_byte(),
        scope_tree,
    )?;
    // An environment whose field set is not exhaustive still answers the fields
    // it *does* record — a name written inside the sandbox lives there and
    // nowhere else, so resolving it to a same-named global would be wrong.
    // Only names it does not have fall through.
    if !env_describes_its_fields(&env_fact, uri_id, index)
        && !env_records_field(&env_fact, name, uri_id, index)
    {
        return None;
    }
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

/// Whether the environment's own shape records `name` as a field.
///
/// Used to keep names *written inside* a non-exhaustive sandbox resolving to
/// that sandbox rather than falling through to a same-named global. Because
/// `summary_builder::visitors::env_binding_fact` guarantees a redirected `_ENV`
/// always has *some* shape, this is the lookup that makes those writes
/// reachable at all.
fn env_records_field(
    env_fact: &crate::type_system::TypeFact,
    name: &str,
    uri_id: UriId,
    index: &WorkspaceAggregation,
) -> bool {
    let resolved = resolver::resolve_type(uri_id, env_fact, index);
    let crate::type_system::TypeFact::Known(crate::type_system::KnownType::Table(shape_id)) =
        resolved.type_fact
    else {
        return false;
    };
    index
        .summary_by_id(resolved.source_uri_id())
        .and_then(|summary| summary.table_shapes.get(&shape_id))
        .is_some_and(|shape| shape.get_field(name).is_some())
}

/// Whether a redirected `_ENV`'s fact tells us what fields the environment has.
///
/// Two things must hold: the fact resolves to a definite table shape, **and**
/// that shape's field set is exhaustive (`TableShape::is_closed`).
///
/// Since `summary_builder::visitors::env_binding_fact` guarantees a redirected
/// `_ENV` always resolves to *some* shape, in practice this reduces to
/// `is_closed` — which is precisely the "no metatable, clean sandbox" case:
///
/// ```lua
/// local _ENV = {}                                  -- closed → true
/// local _ENV = setmetatable({}, { __index = _G })  -- synthesized, open → false
/// local t = {}; setmetatable(t, mt); _ENV = t      -- a shape, but open → false
/// local _ENV = f()                                 -- synthesized, open → false
/// ```
///
/// # What `false` means for the caller
///
/// Names the shape does not record fall through to the global namespace. This
/// implements the documented convention (§1.3): rather than tracking `__index`,
/// any environment carrying a metatable is **assumed** to be
/// `{ __index = _G }`, which is one of the two supported sandbox styles. Code
/// pointing `__index` elsewhere gets global answers anyway — deliberately, to
/// push users toward the two supported styles.
///
/// This is why the assumption is safe to also apply to *diagnostics*: under the
/// convention, a name missing from both the sandbox shape and the global index
/// is missing at run time too, so `undefinedGlobal` can report it. `env_field_at`
/// returning `None` is the single condition all consumers key off.
///
/// The **write** side is unaffected: `setmetatable`'s default `__newindex` does
/// not forward, so `x = 1` in such a sandbox writes the sandbox table, and
/// `summary_builder::type_infer::env_field_base_fact` keeps it out of the global
/// index.
fn env_describes_its_fields(
    env_fact: &crate::type_system::TypeFact,
    uri_id: UriId,
    index: &WorkspaceAggregation,
) -> bool {
    let resolved = resolver::resolve_type(uri_id, env_fact, index);
    let crate::type_system::TypeFact::Known(crate::type_system::KnownType::Table(shape_id)) =
        resolved.type_fact
    else {
        return false;
    };
    index
        .summary_by_id(resolved.source_uri_id())
        .and_then(|summary| summary.table_shapes.get(&shape_id))
        .is_some_and(|shape| shape.is_closed)
}

/// Whether the file binds `_ENV` to anything other than the global environment.
///
/// A file-level pre-check for capabilities whose alternative is a *per
/// occurrence* [`env_field_at`], which queries the index. With no redirecting
/// binding anywhere in the file, no name in it can be an environment field, so
/// the caller may keep its cheaper non-semantic path. `document_highlight` runs
/// on every cursor move, so that distinction is worth making.
///
/// Only a binding provably pointing at the global environment is *not* a
/// redirect: the implicit chunk-level declaration and an explicit
/// `local _ENV = _G`. A declaration carrying no fact counts as a redirect,
/// since nothing shows it is the global environment.
pub(crate) fn file_redirects_env(scope_tree: &ScopeTree) -> bool {
    scope_tree.all_declarations().any(|decl| {
        decl.name.as_str() == crate::lua_builtins::ENV_NAME
            && !decl
                .type_fact
                .as_ref()
                .is_some_and(crate::type_system::is_global_env_fact)
    })
}

/// Whether the free name `name` at `offset` is answered by a redirected `_ENV`
/// — i.e. whether it is *not* an occurrence of the same-named global.
///
/// Name-and-offset mirror of [`env_field_at`]'s gate, for callers that have no
/// AST node at hand (`references::verify_global`, `undefinedGlobal`). The two
/// **must** stay in agreement: if the cursor side treats a sandboxed name as a
/// global while the verification side excludes candidate sites inside that
/// sandbox (or vice versa), references silently splits or merges symbols — the
/// asymmetry §1.6 exists to prevent.
pub(crate) fn is_known_env_field(
    name: &str,
    offset: usize,
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> bool {
    crate::type_inference::env_field_base_fact_in_scope(name, offset, scope_tree).is_some_and(
        |env_fact| {
            env_describes_its_fields(&env_fact, uri_id, index)
                || env_records_field(&env_fact, name, uri_id, index)
        },
    )
}

/// What a redirected `_ENV` at `offset` offers as completion candidates.
///
/// `None` means "the environment is the global one" — the ordinary path, where
/// callers just offer the global namespace.
#[derive(Debug, Clone, Copy)]
pub(crate) enum EnvCompletionScope {
    /// The environment's field set is exhaustive (a clean, metatable-free
    /// sandbox). Only its own fields exist at run time, so offering globals
    /// would suggest names that `envUnknownField` immediately flags.
    OnlyEnvFields(crate::table_shape::TableShapeId, UriId),
    /// The field set is not exhaustive, so by the `{ __index = _G }` convention
    /// (§1.3) both its own fields and the global namespace are reachable.
    EnvFieldsAndGlobals(crate::table_shape::TableShapeId, UriId),
}

/// Completion scope for the environment in effect at `offset`.
///
/// Keyed off the same predicates as [`env_field_at`] / [`is_known_env_field`],
/// so completion cannot end up disagreeing with navigation and diagnostics:
/// whatever a name would resolve to is what gets offered.
///
/// The name passed to `env_field_base_fact_in_scope` is a placeholder — that
/// function only rejects the literal `_ENV`, and completion asks about the
/// environment as a whole rather than about one name.
pub(crate) fn env_completion_scope(
    offset: usize,
    uri_id: UriId,
    scope_tree: &ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<EnvCompletionScope> {
    let env_fact = crate::type_inference::env_field_base_fact_in_scope("", offset, scope_tree)?;
    let resolved = resolver::resolve_type(uri_id, &env_fact, index);
    let crate::type_system::TypeFact::Known(crate::type_system::KnownType::Table(shape_id)) =
        resolved.type_fact
    else {
        return None;
    };
    let owner = resolved.source_uri_id();
    let is_closed = index
        .summary_by_id(owner)
        .and_then(|summary| summary.table_shapes.get(&shape_id))
        .is_some_and(|shape| shape.is_closed);
    Some(if is_closed {
        EnvCompletionScope::OnlyEnvFields(shape_id, owner)
    } else {
        EnvCompletionScope::EnvFieldsAndGlobals(shape_id, owner)
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
