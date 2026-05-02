use std::collections::HashSet;
use tower_lsp_server::ls_types::Uri;

use crate::aggregation::WorkspaceAggregation;
use crate::table_shape::TableShapeId;
use crate::type_system::*;
use crate::util::ByteRange;

const MAX_RESOLVE_DEPTH: usize = 32;

/// Result of resolving a type, with optional source location for goto.
#[derive(Debug, Clone)]
pub struct ResolvedType {
    pub type_fact: TypeFact,
    pub def_uri: Option<Uri>,
    pub def_range: Option<ByteRange>,
}

impl ResolvedType {
    fn unknown() -> Self {
        Self { type_fact: TypeFact::Unknown, def_uri: None, def_range: None }
    }

    fn from_fact(fact: TypeFact) -> Self {
        Self { type_fact: fact, def_uri: None, def_range: None }
    }

    fn with_location(fact: TypeFact, uri: Uri, range: ByteRange) -> Self {
        Self { type_fact: fact, def_uri: Some(uri), def_range: Some(range) }
    }
}

/// Resolve a `TypeFact` (which may contain stubs) to a fully resolved type
/// using the workspace aggregation layer.
pub fn resolve_type(
    fact: &TypeFact,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    let mut visited = HashSet::new();
    resolve_recursive(fact, agg, 0, &mut visited)
}

/// Resolve a chain of field accesses like `obj.pos.x`.
/// Returns the resolved type of the final field.
///
/// When the base is a `GlobalRef`, tracks the qualified global name across
/// the chain so that table-extension globals (e.g. `UE4.Foo = nil` registered
/// separately from the `UE4 = {}` table) can be found via `global_shard`
/// fallback when shape-based field lookup fails.
pub fn resolve_field_chain(
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_field_chain_inner(base_fact, fields, agg, None, false)
}

/// URI-aware variant of `resolve_field_chain` for bases that are
/// **file-local** table shapes. When the base is `Known(Table(shape_id))`,
/// `TableShapeId` is per-file so the plain chain resolver returns
/// `Unknown` (no `source_uri` hint to locate the shape). This seeds the
/// uri and keeps it threaded through intermediate `Known(Table)` results
/// so deep nested writes like `a.b.c = 1` hover-resolve correctly.
pub fn resolve_field_chain_in_file(
    uri: &Uri,
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_field_chain_inner(base_fact, fields, agg, Some(uri), false)
}

/// Resolve a chain that will be used as the base for another field lookup.
///
/// This preserves the resolved owner's URI even when the requested chain is
/// itself the tail of this call. Diagnostics use this for prefixes such as
/// `utils.test_const` before checking `ON_Evt_LALA`; the table owner must stay
/// at `test_const.lua`, not the `utils.test_const = require(...)` assignment.
pub(crate) fn resolve_field_chain_prefix_in_file(
    uri: &Uri,
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_field_chain_inner(base_fact, fields, agg, Some(uri), true)
}

/// Shared core of `resolve_field_chain` and `resolve_field_chain_in_file`.
///
/// When `uri_hint` is `Some`, the resolver seeds the initial `def_uri` for
/// `Known(Table)` bases and preserves it across intermediate table results,
/// enabling per-file `TableShapeId` lookups. When `None`, the resolver
/// behaves like the original `resolve_field_chain`.
fn resolve_field_chain_inner(
    base_fact: &TypeFact,
    fields: &[String],
    agg: &WorkspaceAggregation,
    uri_hint: Option<&Uri>,
    preserve_tail_resolved_location: bool,
) -> ResolvedType {
    let mut visited = HashSet::new();

    // When a URI hint is provided and the base is already a Table, seed
    // the def_uri so that resolve_table_field can locate the shape.
    let mut current = if uri_hint.is_some()
        && matches!(base_fact, TypeFact::Known(KnownType::Table(_)))
    {
        ResolvedType {
            type_fact: base_fact.clone(),
            def_uri: uri_hint.cloned(),
            def_range: None,
        }
    } else {
        resolve_recursive(base_fact, agg, 0, &mut visited)
    };

    let mut global_prefix = match base_fact {
        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.clone()),
        TypeFact::Stub(SymbolicStub::RequireRef { module_path }) => {
            resolve_require_global_name(module_path, agg)
        }
        _ => None,
    };

    for (idx, field) in fields.iter().enumerate() {
        // When a URI hint is present, resolve Table shapes directly via
        // resolve_table_field (which needs the source_uri). Without the
        // hint we always go through the generic resolve_field_access.
        let result = if uri_hint.is_some() {
            match &current.type_fact {
                TypeFact::Known(KnownType::Table(shape_id)) => {
                    resolve_table_field(*shape_id, field, &current.def_uri, agg)
                }
                _ => resolve_field_access(&current.type_fact, field, agg, 0, &mut visited),
            }
        } else {
            resolve_field_access(&current.type_fact, field, agg, 0, &mut visited)
        };

        if result.type_fact == TypeFact::Unknown && result.def_uri.is_none() {
            if let Some(ref prefix) = global_prefix {
                let qualified = format!("{}.{}", prefix, field);
                let is_chain_tail = idx + 1 == fields.len();
                let fallback =
                    try_global_shard_qualified(
                        &qualified,
                        agg,
                        0,
                        &mut visited,
                        !is_chain_tail || preserve_tail_resolved_location,
                    );
                if fallback.type_fact != TypeFact::Unknown || fallback.def_uri.is_some() {
                    current = fallback;
                    global_prefix = Some(qualified);
                    continue;
                }
            }
        }

        // Preserve uri hint when stepping into another file-local Table.
        //
        // Edge case: `resolve_field_access`'s Union branch may return a
        // `Known(Table)` with `def_uri: None` when no variant has a
        // best-location. In that narrow case we'll re-stamp the caller's
        // uri even though the shape may actually live in a different
        // file. Because `TableShapeId` is per-file, a mismatched uri
        // causes `resolve_table_field` to miss silently and return
        // `Unknown` — never wrong data, just a lost goto/hover hit.
        // Accepting that trade for simpler data flow.
        let kept_uri = uri_hint.is_some()
            && matches!(result.type_fact, TypeFact::Known(KnownType::Table(_)))
            && result.def_uri.is_none();
        current = result;
        if kept_uri {
            current.def_uri = uri_hint.cloned();
        }

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
    _uri: &Uri,
    local_name: &str,
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    let fact = match scope_tree.resolve_type(byte_offset, local_name) {
        Some(tf) => tf.clone(),
        None => return ResolvedType::unknown(),
    };
    resolve_type(&fact, agg)
}

/// Get completable fields for a type (for dot-completion).
///
/// `source_uri_hint` provides the file where the base expression lives,
/// used to disambiguate per-file `TableShapeId` values.
pub fn get_fields_for_type(
    fact: &TypeFact,
    source_uri_hint: Option<&Uri>,
    agg: &WorkspaceAggregation,
) -> Vec<FieldCompletion> {
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
                        name: child_name.clone(),
                        type_display: format!("{}", c.type_fact),
                        is_function: is_func,
                        def_uri: Some(c.source_uri.clone()),
                        def_range: Some(c.selection_range),
                    });
                }
            }
        }
    }

    let resolved = resolve_type(fact, agg);
    let uri = resolved.def_uri.as_ref().or(source_uri_hint).cloned();
    let mut fields = collect_fields(&resolved.type_fact, &uri, agg);

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
    pub def_uri: Option<Uri>,
    pub def_range: Option<ByteRange>,
}

// ---------------------------------------------------------------------------
// Internal recursive resolver
// ---------------------------------------------------------------------------

fn resolve_recursive(
    fact: &TypeFact,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    if depth > MAX_RESOLVE_DEPTH {
        return ResolvedType::unknown();
    }

    match fact {
        TypeFact::Known(_) => ResolvedType::from_fact(fact.clone()),
        TypeFact::Unknown => ResolvedType::unknown(),

        TypeFact::Union(types) => {
            let resolved: Vec<TypeFact> = types
                .iter()
                .map(|t| resolve_recursive(t, agg, depth + 1, visited).type_fact)
                .collect();
            ResolvedType::from_fact(TypeFact::Union(resolved))
        }

        TypeFact::Stub(stub) => resolve_stub(stub, agg, depth, visited),
    }
}

fn resolve_stub(
    stub: &SymbolicStub,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let visit_key = format!("{}", stub);
    if visited.contains(&visit_key) {
        return ResolvedType::unknown();
    }
    visited.insert(visit_key.clone());

    let result = match stub {
        SymbolicStub::RequireRef { module_path } => {
            resolve_require(module_path, agg, depth, visited)
        }

        SymbolicStub::GlobalRef { name } => {
            resolve_global(name, agg, depth, visited)
        }

        SymbolicStub::TypeRef { name } => {
            resolve_emmy_type(name, agg)
        }

        SymbolicStub::CallReturn { base, func_name, generic_args, call_arg_types, .. } => {
            resolve_call_return(base, func_name, generic_args, call_arg_types, agg, depth, visited)
        }

        SymbolicStub::FieldOf { base, field } => {
            resolve_field_access(base, field, agg, depth, visited)
        }
    };

    visited.remove(&visit_key);
    result
}

fn resolve_require(
    module_path: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let target_uri = match agg.resolve_module_to_uri(module_path) {
        Some(u) => u,
        None => return ResolvedType::unknown(),
    };

    let return_fact = {
        let summary = match agg.summaries.get(&target_uri) {
            Some(s) => s,
            None => return ResolvedType::unknown(),
        };

        if let Some(ref ret) = summary.module_return_type {
            ret.clone()
        } else {
            TypeFact::Stub(SymbolicStub::GlobalRef {
                name: module_path.to_string(),
            })
        }
    };

    let mut result = resolve_recursive(&return_fact, agg, depth + 1, visited);
    if result.def_uri.is_none() {
        result.def_uri = Some(target_uri);
    }
    result
}

fn resolve_global(
    name: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    // Look up in global_shard for the best candidate
    let candidate = {
        let candidates = match agg.global_shard.get(name) {
            Some(c) if !c.is_empty() => c,
            _ => return ResolvedType::unknown(),
        };
        candidates[0].clone()
    };

    let mut resolved = resolve_recursive(
        &candidate.type_fact,
        agg,
        depth + 1,
        visited,
    );
    resolved.def_uri = Some(candidate.source_uri.clone());
    resolved.def_range = Some(candidate.selection_range);
    resolved
}

/// Fallback for `resolve_field_chain`: try a qualified name (e.g. `UE4.Foo`)
/// directly in `global_shard`. Handles table-extension globals that were
/// registered as separate entries rather than as fields on a table shape.
fn try_global_shard_qualified(
    qualified: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
    preserve_resolved_location: bool,
) -> ResolvedType {
    let candidate = match agg.global_shard.get(qualified) {
        Some(c) if !c.is_empty() => c[0].clone(),
        _ => return ResolvedType::unknown(),
    };

    let mut resolved = resolve_recursive(
        &candidate.type_fact,
        agg,
        depth + 1,
        visited,
    );
    if !preserve_resolved_location || resolved.def_uri.is_none() {
        resolved.def_uri = Some(candidate.source_uri.clone());
        resolved.def_range = Some(candidate.selection_range);
    }
    resolved
}

fn resolve_emmy_type(
    name: &str,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    let candidate = match agg.type_shard.get(name) {
        Some(candidates) if !candidates.is_empty() => &candidates[0],
        _ => return ResolvedType::from_fact(TypeFact::Known(KnownType::EmmyType(name.to_string()))),
    };

    ResolvedType::with_location(
        TypeFact::Known(KnownType::EmmyType(name.to_string())),
        candidate.source_uri.clone(),
        candidate.range,
    )
}

fn resolve_call_return(
    base: &SymbolicStub,
    func_name: &str,
    generic_args: &[TypeFact],
    call_arg_types: &[TypeFact],
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let base_resolved = resolve_stub(base, agg, depth + 1, visited);

    // When the base resolves to a generic class instance (e.g. `Stack<string>`),
    // look up the method's return type and substitute generic parameters.
    if let TypeFact::Known(KnownType::EmmyGeneric(ref type_name, ref actual_params)) = base_resolved.type_fact {
        let ret = resolve_method_return_with_generics(type_name, func_name, actual_params, agg);
        if ret != TypeFact::Unknown {
            return resolve_recursive(&ret, agg, depth + 1, visited);
        }
    }

    // When the stub carried generic_args (e.g. `sstack:pop()` where sstack
    // is `Stack<string>`), use them for substitution even if the resolved
    // base lost the generic info (TypeRef -> EmmyType).
    if !generic_args.is_empty() {
        if let Some(type_name) = base_type_name(base) {
            let ret = resolve_method_return_with_generics(type_name, func_name, generic_args, agg);
            if ret != TypeFact::Unknown {
                return resolve_recursive(&ret, agg, depth + 1, visited);
            }
        }
    }

    // When the base itself is the result of another call, the original
    // `base` stub has no useful qualified name. Continue from the resolved
    // Emmy class type and use the same field lookup path as method hover.
    if let TypeFact::Known(KnownType::EmmyType(ref type_name)) = base_resolved.type_fact {
        let field_result = resolve_emmy_field(type_name, func_name, agg);
        if let Some(ret) = first_function_return_with_call_args(
            &field_result,
            call_arg_types,
            agg,
        ) {
            return resolve_recursive(&ret, agg, depth + 1, visited);
        }
    }

    // When the base resolves to a Table shape (e.g. `local M = {}` returned
    // via `require()`), look up the function field directly in the shape.
    if let TypeFact::Known(KnownType::Table(shape_id)) = &base_resolved.type_fact {
        if let Some(ref uri) = base_resolved.def_uri {
            let ret = {
                let summary = match agg.summaries.get(uri) {
                    Some(s) => s,
                    None => return ResolvedType::unknown(),
                };
                summary.table_shapes.get(shape_id)
                    .and_then(|shape| shape.fields.get(func_name))
                    .and_then(|fi| {
                        match &fi.type_fact {
                            TypeFact::Known(KnownType::Function(ref sig)) => {
                                sig.returns.first().cloned()
                            }
                            TypeFact::Known(KnownType::FunctionRef(fid)) => {
                                summary.function_summaries.get(fid)
                                    .and_then(|fs| function_return_with_call_args(fs, call_arg_types))
                            }
                            _ => None,
                        }
                    })
            };
            if let Some(ret) = ret {
                return resolve_recursive(&ret, agg, depth + 1, visited);
            }
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
    if let SymbolicStub::RequireRef { module_path } = base {
        if let Some(global_name) = resolve_require_global_name(module_path, agg) {
            if !candidate_names.contains(&global_name) {
                candidate_names.push(global_name);
            }
        }
    }

    // If base resolved to a known type, look for the function in its source
    if let Some(ref uri) = base_resolved.def_uri {
        let return_type = {
            let summary = match agg.summaries.get(uri) {
                Some(s) => s,
                None => return ResolvedType::unknown(),
            };

            // Try bare name first, then qualified `Type:method` / `Type.method`.
            let mut found = summary.get_function_by_name(func_name)
                .and_then(|fs| function_return_with_call_args(fs, call_arg_types));

            if found.is_none() {
                'outer_fs: for type_name in &candidate_names {
                    for sep in [":", "."] {
                        let qualified = format!("{}{}{}", type_name, sep, func_name);
                        if let Some(fs) = summary.get_function_by_name(&qualified) {
                            found = function_return_with_call_args(fs, call_arg_types);
                            if found.is_some() { break 'outer_fs; }
                        }
                    }
                }
            }

            found
        };

        if let Some(ret) = return_type {
            return resolve_recursive(&ret, agg, depth + 1, visited);
        }
    }

    // Try looking up `base_name.func_name` as a qualified global name.
    // The tree-structured global_shard merges dot and colon separators,
    // so a single lookup covers both `function Foo.bar()` and
    // `function Foo:bar()`.
    for base_name in &candidate_names {
        let qualified = format!("{}.{}", base_name, func_name);
        if let Some(c) = agg.global_shard.get(&qualified).and_then(|v| v.first().cloned()) {
            let resolved = resolve_recursive(&c.type_fact, agg, depth + 1, visited);
            match &resolved.type_fact {
                TypeFact::Known(KnownType::Function(ref sig)) => {
                    if let Some(ret) = sig.returns.first() {
                        let mut ret_resolved = resolve_recursive(ret, agg, depth + 1, visited);
                        if ret_resolved.def_uri.is_none() {
                            ret_resolved.def_uri = Some(c.source_uri.clone());
                            ret_resolved.def_range = Some(c.selection_range);
                        }
                        return ret_resolved;
                    }
                }
                TypeFact::Known(KnownType::FunctionRef(fid)) => {
                    if let Some(ref uri) = resolved.def_uri {
                        if let Some(summary) = agg.summaries.get(uri) {
                            if let Some(fs) = summary.function_summaries.get(fid) {
                                if let Some(ret) = function_return_with_call_args(fs, call_arg_types) {
                                    let mut ret_resolved = resolve_recursive(&ret, agg, depth + 1, visited);
                                    if ret_resolved.def_uri.is_none() {
                                        ret_resolved.def_uri = Some(c.source_uri.clone());
                                        ret_resolved.def_range = Some(c.selection_range);
                                    }
                                    return ret_resolved;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            return ResolvedType::with_location(
                c.type_fact.clone(),
                c.source_uri.clone(),
                c.selection_range,
            );
        }
    }

    ResolvedType::unknown()
}

fn first_function_return_with_call_args(
    result: &ResolvedType,
    call_arg_types: &[TypeFact],
    agg: &WorkspaceAggregation,
) -> Option<TypeFact> {
    match &result.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => sig.returns.first().cloned(),
        TypeFact::Known(KnownType::FunctionRef(fid)) => {
            let uri = result.def_uri.as_ref()?;
            let summary = agg.summaries.get(uri)?;
            let fs = summary.function_summaries.get(fid)?;
            function_return_with_call_args(fs, call_arg_types)
        }
        _ => None,
    }
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

/// Extract the base name from a `SymbolicStub` for qualified name lookups.
/// Works for `GlobalRef`, `RequireRef`, and `TypeRef` — the three stub
/// variants that carry a meaningful name for `{name}.{field}` / `{name}:{field}`
/// lookups in `function_summaries` and `global_shard`.
///
/// Note: for `RequireRef`, this returns the module_path (e.g. `"player"`),
/// which may differ from the actual global name (e.g. `"Player"`). Callers
/// that need the real global name should also check `module_return_type`.
fn base_type_name(stub: &SymbolicStub) -> Option<&str> {
    match stub {
        SymbolicStub::GlobalRef { name } => Some(name.as_str()),
        SymbolicStub::RequireRef { module_path } => Some(module_path.as_str()),
        SymbolicStub::TypeRef { name } => Some(name.as_str()),
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
    agg.resolve_module_to_uri(module_path)
        .and_then(|target_uri| {
            agg.summaries.get(&target_uri)
                .and_then(|s| s.module_return_type.as_ref())
                .and_then(|ret| match ret {
                    TypeFact::Stub(SymbolicStub::GlobalRef { name }) => Some(name.clone()),
                    _ => None,
                })
        })
}

fn resolve_field_access(
    base: &TypeFact,
    field: &str,
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    if depth > MAX_RESOLVE_DEPTH {
        return ResolvedType::unknown();
    }

    let base_resolved = resolve_recursive(base, agg, depth + 1, visited);

    match &base_resolved.type_fact {
        TypeFact::Known(KnownType::Table(shape_id)) => {
            resolve_table_field(*shape_id, field, &base_resolved.def_uri, agg)
        }

        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            resolve_emmy_field(type_name, field, agg)
        }

        TypeFact::Known(KnownType::EmmyGeneric(type_name, actual_params)) => {
            let mut result = resolve_emmy_field(type_name, field, agg);
            result.type_fact = substitute_generics(
                &result.type_fact, type_name, actual_params, agg,
            );
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
                        c.source_uri.clone(),
                        c.selection_range,
                    );
                }
            }

            ResolvedType::unknown()
        }

        TypeFact::Union(types) => {
            let mut resolved_types = Vec::new();
            let mut best_location: Option<(Uri, ByteRange)> = None;
            for t in types {
                let result = resolve_field_access(t, field, agg, depth + 1, visited);
                if result.type_fact != TypeFact::Unknown {
                    if best_location.is_none() {
                        if let (Some(u), Some(r)) = (&result.def_uri, &result.def_range) {
                            best_location = Some((u.clone(), *r));
                        }
                    }
                    if !resolved_types.contains(&result.type_fact) {
                        resolved_types.push(result.type_fact);
                    }
                }
            }
            match resolved_types.len() {
                0 => ResolvedType::unknown(),
                1 => {
                    let fact = resolved_types.into_iter().next().unwrap();
                    if let Some((uri, range)) = best_location {
                        ResolvedType::with_location(fact, uri, range)
                    } else {
                        ResolvedType::from_fact(fact)
                    }
                }
                _ => {
                    let fact = TypeFact::Union(resolved_types);
                    if let Some((uri, range)) = best_location {
                        ResolvedType::with_location(fact, uri, range)
                    } else {
                        ResolvedType::from_fact(fact)
                    }
                }
            }
        }

        _ => ResolvedType::unknown(),
    }
}

fn resolve_table_field(
    shape_id: TableShapeId,
    field: &str,
    source_uri: &Option<Uri>,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    // TableShapeId is a per-file counter, so we MUST know which file it
    // belongs to.  Without source_uri we could match a wrong file's shape.
    let uri = match source_uri {
        Some(u) => u,
        None => return ResolvedType::unknown(),
    };
    let summary = match agg.summaries.get(uri) {
        Some(s) => s,
        None => return ResolvedType::unknown(),
    };
    if let Some(shape) = summary.table_shapes.get(&shape_id) {
        if let Some(fi) = shape.fields.get(field) {
            return ResolvedType {
                type_fact: fi.type_fact.clone(),
                def_uri: Some(uri.clone()),
                def_range: fi.def_range,
            };
        }
    }
    ResolvedType::unknown()
}

fn resolve_emmy_field(
    type_name: &str,
    field: &str,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    resolve_emmy_field_with_visited(type_name, field, agg, &mut HashSet::new())
}

fn resolve_emmy_field_with_visited(
    type_name: &str,
    field: &str,
    agg: &WorkspaceAggregation,
    visited_types: &mut HashSet<String>,
) -> ResolvedType {
    if !visited_types.insert(type_name.to_string()) {
        return ResolvedType::unknown();
    }

    if let Some(candidates) = agg.type_shard.get(type_name) {
        for candidate in candidates {
            if let Some(summary) = agg.summaries.get(&candidate.source_uri) {
                for td in &summary.type_definitions {
                    if td.name == type_name {

                        if td.kind == crate::summary::TypeDefinitionKind::Alias {
                            if let Some(TypeFact::Known(KnownType::EmmyType(ref aliased_name))) = td.alias_type {
                                let result = resolve_emmy_field_with_visited(aliased_name, field, agg, visited_types);
                                if result.type_fact != TypeFact::Unknown || result.def_uri.is_some() {
                                    return result;
                                }
                            }
                        }

                        for tf in &td.fields {
                            if tf.name == field {
                                return ResolvedType {
                                    type_fact: tf.type_fact.clone(),
                                    def_uri: Some(candidate.source_uri.clone()),
                                    def_range: Some(tf.range),
                                };
                            }
                        }

                        // Fallback: when the class anchor is a local table
                        // (e.g. `local Damageable = {}`), methods defined via
                        // `function Damageable:take_damage()` are written into
                        // the table shape rather than global_shard. Use the
                        // pre-computed anchor_shape_id to find them directly.
                        if let Some(shape_id) = td.anchor_shape_id {
                            if let Some(shape) = summary.table_shapes.get(&shape_id) {
                                if let Some(fi) = shape.fields.get(field) {
                                    return ResolvedType {
                                        type_fact: fi.type_fact.clone(),
                                        def_uri: Some(candidate.source_uri.clone()),
                                        def_range: fi.def_range,
                                    };
                                }
                            }
                        }

                        for parent in &td.parents {
                            let result = resolve_emmy_field_with_visited(parent, field, agg, visited_types);
                            if result.type_fact != TypeFact::Unknown || result.def_uri.is_some() {
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
                    c.source_uri.clone(),
                    c.selection_range,
                );
            }
        }
    }

    ResolvedType::unknown()
}

fn collect_fields(
    fact: &TypeFact,
    source_uri: &Option<Uri>,
    agg: &WorkspaceAggregation,
) -> Vec<FieldCompletion> {
    let mut fields = Vec::new();

    match fact {
        TypeFact::Known(KnownType::Table(shape_id)) => {
            // TableShapeId is per-file, so we need source_uri to avoid
            // cross-file collisions.
            if let Some(uri) = source_uri {
                if let Some(summary) = agg.summaries.get(uri) {
                    if let Some(shape) = summary.table_shapes.get(shape_id) {
                        for (name, fi) in &shape.fields {
                            fields.push(FieldCompletion {
                                name: name.clone(),
                                type_display: format!("{}", fi.type_fact),
                                is_function: is_function_type(&fi.type_fact),
                                def_uri: Some(uri.clone()),
                                def_range: fi.def_range,
                            });
                        }
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
                fields.extend(collect_fields(t, source_uri, agg));
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
    if let Some(candidates) = agg.type_shard.get(type_name) {
        for candidate in candidates {
            if let Some(summary) = agg.summaries.get(&candidate.source_uri) {
                for td in &summary.type_definitions {
                    if td.name == type_name {
                        if td.kind == crate::summary::TypeDefinitionKind::Alias {
                            if let Some(TypeFact::Known(KnownType::EmmyType(ref aliased_name))) = td.alias_type {
                                collect_emmy_fields_recursive(aliased_name, agg, fields, visited);
                            }
                        }

                        for tf in &td.fields {
                            fields.push(FieldCompletion {
                                name: tf.name.clone(),
                                type_display: format!("{}", tf.type_fact),
                                is_function: is_function_type(&tf.type_fact),
                                def_uri: Some(candidate.source_uri.clone()),
                                def_range: Some(tf.range),
                            });
                        }

                        // Also collect fields from the local table shape
                        // when the class anchor is a local variable. Use the
                        // pre-computed anchor_shape_id to find them directly.
                        if let Some(shape_id) = td.anchor_shape_id {
                            if let Some(shape) = summary.table_shapes.get(&shape_id) {
                                for (fname, fi) in &shape.fields {
                                    if !fields.iter().any(|f| f.name == *fname) {
                                        fields.push(FieldCompletion {
                                            name: fname.clone(),
                                            type_display: format!("{}", fi.type_fact),
                                            is_function: is_function_type(&fi.type_fact),
                                            def_uri: Some(candidate.source_uri.clone()),
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
fn get_generic_param_names(type_name: &str, agg: &WorkspaceAggregation) -> Vec<String> {
    if let Some(candidates) = agg.type_shard.get(type_name) {
        for candidate in candidates {
            if let Some(summary) = agg.summaries.get(&candidate.source_uri) {
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
    agg: &WorkspaceAggregation,
) -> TypeFact {
    // Find the source URI for this type so we can look up function_summaries.
    let source_uri = agg.type_shard.get(type_name)
        .and_then(|candidates| candidates.first())
        .map(|c| c.source_uri.clone());

    if let Some(uri) = source_uri {
        // Try qualified name `TypeName:method` or `TypeName.method` in
        // function_summaries (the summary builder registers colon methods
        // under `TypeName:method`).
        let qualified_colon = format!("{}:{}", type_name, method_name);
        let qualified_dot = format!("{}.{}", type_name, method_name);

        let ret = agg.summaries.get(&uri).and_then(|summary| {
            let fs = summary.get_function_by_name(&qualified_colon)
                .or_else(|| summary.get_function_by_name(&qualified_dot));
            fs.and_then(|fs| fs.signature.returns.first().cloned())
        });

        if let Some(ret_fact) = ret {
            return substitute_generics(&ret_fact, type_name, actual_params, agg);
        }
    }

    // Also try global_shard qualified lookup for `TypeName.method`.
    let qualified = format!("{}.{}", type_name, method_name);
    if let Some(c) = agg.global_shard.get(&qualified).and_then(|v| v.first().cloned()) {
        let mut visited = HashSet::new();
        let resolved = resolve_recursive(&c.type_fact, agg, 0, &mut visited);
        match &resolved.type_fact {
            TypeFact::Known(KnownType::Function(ref sig)) => {
                if let Some(ret) = sig.returns.first() {
                    return substitute_generics(ret, type_name, actual_params, agg);
                }
            }
            TypeFact::Known(KnownType::FunctionRef(fid)) => {
                if let Some(ref uri) = resolved.def_uri {
                    if let Some(summary) = agg.summaries.get(uri) {
                        if let Some(fs) = summary.function_summaries.get(fid) {
                            if let Some(ret) = fs.signature.returns.first() {
                                return substitute_generics(ret, type_name, actual_params, agg);
                            }
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
    param_names: &[String],
    actual_params: &[TypeFact],
) -> TypeFact {
    match fact {
        TypeFact::Known(KnownType::EmmyType(name)) => {
            if let Some(i) = param_names.iter().position(|p| p == name) {
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
            TypeFact::Known(KnownType::EmmyGeneric(name.clone(), substituted))
        }
        TypeFact::Stub(SymbolicStub::TypeRef { name }) => {
            if let Some(i) = param_names.iter().position(|p| p == name) {
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
            let params: Vec<crate::type_system::ParamInfo> = sig.params.iter().map(|p| {
                crate::type_system::ParamInfo {
                    name: p.name.clone(),
                    type_fact: substitute_in_fact(&p.type_fact, param_names, actual_params),
                }
            }).collect();
            let returns: Vec<TypeFact> = sig.returns.iter()
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
    generic_params: &[String],
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
        .map(|(i, b)| {
            b.unwrap_or_else(|| TypeFact::Known(KnownType::EmmyType(generic_params[i].clone())))
        })
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
    generic_params: &[String],
    bindings: &mut [Option<TypeFact>],
) {
    // Skip unknown actuals — they don't contribute useful bindings.
    if matches!(actual, TypeFact::Unknown) {
        return;
    }

    match formal {
        // Direct generic param: `@param x T` → T = actual
        TypeFact::Known(KnownType::EmmyType(name)) => {
            if let Some(i) = generic_params.iter().position(|p| p == name) {
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
