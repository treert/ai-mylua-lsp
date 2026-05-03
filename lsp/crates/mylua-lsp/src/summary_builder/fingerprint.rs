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

pub(super) fn hash_function_signature(sig: &FunctionSignature) -> u64 {
    let mut hasher = DefaultHasher::new();
    hash_signature(sig, &mut hasher);
    hasher.finish()
}

fn hash_signature(sig: &FunctionSignature, hasher: &mut impl Hasher) {
    for p in &sig.params {
        p.name.hash(hasher);
        p.optional.hash(hasher);
        hash_type_fact(&p.type_fact, hasher);
    }
    for r in &sig.returns {
        hash_type_fact(r, hasher);
    }
}

fn hash_type_fact(fact: &TypeFact, hasher: &mut impl Hasher) {
    match fact {
        TypeFact::Known(known) => {
            "known".hash(hasher);
            hash_known_type(known, hasher);
        }
        TypeFact::Stub(stub) => {
            "stub".hash(hasher);
            hash_symbolic_stub(stub, hasher);
        }
        TypeFact::Union(parts) => {
            "union".hash(hasher);
            parts.len().hash(hasher);
            for part in parts {
                hash_type_fact(part, hasher);
            }
        }
        TypeFact::Unknown => {
            "unknown".hash(hasher);
        }
    }
}

fn hash_known_type(known: &KnownType, hasher: &mut impl Hasher) {
    match known {
        KnownType::Nil => "nil".hash(hasher),
        KnownType::Boolean => "boolean".hash(hasher),
        KnownType::Number => "number".hash(hasher),
        KnownType::Integer => "integer".hash(hasher),
        KnownType::String => "string".hash(hasher),
        KnownType::Table(shape_id) => {
            "table".hash(hasher);
            shape_id.0.hash(hasher);
        }
        KnownType::Function(sig) => {
            "function".hash(hasher);
            hash_signature(sig, hasher);
        }
        KnownType::FunctionRef(fid) => {
            "function_ref".hash(hasher);
            fid.0.hash(hasher);
        }
        KnownType::EmmyType(name) => {
            "emmy_type".hash(hasher);
            name.hash(hasher);
        }
        KnownType::EmmyGeneric(name, params) => {
            "emmy_generic".hash(hasher);
            name.hash(hasher);
            params.len().hash(hasher);
            for param in params {
                hash_type_fact(param, hasher);
            }
        }
    }
}

fn hash_symbolic_stub(stub: &SymbolicStub, hasher: &mut impl Hasher) {
    match stub {
        SymbolicStub::RequireRef { module_path } => {
            "require_ref".hash(hasher);
            module_path.hash(hasher);
        }
        SymbolicStub::CallReturn {
            base,
            func_name,
            is_method_call,
            call_arg_types,
            generic_args,
        } => {
            "call_return".hash(hasher);
            hash_symbolic_stub(base, hasher);
            func_name.hash(hasher);
            is_method_call.hash(hasher);
            call_arg_types.len().hash(hasher);
            for arg in call_arg_types {
                hash_type_fact(arg, hasher);
            }
            generic_args.len().hash(hasher);
            for arg in generic_args {
                hash_type_fact(arg, hasher);
            }
        }
        SymbolicStub::GlobalRef { name } => {
            "global_ref".hash(hasher);
            name.hash(hasher);
        }
        SymbolicStub::TypeRef { name } => {
            "type_ref".hash(hasher);
            name.hash(hasher);
        }
        SymbolicStub::FieldOf { base, field } => {
            "field_of".hash(hasher);
            hash_type_fact(base, hasher);
            field.hash(hasher);
        }
    }
}

pub(super) fn compute_signature_fingerprint(ctx: &BuildContext) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Hash global contributions including their type facts
    let mut globals: Vec<_> = ctx.global_contributions.iter().collect();
    globals.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.selection_range.start_byte.cmp(&b.selection_range.start_byte))
    });
    for global in &globals {
        global.name.hash(&mut hasher);
        hash_type_fact(&global.type_fact, &mut hasher);
    }

    // Hash function signatures by ID (not by name)
    let mut func_ids: Vec<_> = ctx.function_summaries.keys().copied().collect();
    func_ids.sort_by_key(|id| id.0);
    for id in &func_ids {
        id.0.hash(&mut hasher);
        if let Some(fs) = ctx.function_summaries.get(id) {
            fs.generic_params.hash(&mut hasher);
            fs.signature_fingerprint.hash(&mut hasher);
        }
    }

    // Hash type definitions: kind, parents, alias, fields
    let mut type_defs: Vec<_> = ctx.type_definitions.iter().collect();
    type_defs.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| format!("{:?}", a.kind).cmp(&format!("{:?}", b.kind)))
            .then_with(|| a.range.start_byte.cmp(&b.range.start_byte))
    });
    for type_def in &type_defs {
        type_def.name.hash(&mut hasher);
        format!("{:?}", type_def.kind).hash(&mut hasher);
        type_def.parents.hash(&mut hasher);
        type_def.generic_params.hash(&mut hasher);
        if let Some(alias) = &type_def.alias_type {
            "alias".hash(&mut hasher);
            hash_type_fact(alias, &mut hasher);
        }
        let mut fields: Vec<_> = type_def.fields.iter().collect();
        fields.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.range.start_byte.cmp(&b.range.start_byte))
        });
        for field in fields {
            field.name.hash(&mut hasher);
            hash_type_fact(&field.type_fact, &mut hasher);
        }
    }

    // Hash module return type
    if let Some(ref ret) = ctx.module_return_type {
        "module_return".hash(&mut hasher);
        hash_type_fact(ret, &mut hasher);
    }

    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_signature_hash_includes_param_optionality() {
        let required = FunctionSignature {
            params: vec![ParamInfo {
                name: "value".to_string(),
                type_fact: TypeFact::Known(KnownType::String),
                optional: false,
            }],
            returns: Vec::new(),
        };
        let optional = FunctionSignature {
            params: vec![ParamInfo {
                name: "value".to_string(),
                type_fact: TypeFact::Known(KnownType::String),
                optional: true,
            }],
            returns: Vec::new(),
        };

        assert_ne!(hash_function_signature(&required), hash_function_signature(&optional));
    }
}
