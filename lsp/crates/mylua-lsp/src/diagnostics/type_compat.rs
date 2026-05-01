use crate::type_system::{TypeFact, KnownType, SymbolicStub};
use crate::util::node_text;

pub(crate) fn infer_literal_type(
    node: tree_sitter::Node,
    source: &[u8],
    scope_tree: &crate::scope::ScopeTree,
) -> TypeFact {
    match node.kind() {
        "number" => TypeFact::Known(KnownType::Number),
        "string" => TypeFact::Known(KnownType::String),
        "true" | "false" => TypeFact::Known(KnownType::Boolean),
        "nil" => TypeFact::Known(KnownType::Nil),
        "table_constructor" => TypeFact::Known(KnownType::Table(crate::table_shape::TableShapeId(u32::MAX))),
        "function_definition" => TypeFact::Known(KnownType::Function(crate::type_system::FunctionSignature {
            params: vec![], returns: vec![],
        })),
        "variable" | "identifier" => {
            let text = node_text(node, source);
            if let Some(decl) = scope_tree.resolve_decl(node.start_byte(), text) {
                if !decl.is_emmy_annotated {
                    if let Some(ref tf) = decl.type_fact {
                        return tf.clone();
                    }
                }
            }
            TypeFact::Unknown
        }
        _ => TypeFact::Unknown,
    }
}

pub(crate) fn is_type_compatible(declared: &TypeFact, actual: &TypeFact) -> bool {
    match (declared, actual) {
        (TypeFact::Unknown, _) | (_, TypeFact::Unknown) => true,
        (TypeFact::Known(d), TypeFact::Known(a)) => known_types_compatible(d, a),
        (TypeFact::Union(types), actual) => {
            types.iter().any(|t| is_type_compatible(t, actual))
        }
        (declared, TypeFact::Union(types)) => {
            types.iter().all(|t| is_type_compatible(declared, t))
        }
        (TypeFact::Stub(SymbolicStub::TypeRef { name }), TypeFact::Known(a)) => {
            is_named_type_compatible(name, a, &[])
        }
        _ => true,
    }
}

pub(crate) fn is_named_type_compatible(name: &str, actual: &KnownType, declared_params: &[TypeFact]) -> bool {
    match (name, actual) {
        ("string", KnownType::String) => true,
        ("number" | "integer", KnownType::Number | KnownType::Integer) => true,
        ("boolean", KnownType::Boolean) => true,
        ("nil", KnownType::Nil) => true,
        ("table", KnownType::Table(_)) => true,
        ("__array", KnownType::Table(_)) => true,
        ("__array", KnownType::EmmyGeneric(n, _)) if n == "__array" => true,
        ("function", KnownType::Function(_)) => true,
        ("function", KnownType::FunctionRef(_)) => true,
        ("any", _) => true,
        (_, KnownType::Nil) => true,
        ("string", KnownType::Number | KnownType::Boolean) => false,
        ("number" | "integer", KnownType::String | KnownType::Boolean) => false,
        ("boolean", KnownType::String | KnownType::Number) => false,
        // For EmmyGeneric: check if name matches and parameters are compatible
        (n, KnownType::EmmyGeneric(actual_name, actual_params)) if n == actual_name => {
            // If declared side has no parameters, allow any actual parameters (backwards compat)
            if declared_params.is_empty() {
                return true;
            }
            // If parameter counts differ, they're incompatible
            if declared_params.len() != actual_params.len() {
                return false;
            }
            // Recursively check all parameters for compatibility
            declared_params.iter().zip(actual_params.iter()).all(|(d, a)| {
                is_type_compatible(d, a)
            })
        }
        _ => true,
    }
}

pub(crate) fn known_types_compatible(declared: &KnownType, actual: &KnownType) -> bool {
    match (declared, actual) {
        (KnownType::Nil, _) | (_, KnownType::Nil) => true,
        (KnownType::Number, KnownType::Number | KnownType::Integer) => true,
        (KnownType::Integer, KnownType::Number | KnownType::Integer) => true,
        (KnownType::String, KnownType::String) => true,
        (KnownType::Boolean, KnownType::Boolean) => true,
        (KnownType::Table(_), KnownType::Table(_)) => true,
        // __array<T> is compatible with table and vice versa
        (KnownType::EmmyGeneric(name, _), KnownType::Table(_)) if name == "__array" => true,
        (KnownType::Table(_), KnownType::EmmyGeneric(name, _)) if name == "__array" => true,
        (KnownType::Function(_) | KnownType::FunctionRef(_), KnownType::Function(_) | KnownType::FunctionRef(_)) => true,
        // Handle EmmyGeneric vs EmmyGeneric comparison: check parameters
        (KnownType::EmmyGeneric(d_name, d_params), KnownType::EmmyGeneric(a_name, a_params)) => {
            // Names must match
            if d_name != a_name {
                return false;
            }
            // If declared has no parameters, accept any actual parameters (backwards compat)
            if d_params.is_empty() {
                return true;
            }
            // If parameter counts differ, they're incompatible
            if d_params.len() != a_params.len() {
                return false;
            }
            // Recursively check all parameters for compatibility
            d_params.iter().zip(a_params.iter()).all(|(d, a)| {
                is_type_compatible(d, a)
            })
        }
        (KnownType::EmmyType(name), actual) | (KnownType::EmmyGeneric(name, _), actual) => {
            is_named_type_compatible(name, actual, &[])
        }
        (KnownType::String, KnownType::Number | KnownType::Integer | KnownType::Boolean) => false,
        (KnownType::Number | KnownType::Integer, KnownType::String | KnownType::Boolean) => false,
        (KnownType::Boolean, KnownType::String | KnownType::Number | KnownType::Integer) => false,
        (KnownType::Table(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Function(_) | KnownType::FunctionRef(_)) => false,
        (KnownType::Function(_) | KnownType::FunctionRef(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Table(_)) => false,
        _ => true,
    }
}

/// Extended version of `infer_literal_type` that also allows
/// `EmmyAnnotation`-sourced locals to contribute their declared
/// type. This is what call-site argument checking wants — if
/// `local s ---@type string = f()` appears, then passing `s` to a
/// `@param n number` slot should be diagnosable. The original
/// `infer_literal_type` deliberately refuses Emmy-sourced locals
/// because the initial `local` declaration's mismatch check would
/// otherwise be circular.
pub(crate) fn infer_argument_type(
    node: tree_sitter::Node,
    source: &[u8],
    scope_tree: &crate::scope::ScopeTree,
) -> TypeFact {
    if matches!(node.kind(), "variable" | "identifier") {
        let text = node_text(node, source);
        if let Some(tf) = scope_tree.resolve_type(node.start_byte(), text) {
            return tf.clone();
        }
    }
    infer_literal_type(node, source, scope_tree)
}

/// Literal-only type inference for return values. Avoids the summary
/// dependency of `infer_literal_type` so this walk can run without a
/// summary in scope (keeps the diagnostic self-contained).
pub(crate) fn infer_return_literal_type(node: tree_sitter::Node) -> TypeFact {
    match node.kind() {
        "number" => TypeFact::Known(KnownType::Number),
        "string" => TypeFact::Known(KnownType::String),
        "true" | "false" => TypeFact::Known(KnownType::Boolean),
        "nil" => TypeFact::Known(KnownType::Nil),
        "table_constructor" => {
            TypeFact::Known(KnownType::Table(crate::table_shape::TableShapeId(u32::MAX)))
        }
        "function_definition" => TypeFact::Known(KnownType::Function(
            crate::type_system::FunctionSignature {
                params: vec![],
                returns: vec![],
            },
        )),
        _ => TypeFact::Unknown,
    }
}
