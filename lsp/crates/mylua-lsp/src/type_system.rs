use std::fmt;

use serde::{Deserialize, Serialize};

use crate::table_shape::TableShapeId;

/// A type fact produced by single-file inference.
///
/// `Known` variants are fully resolved within the file.
/// `Stub` variants require cross-file resolution via the aggregation layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypeFact {
    Known(KnownType),
    Stub(SymbolicStub),
    Union(Vec<TypeFact>),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum KnownType {
    Nil,
    Boolean,
    Number,
    Integer,
    String,
    Table(TableShapeId),
    Function(FunctionSignature),
    EmmyType(std::string::String),
    EmmyGeneric(std::string::String, Vec<TypeFact>),
}

/// Placeholder that defers resolution to cross-file analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SymbolicStub {
    /// `local x = require("mod")` — resolve to target file's return type.
    RequireRef { module_path: std::string::String },

    /// `local x = base.func_name()` — resolve to function return type.
    CallReturn {
        base: Box<SymbolicStub>,
        func_name: std::string::String,
    },

    /// Reference to a global name, resolved via GlobalShard.
    GlobalRef { name: std::string::String },

    /// Reference to an Emmy type name, resolved via TypeShard.
    TypeRef { name: std::string::String },

    /// `base.field` — resolve base, then look up field.
    FieldOf {
        base: Box<TypeFact>,
        field: std::string::String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionSignature {
    pub params: Vec<ParamInfo>,
    pub returns: Vec<TypeFact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamInfo {
    pub name: std::string::String,
    pub type_fact: TypeFact,
}

/// Recursively replace `EmmyType("self")` / `EmmyGeneric("self", …)`
/// references inside `fact` with the supplied `class_name`. Used by
/// `summary_builder` when building method signatures so that
/// `---@return self` on a class method surfaces as the method's
/// owner class, which is what the user expects for fluent / builder
/// APIs like `obj:chain():chain2()`.
///
/// A no-op when `class_name` is empty (free functions / top-level
/// code); conservative about other type variants (unions recurse,
/// functions recurse into params + returns, tables / generics too).
pub fn substitute_self(fact: &TypeFact, class_name: &str) -> TypeFact {
    if class_name.is_empty() {
        return fact.clone();
    }
    match fact {
        TypeFact::Known(KnownType::EmmyType(name)) if name == "self" => {
            TypeFact::Known(KnownType::EmmyType(class_name.to_string()))
        }
        TypeFact::Known(KnownType::EmmyGeneric(name, args)) if name == "self" => {
            let new_args: Vec<TypeFact> = args.iter().map(|a| substitute_self(a, class_name)).collect();
            TypeFact::Known(KnownType::EmmyGeneric(class_name.to_string(), new_args))
        }
        TypeFact::Known(KnownType::EmmyGeneric(name, args)) => {
            let new_args: Vec<TypeFact> = args.iter().map(|a| substitute_self(a, class_name)).collect();
            TypeFact::Known(KnownType::EmmyGeneric(name.clone(), new_args))
        }
        TypeFact::Known(KnownType::Function(sig)) => {
            let params = sig
                .params
                .iter()
                .map(|p| ParamInfo {
                    name: p.name.clone(),
                    type_fact: substitute_self(&p.type_fact, class_name),
                })
                .collect();
            let returns = sig.returns.iter().map(|r| substitute_self(r, class_name)).collect();
            TypeFact::Known(KnownType::Function(FunctionSignature { params, returns }))
        }
        TypeFact::Union(parts) => {
            TypeFact::Union(parts.iter().map(|p| substitute_self(p, class_name)).collect())
        }
        other => other.clone(),
    }
}

/// Extract the class name from a qualified function name. Returns
/// `""` for bare / dotted-without-class / top-level names.
///
/// - `Foo:m` → `Foo`
/// - `Foo.m` → `Foo`
/// - `a.b.c` → `a.b`   (dotted — treat everything before the last `.` as container)
/// - `standalone` → `""`
pub fn class_prefix_of(name: &str) -> &str {
    if let Some(idx) = name.rfind(|c: char| c == ':' || c == '.') {
        &name[..idx]
    } else {
        ""
    }
}

impl fmt::Display for TypeFact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Known(k) => write!(f, "{}", k),
            Self::Stub(s) => write!(f, "{}", s),
            Self::Union(types) => {
                for (i, t) in types.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{}", t)?;
                }
                Ok(())
            }
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl fmt::Display for KnownType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nil => write!(f, "nil"),
            Self::Boolean => write!(f, "boolean"),
            Self::Number => write!(f, "number"),
            Self::Integer => write!(f, "integer"),
            Self::String => write!(f, "string"),
            Self::Table(id) => write!(f, "table<{}>", id.0),
            Self::Function(_) => write!(f, "function"),
            Self::EmmyType(name) => write!(f, "{}", name),
            Self::EmmyGeneric(name, params) => {
                write!(f, "{}<{}>", name, params.iter().map(|p| format!("{}", p)).collect::<Vec<_>>().join(", "))
            }
        }
    }
}

impl fmt::Display for SymbolicStub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RequireRef { module_path } => write!(f, "require(\"{}\")", module_path),
            Self::CallReturn { base, func_name } => write!(f, "{}.{}()", base, func_name),
            Self::GlobalRef { name } => write!(f, "global:{}", name),
            Self::TypeRef { name } => write!(f, "type:{}", name),
            Self::FieldOf { base, field } => write!(f, "{}.{}", base, field),
        }
    }
}
