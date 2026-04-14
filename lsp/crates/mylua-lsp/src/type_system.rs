use std::fmt;

use crate::table_shape::TableShapeId;

/// A type fact produced by single-file inference.
///
/// `Known` variants are fully resolved within the file.
/// `Stub` variants require cross-file resolution via the aggregation layer.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeFact {
    Known(KnownType),
    Stub(SymbolicStub),
    Union(Vec<TypeFact>),
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub enum KnownType {
    Nil,
    Boolean,
    Number,
    Integer,
    String,
    Table(TableShapeId),
    Function(FunctionSignature),
    EmmyType(String),
}

/// Placeholder that defers resolution to cross-file analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum SymbolicStub {
    /// `local x = require("mod")` — resolve to target file's return type.
    RequireRef { module_path: String },

    /// `local x = base.func_name()` — resolve to function return type.
    CallReturn {
        base: Box<SymbolicStub>,
        func_name: String,
    },

    /// Reference to a global name, resolved via GlobalShard.
    GlobalRef { name: String },

    /// Reference to an Emmy type name, resolved via TypeShard.
    TypeRef { name: String },

    /// `base.field` — resolve base, then look up field.
    FieldOf {
        base: Box<TypeFact>,
        field: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSignature {
    pub params: Vec<ParamInfo>,
    pub returns: Vec<TypeFact>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamInfo {
    pub name: String,
    pub type_fact: TypeFact,
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
