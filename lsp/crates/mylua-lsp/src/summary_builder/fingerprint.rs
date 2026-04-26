use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::type_system::*;

use super::BuildContext;

// ---------------------------------------------------------------------------
// Type merging
// ---------------------------------------------------------------------------

pub(super) fn merge_types(a: TypeFact, b: TypeFact) -> TypeFact {
    if a == b {
        return a;
    }
    match (a, b) {
        (TypeFact::Unknown, other) | (other, TypeFact::Unknown) => other,
        (TypeFact::Union(mut items), other) => {
            if !items.contains(&other) {
                items.push(other);
            }
            TypeFact::Union(items)
        }
        (other, TypeFact::Union(mut items)) => {
            if !items.contains(&other) {
                items.insert(0, other);
            }
            TypeFact::Union(items)
        }
        (a, b) => TypeFact::Union(vec![a, b]),
    }
}

// ---------------------------------------------------------------------------
// Hashing / fingerprints
// ---------------------------------------------------------------------------

pub(super) fn hash_bytes(data: &[u8]) -> u64 {
    crate::util::hash_bytes(data)
}

pub(super) fn hash_function_signature(sig: &FunctionSignature) -> u64 {
    let mut hasher = DefaultHasher::new();
    for p in &sig.params {
        p.name.hash(&mut hasher);
        format!("{}", p.type_fact).hash(&mut hasher);
    }
    for r in &sig.returns {
        format!("{}", r).hash(&mut hasher);
    }
    hasher.finish()
}

pub(super) fn compute_signature_fingerprint(ctx: &BuildContext) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Hash require bindings (affect cross-file resolution)
    let mut requires: Vec<_> = ctx.require_bindings.iter()
        .map(|r| (&r.local_name, &r.module_path))
        .collect();
    requires.sort();
    for (name, path) in &requires {
        name.hash(&mut hasher);
        path.hash(&mut hasher);
    }

    // Hash global contributions including their type facts
    let mut globals: Vec<_> = ctx.global_contributions.iter()
        .map(|g| (g.name.as_str(), format!("{}", g.type_fact)))
        .collect();
    globals.sort();
    for (name, type_str) in &globals {
        name.hash(&mut hasher);
        type_str.hash(&mut hasher);
    }

    // Hash function signatures by ID (not by name)
    let mut func_ids: Vec<_> = ctx.function_summaries.keys().copied().collect();
    func_ids.sort_by_key(|id| id.0);
    for id in &func_ids {
        id.0.hash(&mut hasher);
        if let Some(fs) = ctx.function_summaries.get(id) {
            fs.signature_fingerprint.hash(&mut hasher);
        }
    }

    // Hash type definitions: kind, parents, alias, fields
    let mut type_defs: Vec<_> = ctx.type_definitions.iter()
        .map(|t| {
            let fields_str: String = t.fields.iter()
                .map(|f| format!("{}:{}", f.name, f.type_fact))
                .collect::<Vec<_>>()
                .join(",");
            let alias_str = t.alias_type.as_ref()
                .map(|a| format!("{}", a))
                .unwrap_or_default();
            let parents_str = t.parents.join(",");
            let kind_str = format!("{:?}", t.kind);
            (t.name.as_str(), kind_str, parents_str, alias_str, fields_str)
        })
        .collect();
    type_defs.sort();
    for (name, kind, parents, alias, fields) in &type_defs {
        name.hash(&mut hasher);
        kind.hash(&mut hasher);
        parents.hash(&mut hasher);
        alias.hash(&mut hasher);
        fields.hash(&mut hasher);
    }

    // Hash module return type
    if let Some(ref ret) = ctx.module_return_type {
        "module_return".hash(&mut hasher);
        format!("{}", ret).hash(&mut hasher);
    }

    hasher.finish()
}
