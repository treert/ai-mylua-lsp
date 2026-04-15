use std::collections::HashSet;
use tower_lsp_server::ls_types::{Range, Uri};

use crate::aggregation::{CacheKey, CachedResolution, WorkspaceAggregation};
use crate::table_shape::TableShapeId;
use crate::type_system::*;

const MAX_RESOLVE_DEPTH: usize = 32;

/// Result of resolving a type, with optional source location for goto.
#[derive(Debug, Clone)]
pub struct ResolvedType {
    pub type_fact: TypeFact,
    pub def_uri: Option<Uri>,
    pub def_range: Option<Range>,
}

impl ResolvedType {
    fn unknown() -> Self {
        Self { type_fact: TypeFact::Unknown, def_uri: None, def_range: None }
    }

    fn from_fact(fact: TypeFact) -> Self {
        Self { type_fact: fact, def_uri: None, def_range: None }
    }

    fn with_location(fact: TypeFact, uri: Uri, range: Range) -> Self {
        Self { type_fact: fact, def_uri: Some(uri), def_range: Some(range) }
    }
}

/// Resolve a `TypeFact` (which may contain stubs) to a fully resolved type
/// using the workspace aggregation layer.
pub fn resolve_type(
    fact: &TypeFact,
    agg: &mut WorkspaceAggregation,
) -> ResolvedType {
    let mut visited = HashSet::new();
    resolve_recursive(fact, agg, 0, &mut visited)
}

/// Resolve a chain of field accesses like `obj.pos.x`.
/// Returns the resolved type of the final field.
pub fn resolve_field_chain(
    base_fact: &TypeFact,
    fields: &[String],
    agg: &mut WorkspaceAggregation,
) -> ResolvedType {
    let mut visited = HashSet::new();
    let mut current = resolve_recursive(base_fact, agg, 0, &mut visited);

    for field in fields {
        current = resolve_field_access(&current.type_fact, field, agg, 0, &mut visited);
    }

    current
}

/// Given a file URI and a local variable name, resolve its type from the summary.
pub fn resolve_local_in_file(
    uri: &Uri,
    local_name: &str,
    agg: &mut WorkspaceAggregation,
) -> ResolvedType {
    let fact = {
        let summary = match agg.summaries.get(uri) {
            Some(s) => s,
            None => return ResolvedType::unknown(),
        };
        match summary.local_type_facts.get(local_name) {
            Some(ltf) => ltf.type_fact.clone(),
            None => return ResolvedType::unknown(),
        }
    };
    resolve_type(&fact, agg)
}

/// Get completable fields for a type (for dot-completion).
pub fn get_fields_for_type(
    fact: &TypeFact,
    agg: &mut WorkspaceAggregation,
) -> Vec<FieldCompletion> {
    let resolved = resolve_type(fact, agg);
    collect_fields(&resolved.type_fact, agg)
}

#[derive(Debug, Clone)]
pub struct FieldCompletion {
    pub name: String,
    pub type_display: String,
    pub def_uri: Option<Uri>,
    pub def_range: Option<Range>,
}

// ---------------------------------------------------------------------------
// Internal recursive resolver
// ---------------------------------------------------------------------------

fn resolve_recursive(
    fact: &TypeFact,
    agg: &mut WorkspaceAggregation,
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
    agg: &mut WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let visit_key = format!("{}", stub);
    if visited.contains(&visit_key) {
        return ResolvedType::unknown();
    }
    visited.insert(visit_key.clone());

    // Check resolution cache
    if let Some(cache_key) = stub_to_cache_key(stub) {
        if let Some(cached) = agg.resolution_cache.get(&cache_key) {
            if !cached.dirty {
                let result = ResolvedType::from_fact(cached.resolved_type.clone());
                visited.remove(&visit_key);
                return result;
            }
        }
    }

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

        SymbolicStub::CallReturn { base, func_name } => {
            resolve_call_return(base, func_name, agg, depth, visited)
        }

        SymbolicStub::FieldOf { base, field } => {
            resolve_field_access(base, field, agg, depth, visited)
        }
    };

    // Cache the result
    if let Some(cache_key) = stub_to_cache_key(stub) {
        agg.resolution_cache.insert(cache_key, CachedResolution {
            resolved_type: result.type_fact.clone(),
            dirty: false,
        });
    }

    visited.remove(&visit_key);
    result
}

fn resolve_require(
    module_path: &str,
    agg: &mut WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let target_uri = match agg.require_map.get(module_path) {
        Some(u) => u.clone(),
        None => return ResolvedType::unknown(),
    };

    // Look at the target file's summary for its return type
    let return_fact = {
        let summary = match agg.summaries.get(&target_uri) {
            Some(s) => s,
            None => return ResolvedType::unknown(),
        };

        // Check for a function named at the module level that represents the return
        // For now, collect all global contributions as a table-like shape
        if summary.global_contributions.is_empty() && summary.function_summaries.is_empty() {
            return ResolvedType::unknown();
        }

        // If the module has a single function summary as its main export,
        // or global contributions, build a type from them
        TypeFact::Stub(SymbolicStub::GlobalRef {
            name: module_path.to_string(),
        })
    };

    let mut result = resolve_recursive(&return_fact, agg, depth + 1, visited);
    result.def_uri = Some(target_uri);
    result
}

fn resolve_global(
    name: &str,
    agg: &mut WorkspaceAggregation,
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
    agg: &mut WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let base_resolved = resolve_stub(base, agg, depth + 1, visited);

    // If base resolved to a known type, look for the function in its source
    if let Some(ref uri) = base_resolved.def_uri {
        let return_type = {
            let summary = match agg.summaries.get(uri) {
                Some(s) => s,
                None => return ResolvedType::unknown(),
            };

            if let Some(fs) = summary.function_summaries.get(func_name) {
                if let Some(ret) = fs.signature.returns.first() {
                    Some(ret.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };

        if let Some(ret) = return_type {
            return resolve_recursive(&ret, agg, depth + 1, visited);
        }
    }

    // Try looking up `base_name.func_name` as a global
    if let SymbolicStub::GlobalRef { name: base_name } | SymbolicStub::RequireRef { module_path: base_name } = base {
        let qualified = format!("{}.{}", base_name, func_name);
        let candidates = agg.global_shard.get(&qualified).cloned();
        if let Some(candidates) = candidates {
            if let Some(c) = candidates.first() {
                return ResolvedType::with_location(
                    c.type_fact.clone(),
                    c.source_uri.clone(),
                    c.selection_range,
                );
            }
        }

        // Also check function summaries across all files
        for (_, summary) in &agg.summaries {
            if let Some(fs) = summary.function_summaries.get(&qualified) {
                if let Some(ret) = fs.signature.returns.first() {
                    return ResolvedType::from_fact(ret.clone());
                }
            }
        }
    }

    ResolvedType::unknown()
}

fn resolve_field_access(
    base: &TypeFact,
    field: &str,
    agg: &mut WorkspaceAggregation,
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

        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => {
            // Try `name.field` as a global
            let qualified = format!("{}.{}", name, field);
            let candidates = agg.global_shard.get(&qualified).cloned();
            if let Some(candidates) = candidates {
                if let Some(c) = candidates.first() {
                    return ResolvedType::with_location(
                        c.type_fact.clone(),
                        c.source_uri.clone(),
                        c.selection_range,
                    );
                }
            }

            // Also try looking up function summaries
            for (_, summary) in &agg.summaries {
                if let Some(fs) = summary.function_summaries.get(&qualified) {
                    return ResolvedType::from_fact(TypeFact::Known(KnownType::Function(
                        fs.signature.clone(),
                    )));
                }
            }

            ResolvedType::unknown()
        }

        TypeFact::Union(types) => {
            // Try each union member; return first successful resolution
            for t in types {
                let result = resolve_field_access(t, field, agg, depth + 1, visited);
                if result.type_fact != TypeFact::Unknown {
                    return result;
                }
            }
            ResolvedType::unknown()
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
    // Search all summaries for the table shape
    for (uri, summary) in &agg.summaries {
        if let Some(source) = source_uri {
            if uri != source {
                continue;
            }
        }
        if let Some(shape) = summary.table_shapes.get(&shape_id) {
            if let Some(fi) = shape.fields.get(field) {
                return ResolvedType {
                    type_fact: fi.type_fact.clone(),
                    def_uri: Some(uri.clone()),
                    def_range: fi.def_range,
                };
            }
        }
    }
    ResolvedType::unknown()
}

fn resolve_emmy_field(
    type_name: &str,
    field: &str,
    agg: &WorkspaceAggregation,
) -> ResolvedType {
    // First, try @field annotations in the class definition
    if let Some(candidates) = agg.type_shard.get(type_name) {
        lsp_log!("[resolve_emmy_field] type '{}' has {} candidates", type_name, candidates.len());

        for candidate in candidates {
            if let Some(summary) = agg.summaries.get(&candidate.source_uri) {
                for td in &summary.type_definitions {
                    if td.name == type_name {
                        lsp_log!("[resolve_emmy_field] found class '{}' with {} fields: {:?}",
                            type_name, td.fields.len(),
                            td.fields.iter().map(|f| &f.name).collect::<Vec<_>>());
                        for tf in &td.fields {
                            if tf.name == field {
                                return ResolvedType {
                                    type_fact: tf.type_fact.clone(),
                                    def_uri: Some(candidate.source_uri.clone()),
                                    def_range: Some(tf.range),
                                };
                            }
                        }
                    }
                }
            }
        }
    } else {
        lsp_log!("[resolve_emmy_field] type '{}' not found in type_shard", type_name);
    }

    // Fallback: check global_shard for `type_name.field` — handles functions
    // defined as `function ClassName.MethodName()` without explicit @field.
    // These are registered in global_shard via visit_function_declaration, so
    // an O(1) HashMap lookup is sufficient (no summaries scan needed).
    let qualified = format!("{}.{}", type_name, field);
    if let Some(global_candidates) = agg.global_shard.get(&qualified) {
        if let Some(c) = global_candidates.first() {
            lsp_log!("[resolve_emmy_field] found '{}' via global_shard fallback", qualified);
            return ResolvedType::with_location(
                c.type_fact.clone(),
                c.source_uri.clone(),
                c.selection_range,
            );
        }
    }

    ResolvedType::unknown()
}

fn collect_fields(
    fact: &TypeFact,
    agg: &WorkspaceAggregation,
) -> Vec<FieldCompletion> {
    let mut fields = Vec::new();

    match fact {
        TypeFact::Known(KnownType::Table(shape_id)) => {
            // Only take fields from the first summary that has this shape ID
            // to avoid cross-file collisions (shape IDs are per-file counters).
            for (uri, summary) in &agg.summaries {
                if let Some(shape) = summary.table_shapes.get(shape_id) {
                    for (name, fi) in &shape.fields {
                        fields.push(FieldCompletion {
                            name: name.clone(),
                            type_display: format!("{}", fi.type_fact),
                            def_uri: Some(uri.clone()),
                            def_range: fi.def_range,
                        });
                    }
                    break;
                }
            }
        }

        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            if let Some(candidates) = agg.type_shard.get(type_name.as_str()) {
                for candidate in candidates {
                    if let Some(summary) = agg.summaries.get(&candidate.source_uri) {
                        for td in &summary.type_definitions {
                            if td.name == *type_name {
                                for tf in &td.fields {
                                    fields.push(FieldCompletion {
                                        name: tf.name.clone(),
                                        type_display: format!("{}", tf.type_fact),
                                        def_uri: Some(candidate.source_uri.clone()),
                                        def_range: Some(tf.range),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        TypeFact::Stub(SymbolicStub::GlobalRef { name }) => {
            // Collect all `name.xxx` globals as fields
            let prefix = format!("{}.", name);
            for (gname, candidates) in &agg.global_shard {
                if let Some(field_name) = gname.strip_prefix(&prefix) {
                    if !field_name.contains('.') {
                        if let Some(c) = candidates.first() {
                            fields.push(FieldCompletion {
                                name: field_name.to_string(),
                                type_display: format!("{}", c.type_fact),
                                def_uri: Some(c.source_uri.clone()),
                                def_range: Some(c.selection_range),
                            });
                        }
                    }
                }
            }
        }

        TypeFact::Union(types) => {
            for t in types {
                fields.extend(collect_fields(t, agg));
            }
            fields.sort_by(|a, b| a.name.cmp(&b.name));
            fields.dedup_by(|a, b| a.name == b.name);
        }

        _ => {}
    }

    fields
}

// ---------------------------------------------------------------------------
// Cache key mapping
// ---------------------------------------------------------------------------

fn stub_to_cache_key(stub: &SymbolicStub) -> Option<CacheKey> {
    match stub {
        SymbolicStub::RequireRef { module_path } => {
            Some(CacheKey::RequireReturn { module_path: module_path.clone() })
        }
        SymbolicStub::GlobalRef { name } => {
            Some(CacheKey::GlobalField { global_name: name.clone(), field: String::new() })
        }
        SymbolicStub::FieldOf { base, field } => {
            if let TypeFact::Stub(base_stub) = base.as_ref() {
                let base_key = stub_to_cache_key(base_stub)?;
                Some(CacheKey::CallReturn {
                    base_key: Box::new(base_key),
                    func_name: field.clone(),
                })
            } else {
                None
            }
        }
        SymbolicStub::CallReturn { base, func_name } => {
            let base_key = stub_to_cache_key(base)?;
            Some(CacheKey::CallReturn {
                base_key: Box::new(base_key),
                func_name: func_name.clone(),
            })
        }
        SymbolicStub::TypeRef { name } => {
            Some(CacheKey::TypeField { type_name: name.clone(), field: String::new() })
        }
    }
}
