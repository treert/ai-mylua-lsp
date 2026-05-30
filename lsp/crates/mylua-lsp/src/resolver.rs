use std::collections::HashSet;

use crate::aggregation::WorkspaceAggregation;
use crate::lua_symbol::LuaSymbol;
use crate::table_shape::TableShapeId;
use crate::type_system::*;
use crate::uri_id::UriId;
use crate::util::ByteRange;

const MAX_RESOLVE_DEPTH: usize = 32;

/// Result of resolving a type, with optional source location for goto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLocation {
    pub uri_id: UriId,
    pub range: ByteRange,
}

#[derive(Debug, Clone)]
pub struct ResolvedType {
    pub type_fact: TypeFact,
    /// Definition location for protocol-facing consumers that need a range.
    pub def_location: Option<ResolvedLocation>,
    /// Owning file identity for per-file values such as `TableShapeId` and
    /// `FunctionSummaryId`. This can be present even when no definition range
    /// exists, for example a module return table.
    pub owner_uri_id: UriId,
}

impl ResolvedType {
    fn unknown(ctx: ResolveCtx) -> Self {
        Self {
            type_fact: TypeFact::Unknown,
            def_location: None,
            owner_uri_id: ctx.owner_uri_id,
        }
    }

    fn from_fact(ctx: ResolveCtx, fact: TypeFact) -> Self {
        Self {
            type_fact: fact,
            def_location: None,
            owner_uri_id: ctx.owner_uri_id,
        }
    }

    fn with_location(fact: TypeFact, uri_id: UriId, range: ByteRange) -> Self {
        Self {
            type_fact: fact,
            def_location: Some(ResolvedLocation { uri_id, range }),
            owner_uri_id: uri_id,
        }
    }

    pub fn source_uri_id(&self) -> UriId {
        self.owner_uri_id
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolveCtx {
    owner_uri_id: UriId,
}

impl ResolveCtx {
    fn new(owner_uri_id: UriId) -> Self {
        Self { owner_uri_id }
    }
}

/// Resolve a `TypeFact` (which may contain stubs) to a fully resolved type
/// using the workspace aggregation layer.
pub fn resolve_type(
    owner_uri_id: UriId,
    fact: &TypeFact,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    let mut visited = HashSet::new();
    resolve_recursive(ResolveCtx::new(owner_uri_id), fact, agg, 0, &mut visited)
}

/// Resolve a chain of field accesses like `obj.pos.x`.
/// Returns the resolved type of the final field.
///
/// When the base is a `GlobalRef`, tracks the qualified global name across
/// the chain so that table-extension globals (e.g. `UE4.Foo = nil` registered
/// separately from the `UE4 = {}` table) can be found via `global_shard`
/// fallback when shape-based field lookup fails.
pub fn resolve_field_chain(
    owner_uri_id: UriId,
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_field_chain_inner(ResolveCtx::new(owner_uri_id), base_fact, fields, agg, false)
}

/// UriId-aware variant of `resolve_field_chain` for bases that are
/// **file-local** table shapes. When the base is `Known(Table(shape_id))`,
/// `TableShapeId` is per-file so the plain chain resolver returns
/// `Unknown` (no source-file hint to locate the shape). This seeds the
/// UriId and keeps it threaded through intermediate `Known(Table)` results
/// so deep nested writes like `a.b.c = 1` hover-resolve correctly.
pub fn resolve_field_chain_in_file_id(
    uri_id: UriId,
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_field_chain(uri_id, base_fact, fields, agg)
}

pub(crate) fn resolve_field_chain_prefix_in_file_id(
    uri_id: UriId,
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_field_chain_inner(ResolveCtx::new(uri_id), base_fact, fields, agg, true)
}

/// Shared core of field-chain resolution. `ctx` carries the file that owns
/// per-file ids in the current `TypeFact`.
fn resolve_field_chain_inner(
    ctx: ResolveCtx,
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
    preserve_tail_resolved_location: bool,
) -> ResolvedType {
    if let TypeFact::Union(types) = base_fact {
        let mut resolved_types = Vec::new();
        let mut best_location: Option<(ByteRange, UriId)> = None;
        let mut first_owner = None;
        for t in types {
            let result =
                resolve_field_chain_inner(ctx, t, fields, agg, preserve_tail_resolved_location);
            if result.type_fact != TypeFact::Unknown {
                first_owner.get_or_insert(result.owner_uri_id);
                if best_location.is_none() {
                    if let Some(location) = result.def_location {
                        best_location = Some((location.range, location.uri_id));
                    }
                }
                if !resolved_types.contains(&result.type_fact) {
                    resolved_types.push(result.type_fact);
                }
            }
        }
        return resolved_union_field(ctx, resolved_types, best_location, first_owner);
    }

    let mut visited = HashSet::new();
    let mut current = ResolvedType::from_fact(ctx, base_fact.clone());

    // When the base itself is a nested FieldOf stub (e.g. the scope tree
    // stores `FieldOf { base: FieldOf { base: GlobalRef("utils"), ... }, ... }`
    // for `local X = a.b.c`), flatten it into a sub-chain and pre-resolve
    // it with this same function (which has the global_prefix fallback).
    // Without this, the loop below sees an unresolved FieldOf stub as
    // `current` and `resolve_field_access` fails for the same reason.
    if let Some((inner_base, inner_fields)) = flatten_field_of_chain(base_fact) {
        let pre = resolve_field_chain_inner(ctx, &inner_base, &inner_fields, agg, false);
        if pre.type_fact != TypeFact::Unknown {
            current = pre;
        }
    }

    let mut global_prefix = match base_fact {
        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.to_string()),
        TypeFact::Stub(SymbolicStub::RequireRef { module_path }) => {
            resolve_require_global_name(module_path, agg)
        }
        _ => None,
    };

    for (idx, field) in fields.iter().enumerate() {
        let current_ctx = ResolveCtx::new(current.owner_uri_id);
        let result = match &current.type_fact {
            TypeFact::Known(KnownType::Table(shape_id)) => {
                resolve_table_field(current_ctx, *shape_id, field, agg)
            }
            _ => resolve_field_access(current_ctx, &current.type_fact, field, agg, 0, &mut visited),
        };

        if result.type_fact == TypeFact::Unknown && result.def_location.is_none() {
            if let Some(ref prefix) = global_prefix {
                let qualified = format!("{}.{}", prefix, field);
                let is_chain_tail = idx + 1 == fields.len();
                let fallback = try_global_shard_qualified(
                    current_ctx,
                    &qualified,
                    agg,
                    0,
                    &mut visited,
                    !is_chain_tail || preserve_tail_resolved_location,
                );
                if fallback.type_fact != TypeFact::Unknown || fallback.def_location.is_some() {
                    current = fallback;
                    global_prefix = Some(qualified);
                    continue;
                }
            }
        }

        current = result;

        if current.type_fact == TypeFact::Unknown {
            global_prefix = None;
        } else {
            global_prefix = global_prefix.map(|p| format!("{}.{}", p, field));
        }
    }

    current
}

/// Given a file URI and a local variable name, resolve its type via the scope tree.
pub fn resolve_local_in_file(
    owner_uri_id: UriId,
    local_name: &str,
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    let fact = match scope_tree.resolve_type(byte_offset, local_name) {
        Some(tf) => tf.clone(),
        None => return ResolvedType::unknown(ResolveCtx::new(owner_uri_id)),
    };

    // For nested FieldOf stubs (e.g. `local x = a.b.c`), flatten into a
    // base + field chain and use `resolve_field_chain` which has global
    // shard qualified-name fallback. Without this, `resolve_type` alone
    // fails when intermediate fields are registered as qualified globals
    // (e.g. `utils.locals` via `---@type` assignment) rather than as
    // entries inside a table shape.
    if let Some((base, fields)) = flatten_field_of_chain(&fact) {
        let result = resolve_field_chain(owner_uri_id, &base, &fields, agg);
        if result.type_fact != TypeFact::Unknown {
            return result;
        }
    }

    resolve_type(owner_uri_id, &fact, agg)
}

/// Flatten a nested `FieldOf { base: FieldOf { ... }, field }` chain into
/// a base `TypeFact` and a vector of field names (in access order).
/// Returns `None` if the fact is not a `FieldOf` stub at all.
fn flatten_field_of_chain(fact: &TypeFact) -> Option<(TypeFact, Vec<String>)> {
    let TypeFact::Stub(SymbolicStub::FieldOf { base, field }) = fact else {
        return None;
    };
    let mut fields = vec![field.to_string()];
    let mut current = base.as_ref();
    loop {
        match current {
            TypeFact::Stub(SymbolicStub::FieldOf {
                base: inner_base,
                field: inner_field,
            }) => {
                fields.push(inner_field.to_string());
                current = inner_base.as_ref();
            }
            _ => {
                fields.reverse();
                return Some((current.clone(), fields));
            }
        }
    }
}

/// Get completable fields for a type (for dot-completion).
///
/// `source_uri_hint` provides the file where the base expression lives,
/// used to disambiguate per-file `TableShapeId` values.
pub fn get_fields_for_type_id(
    owner_uri_id: UriId,
    fact: &TypeFact,
    agg: &WorkspaceAggregation,
) -> Vec<FieldCompletion> {
    if let TypeFact::Union(types) = fact {
        let mut fields = Vec::new();
        for t in types {
            fields.extend(get_fields_for_type_id(owner_uri_id, t, agg));
        }
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        fields.dedup_by(|a, b| a.name == b.name);
        return fields;
    }

    // Collect global_shard direct children BEFORE resolving, so that
    // table-extension globals (e.g. `UE4.Foo`) are included even though
    // they live in global_shard rather than in a table shape.
    let mut global_prefix_fields = Vec::new();
    if let TypeFact::Stub(SymbolicStub::GlobalRef { name }) = fact {
        if let Some(node) = agg.global_shard.get_node(name) {
            for (child_name, child_node) in &node.children {
                if let Some(c) = child_node.candidates.first() {
                    let is_func = is_function_type(&c.type_fact)
                        || matches!(c.kind, crate::summary::GlobalContributionKind::Function);
                    global_prefix_fields.push(FieldCompletion {
                        name: child_name.to_string(),
                        type_display: format!("{}", c.type_fact),
                        is_function: is_func,
                        def_range: Some(c.selection_range),
                    });
                }
            }
        }
    }

    let resolved = resolve_type(owner_uri_id, fact, agg);
    let mut fields = collect_fields(&resolved.type_fact, resolved.source_uri_id(), agg);

    // Merge global prefix fields, deduplicating by name
    for gf in global_prefix_fields {
        if !fields.iter().any(|f| f.name == gf.name) {
            fields.push(gf);
        }
    }

    fields
}

#[derive(Debug, Clone)]
pub struct FieldCompletion {
    pub name: String,
    pub type_display: String,
    pub is_function: bool,
    pub def_range: Option<ByteRange>,
}

// ---------------------------------------------------------------------------
// Internal recursive resolver
// ---------------------------------------------------------------------------

fn resolve_recursive(
    ctx: ResolveCtx,
    fact: &TypeFact,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    if depth > MAX_RESOLVE_DEPTH {
        return ResolvedType::unknown(ctx);
    }

    match fact {
        TypeFact::Known(_) => ResolvedType::from_fact(ctx, fact.clone()),
        TypeFact::Unknown => ResolvedType::unknown(ctx),

        TypeFact::Union(types) => {
            let resolved: Vec<TypeFact> = types
                .iter()
                .map(|t| resolve_recursive(ctx, t, agg, depth + 1, visited).type_fact)
                .collect();
            ResolvedType::from_fact(ctx, TypeFact::Union(resolved))
        }

        TypeFact::Stub(stub) => resolve_stub(ctx, stub, agg, depth, visited),
    }
}

fn resolve_stub(
    ctx: ResolveCtx,
    stub: &SymbolicStub,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let visit_key = format!("{}", stub);
    if visited.contains(&visit_key) {
        return ResolvedType::unknown(ctx);
    }
    visited.insert(visit_key.clone());

    let result = match stub {
        SymbolicStub::RequireRef { module_path } => {
            resolve_require(ctx, module_path, agg, depth, visited)
        }

        SymbolicStub::GlobalRef { name } => resolve_global(ctx, name, agg, depth, visited),

        SymbolicStub::TypeRef { name } => resolve_emmy_type(ctx, name, agg),

        SymbolicStub::CallReturn {
            base,
            func_name,
            is_method_call,
            generic_args,
            call_arg_types,
        } => resolve_call_return(
            ctx,
            base,
            func_name,
            *is_method_call,
            generic_args,
            call_arg_types,
            agg,
            depth,
            visited,
        ),

        SymbolicStub::FunctionCallReturn {
            func_name,
            call_arg_types,
        } => resolve_function_call_return(ctx, func_name, call_arg_types, agg, depth, visited),

        SymbolicStub::FieldOf { base, field } => {
            resolve_field_access(ctx, base, field, agg, depth, visited)
        }
    };

    visited.remove(&visit_key);
    result
}

fn resolve_require(
    ctx: ResolveCtx,
    module_path: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let target_uri_id = match agg.resolve_module_to_id(module_path) {
        Some(id) => id,
        None => return ResolvedType::unknown(ctx),
    };
    let target_ctx = ResolveCtx::new(target_uri_id);

    let return_fact = {
        let summary = match agg.summary_by_id(target_uri_id) {
            Some(s) => s,
            None => return ResolvedType::unknown(target_ctx),
        };

        if let Some(ref ret) = summary.module_return_type {
            ret.clone()
        } else {
            TypeFact::Stub(SymbolicStub::GlobalRef {
                name: module_path.into(),
            })
        }
    };

    resolve_recursive(target_ctx, &return_fact, agg, depth + 1, visited)
}

fn resolve_global(
    ctx: ResolveCtx,
    name: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    // Look up in global_shard for the best candidate
    let candidate = {
        let candidates = match agg.global_shard.get(name) {
            Some(c) if !c.is_empty() => c,
            _ => return ResolvedType::unknown(ctx),
        };
        candidates[0].clone()
    };

    let candidate_ctx = ResolveCtx::new(candidate.source_uri_id());
    let mut resolved =
        resolve_recursive(candidate_ctx, &candidate.type_fact, agg, depth + 1, visited);
    if resolved.def_location.is_none() {
        resolved.def_location = Some(ResolvedLocation {
            uri_id: candidate.source_uri_id(),
            range: candidate.selection_range,
        });
    }
    resolved
}

fn resolve_function_call_return(
    ctx: ResolveCtx,
    func_name: &str,
    call_arg_types: &[TypeFact],
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let candidate = match agg.global_shard.get(func_name) {
        Some(candidates) if !candidates.is_empty() => candidates[0].clone(),
        _ => return ResolvedType::unknown(ctx),
    };

    let owner_uri_id = candidate.source_uri_id();
    let owner_ctx = ResolveCtx::new(owner_uri_id);
    let resolved = resolve_recursive(owner_ctx, &candidate.type_fact, agg, depth + 1, visited);
    let ret = match &resolved.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => sig.returns.first().cloned(),
        TypeFact::Known(KnownType::FunctionRef(fid)) => agg
            .summary_by_id(owner_uri_id)
            .and_then(|summary| summary.function_summaries.get(fid))
            .and_then(|fs| function_return_with_call_args(fs, call_arg_types)),
        _ => None,
    };

    let Some(ret) = ret else {
        return ResolvedType::unknown(owner_ctx);
    };
    resolve_recursive(owner_ctx, &ret, agg, depth + 1, visited)
}

/// Fallback for `resolve_field_chain`: try a qualified name (e.g. `UE4.Foo`)
/// directly in `global_shard`. Handles table-extension globals that were
/// registered as separate entries rather than as fields on a table shape.
fn try_global_shard_qualified(
    ctx: ResolveCtx,
    qualified: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
    preserve_resolved_location: bool,
) -> ResolvedType {
    let candidate = match agg.global_shard.get(qualified) {
        Some(c) if !c.is_empty() => c[0].clone(),
        _ => return ResolvedType::unknown(ctx),
    };

    let candidate_ctx = ResolveCtx::new(candidate.source_uri_id());
    let mut resolved =
        resolve_recursive(candidate_ctx, &candidate.type_fact, agg, depth + 1, visited);
    if !preserve_resolved_location || resolved.def_location.is_none() {
        resolved.def_location = Some(ResolvedLocation {
            uri_id: candidate.source_uri_id(),
            range: candidate.selection_range,
        });
    }
    resolved
}

fn resolve_emmy_type(ctx: ResolveCtx, name: &str, agg: &WorkspaceAggregation) -> ResolvedType {
    let candidate = match agg.type_candidates(name) {
        Some(candidates) if !candidates.is_empty() => &candidates[0],
        _ => {
            return ResolvedType::from_fact(ctx, TypeFact::Known(KnownType::EmmyType(name.into())))
        }
    };
    ResolvedType::with_location(
        TypeFact::Known(KnownType::EmmyType(name.into())),
        candidate.source_uri_id(),
        candidate.range,
    )
}

fn resolve_call_return(
    ctx: ResolveCtx,
    base: &TypeFact,
    func_name: &str,
    is_method_call: bool,
    generic_args: &[TypeFact],
    call_arg_types: &[TypeFact],
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let base_resolved = resolve_recursive(ctx, base, agg, depth + 1, visited);
    let base_ctx = ResolveCtx::new(base_resolved.owner_uri_id);

    // When the base resolves to a generic class instance (e.g. `Stack<string>`),
    // look up the method's return type and substitute generic parameters.
    if let TypeFact::Known(KnownType::EmmyGeneric(ref type_name, ref actual_params)) =
        base_resolved.type_fact
    {
        let ret = resolve_method_return_with_generics(
            type_name,
            func_name,
            actual_params,
            call_arg_types,
            is_method_call,
            agg,
        );
        if ret != TypeFact::Unknown {
            return resolve_recursive(base_ctx, &ret, agg, depth + 1, visited);
        }
    }

    // When the stub carried generic_args (e.g. `sstack:pop()` where sstack
    // is `Stack<string>`), use them for substitution even if the resolved
    // base lost the generic info (TypeRef -> EmmyType).
    if !generic_args.is_empty() {
        if let Some(type_name) = base_type_name(base) {
            let ret = resolve_method_return_with_generics(
                type_name,
                func_name,
                generic_args,
                call_arg_types,
                is_method_call,
                agg,
            );
            if ret != TypeFact::Unknown {
                return resolve_recursive(ctx, &ret, agg, depth + 1, visited);
            }
        }
    }

    // When the base itself is the result of another call, the original
    // `base` stub has no useful qualified name. Continue from the resolved
    // Emmy class type and use the same field lookup path as method hover.
    if let TypeFact::Known(KnownType::EmmyType(ref type_name)) = base_resolved.type_fact {
        let field_result = resolve_emmy_field(base_ctx, type_name, func_name, agg);
        if let Some(ret) =
            first_function_return_with_call_args(&field_result, call_arg_types, is_method_call, agg)
        {
            return resolve_recursive(base_ctx, &ret, agg, depth + 1, visited);
        }
    }

    // When the base resolves to a Table shape (e.g. `local M = {}` returned
    // via `require()`), look up the function field directly in the shape.
    if let TypeFact::Known(KnownType::Table(shape_id)) = &base_resolved.type_fact {
        let uri_id = base_resolved.source_uri_id();
        let ret = {
            let summary = match agg.summary_by_id(uri_id) {
                Some(s) => s,
                None => return ResolvedType::unknown(base_ctx),
            };
            summary
                .table_shapes
                .get(shape_id)
                .and_then(|shape| shape.get_field(func_name))
                .and_then(|fi| match &fi.type_fact {
                    TypeFact::Known(KnownType::Function(ref sig)) => sig.returns.first().cloned(),
                    TypeFact::Known(KnownType::FunctionRef(fid)) => {
                        summary.function_summaries.get(fid).and_then(|fs| {
                            function_return_with_call_args_for_call(
                                fs,
                                call_arg_types,
                                is_method_call,
                            )
                        })
                    }
                    _ => None,
                })
        };
        if let Some(ret) = ret {
            return resolve_recursive(base_ctx, &ret, agg, depth + 1, visited);
        }
    }

    // Collect candidate base names for qualified lookups.
    // `base_type_name` gives the stub's own name (e.g. module_path for
    // RequireRef), but when the module returns a global (e.g. `return Player`),
    // the *real* qualified name lives under that global name, not the module
    // path. Collect both so we try e.g. "player.new" AND "Player.new".
    let mut candidate_names: Vec<String> = Vec::new();
    if let Some(name) = base_type_name(base) {
        candidate_names.push(name.to_string());
    }
    if let Some(module_path) = base_require_module_path(base) {
        if let Some(global_name) = resolve_require_global_name(module_path, agg) {
            if !candidate_names.contains(&global_name) {
                candidate_names.push(global_name);
            }
        }
    }

    // If base resolved to a known type, look for the function in its source
    {
        let uri_id = base_resolved.source_uri_id();
        let return_type = {
            let summary = match agg.summary_by_id(uri_id) {
                Some(s) => s,
                None => return ResolvedType::unknown(base_ctx),
            };

            // Try bare name first, then qualified `Type:method` / `Type.method`.
            let mut found = summary
                .get_function_by_name(func_name)
                .and_then(|fs| function_return_with_call_args(fs, call_arg_types));

            if found.is_none() {
                'outer_fs: for type_name in &candidate_names {
                    for sep in [":", "."] {
                        let qualified = format!("{}{}{}", type_name, sep, func_name);
                        if let Some(fs) = summary.get_function_by_name(&qualified) {
                            found = function_return_with_call_args(fs, call_arg_types);
                            if found.is_some() {
                                break 'outer_fs;
                            }
                        }
                    }
                }
            }

            found
        };

        if let Some(ret) = return_type {
            return resolve_recursive(base_ctx, &ret, agg, depth + 1, visited);
        }
    }

    // Try looking up `base_name.func_name` as a qualified global name.
    // The tree-structured global_shard merges dot and colon separators,
    // so a single lookup covers both `function Foo.bar()` and
    // `function Foo:bar()`.
    for base_name in &candidate_names {
        let qualified = format!("{}.{}", base_name, func_name);
        if let Some(c) = agg
            .global_shard
            .get(&qualified)
            .and_then(|v| v.first().cloned())
        {
            let candidate_ctx = ResolveCtx::new(c.source_uri_id());
            let resolved = resolve_recursive(candidate_ctx, &c.type_fact, agg, depth + 1, visited);
            match &resolved.type_fact {
                TypeFact::Known(KnownType::Function(ref sig)) => {
                    if let Some(ret) = sig.returns.first() {
                        let mut ret_resolved =
                            resolve_recursive(candidate_ctx, ret, agg, depth + 1, visited);
                        if ret_resolved.def_location.is_none() {
                            ret_resolved.def_location = Some(ResolvedLocation {
                                uri_id: c.source_uri_id(),
                                range: c.selection_range,
                            });
                        }
                        return ret_resolved;
                    }
                }
                TypeFact::Known(KnownType::FunctionRef(fid)) => {
                    let uri_id = resolved.source_uri_id();
                    if let Some(summary) = agg.summary_by_id(uri_id) {
                        if let Some(fs) = summary.function_summaries.get(fid) {
                            if let Some(ret) = function_return_with_call_args_for_call(
                                fs,
                                call_arg_types,
                                is_method_call,
                            ) {
                                let mut ret_resolved =
                                    resolve_recursive(candidate_ctx, &ret, agg, depth + 1, visited);
                                if ret_resolved.def_location.is_none() {
                                    ret_resolved.def_location = Some(ResolvedLocation {
                                        uri_id: c.source_uri_id(),
                                        range: c.selection_range,
                                    });
                                }
                                return ret_resolved;
                            }
                        }
                    }
                }
                _ => {}
            }
            return ResolvedType::with_location(
                c.type_fact.clone(),
                c.source_uri_id(),
                c.selection_range,
            );
        }
    }

    ResolvedType::unknown(ctx)
}

fn first_function_return_with_call_args(
    result: &ResolvedType,
    call_arg_types: &[TypeFact],
    is_method_call: bool,
    agg: &WorkspaceAggregation,
) -> Option<TypeFact> {
    match &result.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => sig.returns.first().cloned(),
        TypeFact::Known(KnownType::FunctionRef(fid)) => {
            let uri_id = result.source_uri_id();
            let summary = agg.summary_by_id(uri_id)?;
            let fs = summary.function_summaries.get(fid)?;
            function_return_with_call_args_for_call(fs, call_arg_types, is_method_call)
        }
        _ => None,
    }
}

fn function_return_with_call_args_for_call(
    fs: &crate::summary::FunctionSummary,
    call_arg_types: &[TypeFact],
    is_method_call: bool,
) -> Option<TypeFact> {
    let effective_args = if is_method_call
        && !call_arg_types.is_empty()
        && fs
            .signature
            .params
            .first()
            .map(|p| p.name.as_str() != "self")
            .unwrap_or(true)
    {
        &call_arg_types[1..]
    } else {
        call_arg_types
    };
    function_return_with_call_args(fs, effective_args)
}

fn function_return_with_call_args(
    fs: &crate::summary::FunctionSummary,
    call_arg_types: &[TypeFact],
) -> Option<TypeFact> {
    if !fs.generic_params.is_empty() && !call_arg_types.is_empty() {
        if let Some(substituted_returns) = unify_function_generics(
            &fs.generic_params,
            &fs.signature.params,
            call_arg_types,
            &fs.signature.returns,
        ) {
            if let Some(ret) = substituted_returns.first() {
                return Some(ret.clone());
            }
        }
    }
    fs.signature.returns.first().cloned()
}

/// Extract the base name from a `TypeFact` for qualified name lookups.
/// Works for the named stub variants and known Emmy class facts — the values
/// that carry a meaningful name for `{name}.{field}` / `{name}:{field}` lookups
/// in `function_summaries` and `global_shard`.
///
/// Note: for `RequireRef`, this returns the module_path (e.g. `"player"`),
/// which may differ from the actual global name (e.g. `"Player"`). Callers
/// that need the real global name should also check `module_return_type`.
fn base_type_name(fact: &TypeFact) -> Option<&str> {
    match fact {
        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.as_str()),
        TypeFact::Stub(SymbolicStub::RequireRef { module_path }) => Some(module_path.as_str()),
        TypeFact::Stub(SymbolicStub::TypeRef { name }) => Some(name.as_str()),
        TypeFact::Known(KnownType::EmmyType(name)) => Some(name.as_str()),
        TypeFact::Known(KnownType::EmmyGeneric(name, _)) => Some(name.as_str()),
        _ => None,
    }
}

fn base_require_module_path(fact: &TypeFact) -> Option<&str> {
    match fact {
        TypeFact::Stub(SymbolicStub::RequireRef { module_path }) => Some(module_path.as_str()),
        _ => None,
    }
}

/// Given a `RequireRef` module path, resolve the module's `module_return_type`
/// and extract the global name if it is a `GlobalRef` stub.
///
/// This is the common pattern for `local Player = require("player")` where
/// `player.lua` does `return Player` — the module_return_type is
/// `GlobalRef { name: "Player" }`, and we need the real global name `"Player"`
/// (not the module path `"player"`) for qualified name lookups like
/// `"Player.new"` in `function_summaries` and `global_shard`.
pub fn resolve_require_global_name(
    module_path: &str,
    agg: &WorkspaceAggregation,
) -> Option<String> {
    agg.resolve_module_to_id(module_path)
        .and_then(|target_uri_id| {
            agg.summary_by_id(target_uri_id)
                .and_then(|s| s.module_return_type.as_ref())
                .and_then(|ret| match ret {
                    TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.to_string()),
                    _ => None,
                })
        })
}

fn resolve_field_access(
    ctx: ResolveCtx,
    base: &TypeFact,
    field: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    if depth > MAX_RESOLVE_DEPTH {
        return ResolvedType::unknown(ctx);
    }

    if let TypeFact::Union(types) = base {
        let mut resolved_types = Vec::new();
        let mut best_location: Option<(ByteRange, UriId)> = None;
        let mut first_owner = None;
        for t in types {
            let result = resolve_field_access(ctx, t, field, agg, depth + 1, visited);
            if result.type_fact != TypeFact::Unknown {
                first_owner.get_or_insert(result.owner_uri_id);
                if best_location.is_none() {
                    if let Some(location) = result.def_location {
                        best_location = Some((location.range, location.uri_id));
                    }
                }
                if !resolved_types.contains(&result.type_fact) {
                    resolved_types.push(result.type_fact);
                }
            }
        }
        return resolved_union_field(ctx, resolved_types, best_location, first_owner);
    }

    let base_resolved = resolve_recursive(ctx, base, agg, depth + 1, visited);
    let base_ctx = ResolveCtx::new(base_resolved.owner_uri_id);

    match &base_resolved.type_fact {
        TypeFact::Known(KnownType::Table(shape_id)) => {
            let result = resolve_table_field(base_ctx, *shape_id, field, agg);
            if result.type_fact != TypeFact::Unknown || result.def_location.is_some() {
                return result;
            }
            // Fallback: when the table shape doesn't contain the field,
            // check if the original base was a GlobalRef and try a
            // qualified name lookup in global_shard (e.g. `utils.locals`
            // registered as a global TableExtension rather than a shape
            // field).
            if let TypeFact::Stub(SymbolicStub::GlobalRef { name }) = base {
                let qualified = format!("{}.{}", name, field);
                if let Some(candidates) = agg.global_shard.get(&qualified).cloned() {
                    if let Some(c) = candidates.first() {
                        let candidate_ctx = ResolveCtx::new(c.source_uri_id());
                        let mut resolved =
                            resolve_recursive(candidate_ctx, &c.type_fact, agg, depth + 1, visited);
                        if resolved.def_location.is_none() {
                            resolved.def_location = Some(ResolvedLocation {
                                uri_id: c.source_uri_id(),
                                range: c.selection_range,
                            });
                        }
                        return resolved;
                    }
                }
            }
            result
        }

        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            resolve_emmy_field(base_ctx, type_name, field, agg)
        }

        TypeFact::Known(KnownType::EmmyGeneric(type_name, actual_params)) => {
            let mut result = resolve_emmy_field(base_ctx, type_name, field, agg);
            result.type_fact =
                substitute_generics(&result.type_fact, type_name, actual_params, agg);
            result
        }

        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => {
            // Try `name.field` as a qualified global name (O(1) lookup).
            // Function declarations like `function Foo.bar()` are registered
            // in global_shard as "Foo.bar" by summary_builder.
            let qualified = format!("{}.{}", name, field);
            if let Some(candidates) = agg.global_shard.get(&qualified).cloned() {
                if let Some(c) = candidates.first() {
                    return ResolvedType::with_location(
                        c.type_fact.clone(),
                        c.source_uri_id(),
                        c.selection_range,
                    );
                }
            }

            ResolvedType::unknown(base_ctx)
        }

        TypeFact::Union(types) => {
            let mut resolved_types = Vec::new();
            let mut best_location: Option<(ByteRange, UriId)> = None;
            let mut first_owner = None;
            for t in types {
                let result = resolve_field_access(base_ctx, t, field, agg, depth + 1, visited);
                if result.type_fact != TypeFact::Unknown {
                    first_owner.get_or_insert(result.owner_uri_id);
                    if best_location.is_none() {
                        if let Some(location) = result.def_location {
                            best_location = Some((location.range, location.uri_id));
                        }
                    }
                    if !resolved_types.contains(&result.type_fact) {
                        resolved_types.push(result.type_fact);
                    }
                }
            }
            resolved_union_field(base_ctx, resolved_types, best_location, first_owner)
        }

        _ => ResolvedType::unknown(base_ctx),
    }
}

fn resolved_union_field(
    fallback_ctx: ResolveCtx,
    mut resolved_types: Vec<TypeFact>,
    best_location: Option<(ByteRange, UriId)>,
    first_owner: Option<UriId>,
) -> ResolvedType {
    match resolved_types.len() {
        0 => ResolvedType::unknown(fallback_ctx),
        1 => {
            let fact = resolved_types.pop().unwrap();
            if let Some((range, uri_id)) = best_location {
                ResolvedType::with_location(fact, uri_id, range)
            } else {
                ResolvedType::from_fact(
                    ResolveCtx::new(first_owner.unwrap_or(fallback_ctx.owner_uri_id)),
                    fact,
                )
            }
        }
        _ => {
            let fact = TypeFact::Union(resolved_types);
            if let Some((range, uri_id)) = best_location {
                ResolvedType::with_location(fact, uri_id, range)
            } else {
                ResolvedType::from_fact(
                    ResolveCtx::new(first_owner.unwrap_or(fallback_ctx.owner_uri_id)),
                    fact,
                )
            }
        }
    }
}

fn resolve_table_field(
    ctx: ResolveCtx,
    shape_id: TableShapeId,
    field: &str,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    let uri_id = ctx.owner_uri_id;
    let summary = match agg.summary_by_id(uri_id) {
        Some(s) => s,
        None => return ResolvedType::unknown(ctx),
    };
    if let Some(shape) = summary.table_shapes.get(&shape_id) {
        if let Some(fi) = shape.get_field(field) {
            return ResolvedType {
                type_fact: fi.type_fact.clone(),
                def_location: fi.def_range.map(|range| ResolvedLocation { uri_id, range }),
                owner_uri_id: uri_id,
            };
        }
    }
    ResolvedType::unknown(ctx)
}

fn resolve_emmy_field(
    ctx: ResolveCtx,
    type_name: &str,
    field: &str,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_emmy_field_with_visited(ctx, type_name, field, agg, &mut HashSet::new())
}

fn resolve_emmy_field_with_visited(
    ctx: ResolveCtx,
    type_name: &str,
    field: &str,
    agg: &WorkspaceAggregation,
    visited_types: &mut HashSet<String>,
) -> ResolvedType {
    if !visited_types.insert(type_name.to_string()) {
        return ResolvedType::unknown(ctx);
    }

    if let Some(candidates) = agg.type_candidates(type_name) {
        for candidate in candidates {
            if let Some(summary) = agg.summary_by_id(candidate.source_uri_id()) {
                let candidate_ctx = ResolveCtx::new(candidate.source_uri_id());
                for td in &summary.type_definitions {
                    if td.name == type_name {
                        if td.kind == crate::summary::TypeDefinitionKind::Alias {
                            if let Some(TypeFact::Known(KnownType::EmmyType(ref aliased_name))) =
                                td.alias_type
                            {
                                let result = resolve_emmy_field_with_visited(
                                    candidate_ctx,
                                    aliased_name,
                                    field,
                                    agg,
                                    visited_types,
                                );
                                if result.type_fact != TypeFact::Unknown
                                    || result.def_location.is_some()
                                {
                                    return result;
                                }
                            }
                        }

                        for tf in &td.fields {
                            if tf.name == field {
                                return ResolvedType {
                                    type_fact: tf.type_fact.clone(),
                                    def_location: Some(ResolvedLocation {
                                        uri_id: candidate.source_uri_id(),
                                        range: tf.range,
                                    }),
                                    owner_uri_id: candidate.source_uri_id(),
                                };
                            }
                        }

                        // Fallback: when the class anchor has a table shape
                        // (e.g. `local Damageable = {}` or `Damageable = {}`),
                        // fields may live in the shape rather than global_shard.
                        // Use the pre-computed anchor_shape_id to find them directly.
                        if let Some(shape_id) = td.anchor_shape_id {
                            if let Some(shape) = summary.table_shapes.get(&shape_id) {
                                if let Some(fi) = shape.get_field(field) {
                                    return ResolvedType {
                                        type_fact: fi.type_fact.clone(),
                                        def_location: fi.def_range.map(|range| ResolvedLocation {
                                            uri_id: candidate.source_uri_id(),
                                            range,
                                        }),
                                        owner_uri_id: candidate.source_uri_id(),
                                    };
                                }
                            }
                        }

                        for parent in &td.parents {
                            let result = resolve_emmy_field_with_visited(
                                candidate_ctx,
                                parent,
                                field,
                                agg,
                                visited_types,
                            );
                            if result.type_fact != TypeFact::Unknown
                                || result.def_location.is_some()
                            {
                                return result;
                            }
                        }
                    }
                }
            }
        }
    }

    // Try `Type.field` in global_shard (tree merges dot and colon,
    // so `function Stack:push()` and `function Stack.new()` are both
    // reachable via a single lookup).
    {
        let qualified = format!("{}.{}", type_name, field);
        if let Some(global_candidates) = agg.global_shard.get(&qualified) {
            if let Some(c) = global_candidates.first() {
                return ResolvedType::with_location(
                    c.type_fact.clone(),
                    c.source_uri_id(),
                    c.selection_range,
                );
            }
        }
    }

    ResolvedType::unknown(ctx)
}

fn collect_fields(
    fact: &TypeFact,
    source_uri_id: UriId,
    agg: &WorkspaceAggregation,
) -> Vec<FieldCompletion> {
    let mut fields = Vec::new();

    match fact {
        TypeFact::Known(KnownType::Table(shape_id)) => {
            let uri_id = source_uri_id;
            if let Some(summary) = agg.summary_by_id(uri_id) {
                if let Some(shape) = summary.table_shapes.get(shape_id) {
                    for (name, fi) in &shape.fields {
                        fields.push(FieldCompletion {
                            name: name.to_string(),
                            type_display: format!("{}", fi.type_fact),
                            is_function: is_function_type(&fi.type_fact),
                            def_range: fi.def_range,
                        });
                    }
                }
            }
        }

        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            collect_emmy_fields_recursive(type_name, agg, &mut fields, &mut HashSet::new());
        }

        TypeFact::Known(KnownType::EmmyGeneric(type_name, actual_params)) => {
            collect_emmy_fields_recursive(type_name, agg, &mut fields, &mut HashSet::new());
            let param_names = get_generic_param_names(type_name, agg);
            for f in &mut fields {
                for (i, pname) in param_names.iter().enumerate() {
                    if let Some(actual) = actual_params.get(i) {
                        let actual_str = format!("{}", actual);
                        f.type_display = f.type_display.replace(pname.as_str(), &actual_str);
                    }
                }
            }
        }

        // GlobalRef stubs are normally resolved before reaching collect_fields.
        // Global prefix-based field collection is handled in get_fields_for_type.
        TypeFact::Stub(SymbolicStub::GlobalRef { .. }) => {}

        TypeFact::Union(types) => {
            for t in types {
                fields.extend(collect_fields(t, source_uri_id, agg));
            }
            fields.sort_by(|a, b| a.name.cmp(&b.name));
            fields.dedup_by(|a, b| a.name == b.name);
        }

        _ => {}
    }

    fields
}

fn collect_emmy_fields_recursive(
    type_name: &str,
    agg: &WorkspaceAggregation,
    fields: &mut Vec<FieldCompletion>,
    visited: &mut HashSet<String>,
) {
    if !visited.insert(type_name.to_string()) {
        return;
    }
    if let Some(candidates) = agg.type_candidates(type_name) {
        for candidate in candidates {
            if let Some(summary) = agg.summary_by_id(candidate.source_uri_id()) {
                for td in &summary.type_definitions {
                    if td.name == type_name {
                        if td.kind == crate::summary::TypeDefinitionKind::Alias {
                            if let Some(TypeFact::Known(KnownType::EmmyType(ref aliased_name))) =
                                td.alias_type
                            {
                                collect_emmy_fields_recursive(aliased_name, agg, fields, visited);
                            }
                        }

                        for tf in &td.fields {
                            fields.push(FieldCompletion {
                                name: tf.name.to_string(),
                                type_display: format!("{}", tf.type_fact),
                                is_function: is_function_type(&tf.type_fact),
                                def_range: Some(tf.range),
                            });
                        }

                        // Also collect fields from the local table shape
                        // when the class anchor is a local variable. Use the
                        // pre-computed anchor_shape_id to find them directly.
                        if let Some(shape_id) = td.anchor_shape_id {
                            if let Some(shape) = summary.table_shapes.get(&shape_id) {
                                for (fname, fi) in &shape.fields {
                                    if !fields.iter().any(|f| f.name == fname.as_str()) {
                                        fields.push(FieldCompletion {
                                            name: fname.to_string(),
                                            type_display: format!("{}", fi.type_fact),
                                            is_function: is_function_type(&fi.type_fact),
                                            def_range: fi.def_range,
                                        });
                                    }
                                }
                            }
                        }

                        for parent in &td.parents {
                            collect_emmy_fields_recursive(parent, agg, fields, visited);
                        }
                    }
                }
            }
        }
    }
}

fn is_function_type(fact: &TypeFact) -> bool {
    match fact {
        TypeFact::Known(KnownType::Function(_))
        | TypeFact::Known(KnownType::FunctionRef(_))
        | TypeFact::Stub(SymbolicStub::CallReturn { .. }) => true,
        TypeFact::Union(types) => types.iter().any(is_function_type),
        _ => false,
    }
}

/// Look up the generic parameter names for a class definition.
fn get_generic_param_names(type_name: &str, agg: &WorkspaceAggregation) -> Vec<LuaSymbol> {
    if let Some(candidates) = agg.type_candidates(type_name) {
        for candidate in candidates {
            if let Some(summary) = agg.summary_by_id(candidate.source_uri_id()) {
                for td in &summary.type_definitions {
                    if td.name == type_name && !td.generic_params.is_empty() {
                        return td.generic_params.clone();
                    }
                }
            }
        }
    }
    Vec::new()
}

/// Resolve a method's return type on a generic class instance, substituting
/// the class's generic parameters with the actual type arguments.
///
/// E.g. for `Stack<string>:pop()` where `pop` is declared as `@return T?`,
/// this returns `string?` by looking up `pop` in `Stack`'s source file's
/// `function_summaries` and substituting `T` → `string`.
pub fn resolve_method_return_with_generics(
    type_name: &str,
    method_name: &str,
    actual_params: &[TypeFact],
    call_arg_types: &[TypeFact],
    is_method_call: bool,
    agg: &WorkspaceAggregation,
) -> TypeFact {
    // Find the source UriId for this type so we can look up function_summaries.
    let source_uri_id = agg
        .type_candidates(type_name)
        .and_then(|candidates| candidates.first())
        .map(|c| c.source_uri_id());

    if let Some(uri_id) = source_uri_id {
        let ret = agg.summary_by_id(uri_id).and_then(|summary| {
            for td in &summary.type_definitions {
                if td.name != type_name {
                    continue;
                }
                for field in &td.fields {
                    if field.name != method_name {
                        continue;
                    }
                    return match &field.type_fact {
                        TypeFact::Known(KnownType::Function(sig)) => sig.returns.first().cloned(),
                        TypeFact::Known(KnownType::FunctionRef(fid)) => {
                            summary.function_summaries.get(fid).and_then(|fs| {
                                function_return_with_call_args_for_call(
                                    fs,
                                    call_arg_types,
                                    is_method_call,
                                )
                            })
                        }
                        _ => None,
                    };
                }
            }

            // Try qualified name `TypeName:method` or `TypeName.method` in
            // function_summaries (global functions are indexed with colon
            // normalized to dot).
            let qualified_colon = format!("{}:{}", type_name, method_name);
            let qualified_dot = format!("{}.{}", type_name, method_name);
            let fs = summary
                .get_function_by_name(&qualified_colon)
                .or_else(|| summary.get_function_by_name(&qualified_dot));
            fs.and_then(|fs| {
                function_return_with_call_args_for_call(fs, call_arg_types, is_method_call)
            })
        });

        if let Some(ret_fact) = ret {
            return substitute_generics(&ret_fact, type_name, actual_params, agg);
        }
    }

    // Also try global_shard qualified lookup for `TypeName.method`.
    let qualified = format!("{}.{}", type_name, method_name);
    if let Some(c) = agg
        .global_shard
        .get(&qualified)
        .and_then(|v| v.first().cloned())
    {
        let mut visited = HashSet::new();
        let resolved = resolve_recursive(
            ResolveCtx::new(c.source_uri_id()),
            &c.type_fact,
            agg,
            0,
            &mut visited,
        );
        match &resolved.type_fact {
            TypeFact::Known(KnownType::Function(ref sig)) => {
                if let Some(ret) = sig.returns.first() {
                    return substitute_generics(ret, type_name, actual_params, agg);
                }
            }
            TypeFact::Known(KnownType::FunctionRef(fid)) => {
                let uri_id = resolved.source_uri_id();
                if let Some(summary) = agg.summary_by_id(uri_id) {
                    if let Some(fs) = summary.function_summaries.get(fid) {
                        if let Some(ret) = function_return_with_call_args_for_call(
                            fs,
                            call_arg_types,
                            is_method_call,
                        ) {
                            return substitute_generics(&ret, type_name, actual_params, agg);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    TypeFact::Unknown
}

/// Substitute generic type parameters in a TypeFact.
/// E.g. for `List<string>` where `List` has generic param `T`,
/// replaces any `EmmyType("T")` or `TypeRef("T")` with `string`.
fn substitute_generics(
    fact: &TypeFact,
    class_name: &str,
    actual_params: &[TypeFact],
    agg: &WorkspaceAggregation,
) -> TypeFact {
    let param_names = get_generic_param_names(class_name, agg);
    if param_names.is_empty() {
        return fact.clone();
    }
    substitute_in_fact(fact, &param_names, actual_params)
}

fn substitute_in_fact(
    fact: &TypeFact,
    param_names: &[LuaSymbol],
    actual_params: &[TypeFact],
) -> TypeFact {
    match fact {
        TypeFact::Known(KnownType::EmmyType(name)) => {
            if let Some(i) = param_names.iter().position(|p| p == name.as_str()) {
                if let Some(actual) = actual_params.get(i) {
                    return actual.clone();
                }
            }
            fact.clone()
        }
        TypeFact::Known(KnownType::EmmyGeneric(name, inner_params)) => {
            let substituted: Vec<TypeFact> = inner_params
                .iter()
                .map(|p| substitute_in_fact(p, param_names, actual_params))
                .collect();
            TypeFact::Known(KnownType::EmmyGeneric(*name, substituted))
        }
        TypeFact::Stub(SymbolicStub::TypeRef { name }) => {
            if let Some(i) = param_names.iter().position(|p| p == name.as_str()) {
                if let Some(actual) = actual_params.get(i) {
                    return actual.clone();
                }
            }
            fact.clone()
        }
        TypeFact::Union(types) => {
            let substituted: Vec<TypeFact> = types
                .iter()
                .map(|t| substitute_in_fact(t, param_names, actual_params))
                .collect();
            TypeFact::Union(substituted)
        }
        TypeFact::Known(KnownType::Function(sig)) => {
            let params: Vec<crate::type_system::ParamInfo> = sig
                .params
                .iter()
                .map(|p| crate::type_system::ParamInfo {
                    name: p.name,
                    type_fact: substitute_in_fact(&p.type_fact, param_names, actual_params),
                    optional: p.optional,
                })
                .collect();
            let returns: Vec<TypeFact> = sig
                .returns
                .iter()
                .map(|r| substitute_in_fact(r, param_names, actual_params))
                .collect();
            TypeFact::Known(KnownType::Function(crate::type_system::FunctionSignature {
                params,
                returns,
            }))
        }
        TypeFact::Known(KnownType::FunctionRef(_)) => fact.clone(),
        _ => fact.clone(),
    }
}

// ===========================================================================
// Function-level generic inference (unification)
// ===========================================================================

/// Perform simple unification of function-level generic type parameters.
///
/// Given a function's `generic_params` (e.g. `["T"]`), its formal parameter
/// types (from `FunctionSignature.params`), and the actual argument types
/// at the call site, infer a binding table `{ T → string }` and substitute
/// those bindings into the return types.
///
/// Returns `None` if the function has no generic params, or if no bindings
/// could be inferred. Returns `Some(substituted_returns)` on success.
///
/// Currently supports top-level unification only:
/// - `@param x T` + actual `string` → `T = string`
/// - `@param xs T[]` + actual `string[]` → `T = string` (array element)
/// - Nested generics (`List<T>` vs `List<string>`) are P3.
pub fn unify_function_generics(
    generic_params: &[LuaSymbol],
    formal_params: &[crate::type_system::ParamInfo],
    actual_arg_types: &[TypeFact],
    return_types: &[TypeFact],
) -> Option<Vec<TypeFact>> {
    if generic_params.is_empty() {
        return None;
    }

    // Build binding table: generic_param_name → actual_type
    let mut bindings: Vec<Option<TypeFact>> = vec![None; generic_params.len()];

    for (formal, actual) in formal_params.iter().zip(actual_arg_types.iter()) {
        unify_one(&formal.type_fact, actual, generic_params, &mut bindings);
    }

    // Check if we got any bindings at all
    let any_bound = bindings.iter().any(|b| b.is_some());
    if !any_bound {
        return None;
    }

    // Build the actual_params vector for substitute_in_fact.
    // For unbound params, keep the original EmmyType("T") so it
    // degrades gracefully.
    let actual_params: Vec<TypeFact> = bindings
        .into_iter()
        .enumerate()
        .map(|(i, b)| b.unwrap_or_else(|| TypeFact::Known(KnownType::EmmyType(generic_params[i]))))
        .collect();

    let substituted: Vec<TypeFact> = return_types
        .iter()
        .map(|r| substitute_in_fact(r, generic_params, &actual_params))
        .collect();

    Some(substituted)
}

/// Try to unify a single formal parameter type against an actual argument type,
/// filling in the `bindings` table for any generic params that match.
fn unify_one(
    formal: &TypeFact,
    actual: &TypeFact,
    generic_params: &[LuaSymbol],
    bindings: &mut [Option<TypeFact>],
) {
    // Skip unknown actuals — they don't contribute useful bindings.
    if matches!(actual, TypeFact::Unknown) {
        return;
    }

    match formal {
        // Direct generic param: `@param x T` → T = actual
        TypeFact::Known(KnownType::EmmyType(name)) => {
            if let Some(i) = generic_params.iter().position(|p| p == name.as_str()) {
                if bindings[i].is_none() {
                    bindings[i] = Some(actual.clone());
                }
            }
        }
        // Array of generic: `@param xs T[]` → T = element type of actual.
        // `T[]` is represented as `EmmyGeneric("__array", [EmmyType("T")])`.
        // When the actual is also `__array<X>`, extract the element type X
        // and unify the inner generic param against X.
        TypeFact::Known(KnownType::EmmyGeneric(name, inner_params))
            if name == "__array" && inner_params.len() == 1 =>
        {
            // If actual is `__array<X>`, extract X and unify inner param against it.
            if let TypeFact::Known(KnownType::EmmyGeneric(actual_name, actual_inner)) = actual {
                if actual_name == "__array" && actual_inner.len() == 1 {
                    unify_one(&inner_params[0], &actual_inner[0], generic_params, bindings);
                    return;
                }
            }
            // Fallback: unify inner param directly against the actual
            // (e.g. when actual is a plain type like `number`).
            unify_one(&inner_params[0], actual, generic_params, bindings);
        }
        // Handle Union case (T | nil → T?) as well.
        TypeFact::Union(parts) => {
            // For `T?` (= `T | nil`), try to unify the non-nil part
            for part in parts {
                if !matches!(part, TypeFact::Known(KnownType::Nil)) {
                    // For the actual, also strip nil from union if present
                    let stripped_actual = strip_nil(actual);
                    unify_one(part, &stripped_actual, generic_params, bindings);
                }
            }
        }
        _ => {}
    }
}

/// Strip `nil` from a union type, returning the remaining type.
/// If the type is not a union or has no nil, returns it unchanged.
fn strip_nil(fact: &TypeFact) -> TypeFact {
    match fact {
        TypeFact::Union(parts) => {
            let non_nil: Vec<TypeFact> = parts
                .iter()
                .filter(|p| !matches!(p, TypeFact::Known(KnownType::Nil)))
                .cloned()
                .collect();
            match non_nil.len() {
                0 => TypeFact::Known(KnownType::Nil),
                1 => non_nil.into_iter().next().unwrap(),
                _ => TypeFact::Union(non_nil),
            }
        }
        other => other.clone(),
    }
}
