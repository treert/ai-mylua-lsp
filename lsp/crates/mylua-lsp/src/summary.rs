use std::collections::HashMap;
use tower_lsp_server::ls_types::{Range, Uri};

use crate::table_shape::{TableShape, TableShapeId};
use crate::type_system::{FunctionSignature, TypeFact};

/// Per-file summary: the "recipe" of type facts produced by single-file inference.
///
/// Contains everything needed to participate in the workspace aggregation layer
/// without re-parsing the AST. See `index-architecture.md` §2.1.
#[derive(Debug, Clone)]
pub struct DocumentSummary {
    pub uri: Uri,
    /// Hash of source text; used for cache invalidation.
    pub content_hash: u64,
    /// `local x = require("mod")` bindings.
    pub require_bindings: Vec<RequireBinding>,
    /// Globals defined/extended by this file.
    pub global_contributions: Vec<GlobalContribution>,
    /// Top-level and named function summaries.
    pub function_summaries: HashMap<String, FunctionSummary>,
    /// `---@class`, `---@alias`, `---@enum` definitions.
    pub type_definitions: Vec<TypeDefinition>,
    /// Key local variables' inferred type facts.
    pub local_type_facts: HashMap<String, LocalTypeFact>,
    /// Table shape instances defined in this file.
    pub table_shapes: HashMap<TableShapeId, TableShape>,
    /// Fingerprint of all externally-visible type signatures.
    /// Used for cascade invalidation: if unchanged, dependants don't need revalidation.
    pub signature_fingerprint: u64,
}

/// `local <name> = require("<module_path>")`.
#[derive(Debug, Clone)]
pub struct RequireBinding {
    pub local_name: String,
    pub module_path: String,
    pub range: Range,
}

/// A global name contributed by this file (assignment, function declaration, table extension).
#[derive(Debug, Clone)]
pub struct GlobalContribution {
    pub name: String,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: Range,
    pub selection_range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalContributionKind {
    Variable,
    Function,
    TableExtension,
}

/// Summary of a function's type-level contract.
#[derive(Debug, Clone)]
pub struct FunctionSummary {
    pub name: String,
    pub signature: FunctionSignature,
    /// Range of the full function declaration.
    pub range: Range,
    /// Stable hash of `(params_types, return_types)` for cascade invalidation.
    pub signature_fingerprint: u64,
    /// Table shapes constructed and returned by this function.
    pub returned_shapes: Vec<TableShapeId>,
    /// Whether Emmy annotations are the authority for this function's signature.
    pub emmy_annotated: bool,
}

/// An Emmy type definition (`---@class`, `---@alias`, `---@enum`).
#[derive(Debug, Clone)]
pub struct TypeDefinition {
    pub name: String,
    pub kind: TypeDefinitionKind,
    pub fields: Vec<TypeFieldDef>,
    pub range: Range,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDefinitionKind {
    Class,
    Alias,
    Enum,
}

/// A field declared within `---@class` or `---@field`.
#[derive(Debug, Clone)]
pub struct TypeFieldDef {
    pub name: String,
    pub type_fact: TypeFact,
    pub range: Range,
}

/// Inferred type fact for a key local variable, with provenance.
#[derive(Debug, Clone)]
pub struct LocalTypeFact {
    pub name: String,
    pub type_fact: TypeFact,
    pub source: TypeFactSource,
    pub range: Range,
}

/// Where a local's type information came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeFactSource {
    Assignment,
    CallReturn,
    FieldAccess,
    RequireBinding,
    EmmyAnnotation,
}
