//! `envUnknownField` — reading a field the *redirected* `_ENV` does not have.
//!
//! In Lua 5.2+ a free name `x` is `_ENV.x`. Once `_ENV` points somewhere other
//! than the global environment (`_ENV = {}`, `local _ENV = t`, `function f(_ENV)`)
//! a free name is a field of *that* table, so `undefinedGlobal` no longer
//! applies — but reading a field the table does not have yields `nil`, which is
//! almost always a bug. This module reports that case.
//!
//! # Why this check is position-sensitive and `luaFieldWarning` is not
//!
//! For an ordinary table, `local M = {}; print(M.a); M.a = 1` is not flagged:
//! the shape is a whole-file summary with no ordering. That stays unchanged.
//! Here we can do better because the environment of a *chunk* has a
//! distinguished, linear execution flow: statements at the top level of the
//! file run exactly once, in source order. `g1 = 321` therefore genuinely
//! happens after `print(g1)` two lines above it.
//!
//! That reasoning collapses the moment a function boundary is crossed:
//!
//! ```lua
//! local _ENV = {}
//! function f() print(gg) end   -- byte position earlier …
//! gg = 1                       -- … than the write
//! ```
//!
//! `f`'s call time has nothing to do with its definition site, so comparing
//! byte offsets would be a false positive. The same applies to a write inside a
//! top-level `if` / `while` body, which is a flow-sensitivity question we do
//! not attempt to answer.
//!
//! The check is therefore fenced in on both sides:
//! - the **read** must sit on the chunk's straight-line execution flow;
//! - **every** write of that field must sit on that same flow, otherwise the
//!   field is treated as defined and nothing is reported.
//!
//! "Straight-line flow" means the chunk's top-level scope *or* a plain
//! `do … end` block nested in it: such a block adds a lexical scope but no
//! control flow, so its statements still run exactly once in source order.
//! Scoping a sandbox with `do local _ENV = … end` is the idiomatic spelling,
//! and excluding it left that whole shape unchecked. See
//! `ScopeTree::is_on_chunk_straight_line`.
//!
//! # Why no exemption for built-ins
//!
//! After `_ENV = {}` the stdlib genuinely is unreachable by its bare name —
//! that is precisely why the canonical sandbox opens with
//! `local print = print`. So when the environment's shape is *fully known*
//! there is nothing speculative about reporting `print`, and an exemption
//! would only hide a real bug.
//!
//! The idiom that makes built-ins reachable is
//! `setmetatable({}, { __index = _G })`, where every missing field falls
//! through to the real global environment. We do not follow `__index`, so
//! rather than exempting a name list we treat *attaching a metatable* as what
//! it is: the table's static field set stops being an exhaustive description
//! of the environment, and the whole check goes silent for it. That fact is
//! recorded on the shape itself by
//! `summary_builder::mark_shapes_opened_by_metatable_calls`, so the
//! `!is_closed` early return in `check_one_read` covers it — same category as
//! a dynamic-key write, and the same single fact that makes navigation fall
//! back to the global namespace (`name_resolution::env_describes_its_fields`).
//!
//! # Known limitations (deliberate silence)
//!
//! - `_ENV` of unknown type (`local _ENV = f()`) — nothing is known.
//! - a dynamic-key write (`_ENV[k] = v`) — the shape stops being exhaustive.
//! - `setmetatable` / `rawset` applied to the environment table — likewise.
//! - `load(chunk, name, mode, env)` / `debug.setupvalue` /
//!   `debug.setmetatable` — untracked.
//! - fields only ever written from inside a function body — see above.

use crate::aggregation::WorkspaceAggregation;
use crate::resolver;
use crate::scope::ScopeTree;
use crate::syntax_kind::{field, kind, NodeKindExt};
use crate::table_shape::TableShapeId;
use crate::type_system::{KnownType, TypeFact};
use crate::uri_id::UriId;
use crate::util::{node_text, LineIndex};
use std::collections::{HashMap, HashSet};
use tower_lsp_server::ls_types::*;

/// Identity of one environment table. `TableShapeId` is only unique per file,
/// so the owning file is part of the key (a `local _ENV = require("m")` can
/// legitimately point at another file's table).
type ShapeKey = (UriId, TableShapeId);

/// Where a field of a redirected `_ENV` gets written.
#[derive(Default)]
struct FieldWrites {
    /// Earliest write sitting on the chunk's straight-line execution flow.
    first_top_level: Option<usize>,
    /// A write was seen somewhere whose execution order relative to a read's
    /// byte position is unknowable — inside a function body, or inside a
    /// conditional / loop block such as an `if` branch. Any such write disables
    /// the positional judgement for this field.
    has_indirect: bool,
}

impl FieldWrites {
    fn record(&mut self, offset: usize, top_level: bool) {
        if top_level {
            self.first_top_level = Some(match self.first_top_level {
                Some(existing) => existing.min(offset),
                None => offset,
            });
        } else {
            self.has_indirect = true;
        }
    }
}

struct EnvCtx<'a> {
    source: &'a [u8],
    line_index: &'a LineIndex,
    uri_id: UriId,
    scope_tree: &'a ScopeTree,
    index: &'a WorkspaceAggregation,
    severity: DiagnosticSeverity,
    /// `(env shape, field name) → write sites`.
    writes: HashMap<(ShapeKey, String), FieldWrites>,
    /// Environments made non-exhaustive by an `_ENV[k] = v` write, whose key
    /// cannot be named statically.
    ///
    /// Narrower than it looks: metatables and `rawset` are *not* recorded here
    /// but on the shape itself (`TableShape::is_closed`, set by
    /// `summary_builder::mark_shapes_opened_by_metatable_calls`), and reach
    /// this module through the `!is_closed` early return in `check_one_read`.
    /// Only the dynamic-key case stays local, because it is a statement about
    /// this specific `_ENV` binding rather than about the table.
    poisoned: HashSet<ShapeKey>,
}

pub(super) fn check_env_field_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri_id: UriId,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let mut ctx = EnvCtx {
        source,
        line_index,
        uri_id,
        scope_tree,
        index,
        severity,
        writes: HashMap::new(),
        poisoned: HashSet::new(),
    };
    // Two passes: the read check needs to know about writes that appear later
    // in the file, so every write site must be collected up front.
    collect_writes(&mut ctx, root);
    check_reads(&ctx, root, diagnostics);
}

// ---------------------------------------------------------------------------
// Environment identity
// ---------------------------------------------------------------------------

/// The table `_ENV` points at, at `offset`, when its shape is fully known.
/// `None` for the global environment, an unknown type, or a non-table.
fn env_shape_at(ctx: &EnvCtx, offset: usize) -> Option<ShapeKey> {
    let fact = ctx
        .scope_tree
        .resolve_type(offset, crate::lua_builtins::ENV_NAME)?;
    let resolved = resolver::resolve_type(ctx.uri_id, fact, ctx.index);
    match resolved.type_fact {
        TypeFact::Known(KnownType::Table(shape_id)) => Some((resolved.source_uri_id(), shape_id)),
        _ => None,
    }
}

/// Same, but gated on the environment actually being *redirected*. Keying off
/// `env_field_base_fact_in_scope` — the single query-side entry point for the
/// free-name rules — keeps this module from re-deciding what counts as the
/// global environment.
fn redirected_env_shape_at(ctx: &EnvCtx, name: &str, offset: usize) -> Option<ShapeKey> {
    crate::type_inference::env_field_base_fact_in_scope(name, offset, ctx.scope_tree)?;
    env_shape_at(ctx, offset)
}

/// True if `offset` sits on the chunk's straight-line execution flow — the file
/// scope itself, or nested only inside plain `do … end` blocks.
///
/// A `do … end` block is included because it adds a lexical scope but no
/// control flow: its statements still run exactly once, in source order. That
/// matters in practice, because scoping a sandbox with
/// `do local _ENV = … end` is the idiomatic spelling. Function bodies,
/// conditional branches and loop bodies stay excluded — see
/// `ScopeTree::is_on_chunk_straight_line`.
fn is_top_level(ctx: &EnvCtx, offset: usize) -> bool {
    ctx.scope_tree.is_on_chunk_straight_line(offset)
}

// ---------------------------------------------------------------------------
// Pass 1 — collect writes
// ---------------------------------------------------------------------------

fn collect_writes(ctx: &mut EnvCtx, node: tree_sitter::Node) {
    match node.syntax_kind() {
        kind::ASSIGNMENT_STATEMENT => collect_assignment_writes(ctx, node),
        // `function foo() end` under a redirected `_ENV` writes `foo` into the
        // new environment, just like the assignment spelling. Both spellings
        // must be collected here: the shape alone cannot serve this check,
        // because `FieldInfo.def_range` is overwritten by `set_field` and ends
        // up holding the *last* write, whereas we need the first — and we need
        // to know whether each write sits on the top-level straight-line flow.
        kind::FUNCTION_DECLARATION => collect_function_declaration_write(ctx, node),
        _ => {}
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_writes(ctx, child);
        }
    }
}

fn collect_assignment_writes(ctx: &mut EnvCtx, node: tree_sitter::Node) {
    let Some(left) = node.child_by_field(field::LEFT) else {
        return;
    };
    for i in 0..left.named_child_count() {
        let Some(var) = left.named_child(i as u32) else {
            continue;
        };
        if !var.is_kind(kind::VARIABLE) {
            continue;
        }
        // `_ENV[k] = v` — a field we cannot name statically.
        if let (Some(object), Some(_index)) = (
            var.child_by_field(field::OBJECT),
            var.child_by_field(field::INDEX),
        ) {
            if node_text(object, ctx.source) == crate::lua_builtins::ENV_NAME {
                if let Some(key) = env_shape_at(ctx, var.start_byte()) {
                    ctx.poisoned.insert(key);
                }
            }
            continue;
        }
        // `_ENV.name = v` — an explicit field write on the environment.
        if let (Some(object), Some(field_node)) = (
            var.child_by_field(field::OBJECT),
            var.child_by_field(field::FIELD),
        ) {
            if node_text(object, ctx.source) == crate::lua_builtins::ENV_NAME {
                let name = node_text(field_node, ctx.source).to_string();
                if let Some(key) = env_shape_at(ctx, var.start_byte()) {
                    record_write(ctx, key, name, var.start_byte());
                }
            }
            continue;
        }
        // Bare `name = v` — sugar for `_ENV.name = v`.
        if var.child_count() != 1 {
            continue;
        }
        let name = node_text(var, ctx.source).to_string();
        let offset = var.start_byte();
        if ctx.scope_tree.resolve_decl(offset, &name).is_some() {
            continue; // a visible local, not an environment field
        }
        if let Some(key) = redirected_env_shape_at(ctx, &name, offset) {
            record_write(ctx, key, name, offset);
        }
    }
}

fn collect_function_declaration_write(ctx: &mut EnvCtx, node: tree_sitter::Node) {
    let Some(name_node) = node.child_by_field(field::NAME) else {
        return;
    };
    // Only the bare form defines a new name. `function foo.bar()` writes a
    // field of `foo` and *reads* `foo`, which the read pass handles.
    if name_node.named_child_count() != 1 {
        return;
    }
    let Some(ident) = name_node.named_child(0) else {
        return;
    };
    if !ident.is_kind(kind::IDENTIFIER) {
        return;
    }
    let name = node_text(ident, ctx.source).to_string();
    let offset = ident.start_byte();
    if ctx.scope_tree.resolve_decl(offset, &name).is_some() {
        return; // assigns an existing visible local
    }
    if let Some(key) = redirected_env_shape_at(ctx, &name, offset) {
        record_write(ctx, key, name, offset);
    }
}

fn record_write(ctx: &mut EnvCtx, key: ShapeKey, name: String, offset: usize) {
    let top_level = is_top_level(ctx, offset);
    ctx.writes
        .entry((key, name))
        .or_default()
        .record(offset, top_level);
}

// ---------------------------------------------------------------------------
// Pass 2 — check reads
// ---------------------------------------------------------------------------

fn check_reads(ctx: &EnvCtx, node: tree_sitter::Node, diagnostics: &mut Vec<Diagnostic>) {
    if node.is_kind(kind::IDENTIFIER)
        && super::free_name::is_free_name_reference(node)
        && !super::free_name::is_assignment_target(node)
    {
        check_one_read(ctx, node, diagnostics);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            check_reads(ctx, child, diagnostics);
        }
    }
}

fn check_one_read(ctx: &EnvCtx, node: tree_sitter::Node, diagnostics: &mut Vec<Diagnostic>) {
    let name = node_text(node, ctx.source);
    let offset = node.start_byte();

    if ctx.scope_tree.resolve_decl(offset, name).is_some() {
        return; // a visible local — the environment is not involved
    }
    if !is_top_level(ctx, offset) {
        return; // positional judgement is unsound past this point
    }
    let Some(key) = redirected_env_shape_at(ctx, name, offset) else {
        return;
    };
    if ctx.poisoned.contains(&key) {
        return;
    }
    let (owner_uri_id, shape_id) = key;
    let Some(shape) = ctx
        .index
        .summary_by_id(owner_uri_id)
        .and_then(|summary| summary.table_shapes.get(&shape_id))
    else {
        return;
    };
    if !shape.is_closed {
        return; // a dynamic key made the field set non-exhaustive
    }

    let in_shape = shape.get_field(name).is_some();
    let message = match ctx.writes.get(&(key, name.to_string())) {
        // Any write we cannot order against this read forfeits the check.
        Some(writes) if writes.has_indirect => return,
        Some(writes) => match writes.first_top_level {
            Some(first) if offset < first => format!(
                "'{}' is read before it is assigned in the current _ENV",
                name
            ),
            _ => return,
        },
        // No write we model. If the shape has the field anyway it came from
        // somewhere unordered (the environment's own literal, a nested
        // constructor, a method declaration) — treat it as always present.
        None if in_shape => return,
        None => format!("'{}' is not a field of the current _ENV", name),
    };

    diagnostics.push(Diagnostic {
        range: ctx.line_index.ts_node_to_range(node, ctx.source),
        severity: Some(ctx.severity),
        source: Some("mylua".to_string()),
        message,
        ..Default::default()
    });
}
