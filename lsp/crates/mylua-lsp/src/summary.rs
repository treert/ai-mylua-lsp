use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use tower_lsp_server::ls_types::Uri;

use crate::table_shape::{TableShape, TableShapeId};
use crate::type_system::{FunctionSignature, FunctionSummaryId, TypeFact};
use crate::util::ByteRange;

/// Per-file summary: the "recipe" of type facts produced by single-file inference.
///
/// Contains everything needed to participate in the workspace aggregation layer
/// without re-parsing the AST. See `index-architecture.md` §2.1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub uri: Uri,
    /// Hash of source text; used for cache invalidation.
    pub content_hash: u64,
    /// `local x = require("mod")` bindings.
    pub require_bindings: Vec<RequireBinding>,
    /// Globals defined/extended by this file.
    pub global_contributions: Vec<GlobalContribution>,
    /// Top-level and named function summaries, keyed by `FunctionSummaryId`.
    /// Note: Must maintain a reverse mapping (name → ID) in callers that need
    /// to look up functions by name. See `summary_builder/mod.rs::BuildContext::function_name_to_id`.
    pub function_summaries: HashMap<FunctionSummaryId, FunctionSummary>,
    /// Reverse index: function name → FunctionSummaryId.
    /// Only contains **global** functions. Colon-separated names are normalized
    /// to dot (e.g. `"Player:new"` → `"Player.new"`).
    /// Local functions are accessed via `local_type_facts` → `FunctionRef(id)` instead.
    #[serde(default)]
    pub function_name_index: HashMap<String, FunctionSummaryId>,
    /// `---@class`, `---@alias`, `---@enum` definitions.
    pub type_definitions: Vec<TypeDefinition>,
    /// Key local variables' inferred type facts.
    pub local_type_facts: HashMap<String, LocalTypeFact>,
    /// Table shape instances defined in this file.
    pub table_shapes: HashMap<TableShapeId, TableShape>,
    /// Type of the file-level `return` statement (module export).
    /// `None` if the file has no top-level return.
    pub module_return_type: Option<TypeFact>,
    /// Source range of the file-level `return` statement, used by `require`
    /// goto-definition to jump to the module's export. `None` if the file
    /// has no top-level return.
    #[serde(default)]
    pub module_return_range: Option<ByteRange>,
    /// Fingerprint of all externally-visible type signatures.
    /// Used for cascade invalidation: if unchanged, dependants don't need revalidation.
    pub signature_fingerprint: u64,
    /// Call sites captured from this file's function bodies (and
    /// top-level code). Feeds `call_hierarchy` incoming/outgoing
    /// queries without requiring the tree to be re-parsed at query
    /// time. `#[serde(default)]` keeps cached summaries produced by
    /// older builds readable.
    #[serde(default)]
    pub call_sites: Vec<CallSite>,
    /// `true` when this file carries a top-level `---@meta` annotation.
    /// Meta files are treated as stub / definition sources per the
    /// Lua-LS convention: their globals still populate `global_shard`
    /// (so references elsewhere resolve), but diagnostics that reason
    /// about runtime behavior (like `undefinedGlobal`) are suppressed
    /// inside the meta file itself — meta files often reference
    /// runtime-provided symbols that don't have a declaration in the
    /// workspace.
    #[serde(default)]
    pub is_meta: bool,
    /// Optional module name supplied via `---@meta <name>`; purely
    /// informational at present (no require_map mapping yet).
    #[serde(default)]
    pub meta_name: Option<String>,
    /// Type names referenced by local variable type facts.
    /// Pre-computed during build so `collect_referenced_type_names` in
    /// aggregation doesn't need `local_type_facts` — prepares for
    /// their removal.
    #[serde(default)]
    pub referenced_local_type_names: std::collections::HashSet<String>,
}

/// One `function_call` occurrence recorded during summary build.
/// `callee_name` preserves the full dotted / colon-qualified form
/// (`m.sub.foo`, `obj:bar`) so consumers can do exact or
/// last-segment matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallSite {
    /// Full callee text (e.g. `foo`, `m.sub.foo`, `obj:bar`).
    pub callee_name: String,
    /// Enclosing function's (possibly qualified) name. Empty string
    /// when the call is at file top level.
    pub caller_name: String,
    /// Range of the callee identifier / final segment — not the
    /// whole `function_call` node. Preferred by clients as the
    /// highlight target.
    pub range: ByteRange,
}

/// `local <name> = require("<module_path>")`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequireBinding {
    pub local_name: String,
    pub module_path: String,
    pub range: ByteRange,
}

/// A global name contributed by this file (assignment, function declaration, table extension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalContribution {
    pub name: String,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: ByteRange,
    pub selection_range: ByteRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlobalContributionKind {
    Variable,
    Function,
    TableExtension,
}

/// Summary of a function's type-level contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSummary {
    pub name: String,
    pub signature: FunctionSignature,
    /// Range of the full function declaration.
    pub range: ByteRange,
    /// Stable hash of `(params_types, return_types)` for cascade invalidation.
    pub signature_fingerprint: u64,
    /// Whether Emmy annotations are the authority for this function's signature.
    pub emmy_annotated: bool,
    /// Alternative signatures from `---@overload` annotations.
    pub overloads: Vec<FunctionSignature>,
    /// Function-level generic type parameter names from `---@generic T, K`.
    /// Empty for non-generic functions. Used by the resolver to perform
    /// call-site generic argument inference (unification).
    #[serde(default)]
    pub generic_params: Vec<String>,
}

/// An Emmy type definition (`---@class`, `---@alias`, `---@enum`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDefinition {
    pub name: String,
    pub kind: TypeDefinitionKind,
    pub parents: Vec<String>,
    pub fields: Vec<TypeFieldDef>,
    pub alias_type: Option<TypeFact>,
    /// Names of generic type parameters (from `---@generic T, K`).
    #[serde(default)]
    pub generic_params: Vec<String>,
    /// Full range of the declaration anchor — for a class this is the
    /// following statement that anchors the class value (`Foo = {}`);
    /// for alias/enum it is the range of the emmy_comment node itself.
    /// Used by clients that want to highlight the whole construct.
    pub range: ByteRange,
    /// Range of just the `Foo` identifier within `---@class Foo`,
    /// `---@alias Foo ...`, or `---@enum Foo`. Used as the
    /// `selection_range` in `documentSymbol` and as the highlight
    /// target in `workspace/symbol` so that clicking the outline entry
    /// jumps precisely to the type name rather than the whole line.
    /// Falls back to `range` for legacy summaries (`#[serde(default)]`).
    #[serde(default)]
    pub name_range: Option<ByteRange>,
    /// When the `@class` anchors a local table (`local M = {}`), stores the
    /// shape ID so cross-file consumers can look up fields directly.
    #[serde(default)]
    pub anchor_shape_id: Option<crate::table_shape::TableShapeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeDefinitionKind {
    Class,
    Alias,
    Enum,
}

/// A field declared within `---@class` or `---@field`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeFieldDef {
    pub name: String,
    pub type_fact: TypeFact,
    /// Full `---@field ...` line range.
    pub range: ByteRange,
    /// Range of just the field name token (`bar` within
    /// `---@field bar integer`). When `None`, clients should fall
    /// back to `range`. `#[serde(default)]` keeps cached summaries
    /// produced by older builds readable.
    #[serde(default)]
    pub name_range: Option<ByteRange>,
}

/// Inferred type fact for a key local variable, with provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalTypeFact {
    pub name: String,
    pub type_fact: TypeFact,
    pub source: TypeFactSource,
    pub range: ByteRange,
}

/// Where a local's type information came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeFactSource {
    Assignment,
    CallReturn,
    FieldAccess,
    RequireBinding,
    EmmyAnnotation,
}

impl DocumentSummary {
    /// Look up a function summary by name using `function_name_index` (O(1)).
    /// Falls back to linear scan when the name isn't in the index (e.g. local
    /// functions looked up by call_hierarchy, or colon-qualified names).
    pub fn get_function_by_name(&self, name: &str) -> Option<&FunctionSummary> {
        // O(1) path: try the normalized (dot) form in the index first.
        let normalized = name.replace(':', ".");
        if let Some(id) = self.function_name_index.get(&normalized) {
            return self.function_summaries.get(id);
        }
        // Fallback: linear scan for non-indexed entries (local functions
        // during query time when called from call_hierarchy etc.).
        self.function_summaries
            .values()
            .find(|fs| fs.name == name)
    }

    /// Mutable version: look up a function summary by name and return both
    /// the ID and the mutable reference. Used by code that needs to modify
    /// the summary during resolution.
    pub fn get_function_by_name_mut(&mut self, name: &str) -> Option<(FunctionSummaryId, &mut FunctionSummary)> {
        self.function_summaries
            .iter_mut()
            .find(|(_, fs)| fs.name == name)
            .map(|(id, fs)| (*id, fs))
    }

    /// Iterate all (ID, FunctionSummary) pairs. Useful for fingerprinting,
    /// aggregation, and diagnostics that need to examine all functions.
    pub fn iter_functions(&self) -> impl Iterator<Item = (FunctionSummaryId, &FunctionSummary)> {
        self.function_summaries
            .iter()
            .map(|(id, fs)| (*id, fs))
    }
}
