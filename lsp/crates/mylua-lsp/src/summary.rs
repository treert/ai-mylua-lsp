use std::collections::HashMap;
use serde::Serialize;
use tower_lsp_server::ls_types::Uri;

use crate::lua_symbol::{get_lua_symbol, LuaSymbol};
use crate::table_shape::{TableShape, TableShapeId};
use crate::type_system::{FunctionSignature, FunctionSummaryId, TypeFact};
use crate::util::ByteRange;

/// Per-file summary: the "recipe" of type facts produced by single-file inference.
///
/// Contains everything needed to participate in the workspace aggregation layer
/// without re-parsing the AST. See `index-architecture.md` §2.1.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentSummary {
    pub uri: Uri,
    /// Globals defined/extended by this file.
    pub global_contributions: Vec<GlobalContribution>,
    /// Top-level and named function summaries, keyed by `FunctionSummaryId`.
    /// Note: Must maintain a reverse mapping (name → ID) in callers that need
    /// to look up functions by name. See `summary_builder/mod.rs::BuildContext::function_name_to_id`.
    pub function_summaries: HashMap<FunctionSummaryId, FunctionSummary>,
    /// Reverse index: function name → FunctionSummaryId.
    /// Only contains **global** functions. Colon-separated names are normalized
    /// to dot (e.g. `"Player:new"` → `"Player.new"`).
    /// Local functions are accessed via scope_tree → `FunctionRef(id)` instead.
    pub function_name_index: HashMap<LuaSymbol, FunctionSummaryId>,
    /// `---@class`, `---@alias`, `---@enum` definitions.
    pub type_definitions: Vec<TypeDefinition>,
    /// Table shape instances defined in this file.
    pub table_shapes: HashMap<TableShapeId, TableShape>,
    /// Type of the file-level `return` statement (module export).
    /// `None` if the file has no top-level return.
    pub module_return_type: Option<TypeFact>,
    /// Source range of the file-level `return` statement, used by `require`
    /// goto-definition to jump to the module's export. `None` if the file
    /// has no top-level return.
    pub module_return_range: Option<ByteRange>,
    /// Fingerprint of all externally-visible type signatures.
    /// Used for cascade invalidation: if unchanged, dependants don't need revalidation.
    pub signature_fingerprint: u64,
    /// Call sites captured from this file's function bodies (and
    /// top-level code). Feeds `call_hierarchy` incoming/outgoing
    /// queries without requiring the tree to be re-parsed at query
    /// time.
    pub call_sites: Vec<CallSite>,
    /// `true` when this file carries a top-level `---@meta` annotation.
    /// Meta files are treated as stub / definition sources per the
    /// Lua-LS convention: their globals still populate `global_shard`
    /// (so references elsewhere resolve), but diagnostics that reason
    /// about runtime behavior (like `undefinedGlobal`) are suppressed
    /// inside the meta file itself — meta files often reference
    /// runtime-provided symbols that don't have a declaration in the
    /// workspace.
    pub is_meta: bool,
    /// Optional module name supplied via `---@meta <name>`; purely
    /// informational at present (no require_map mapping yet).
    pub meta_name: Option<LuaSymbol>,
}

/// One `function_call` occurrence recorded during summary build.
/// `callee_name` preserves the full dotted / colon-qualified form
/// (`m.sub.foo`, `obj:bar`) so consumers can do exact or
/// last-segment matching.
#[derive(Debug, Clone, Serialize)]
pub struct CallSite {
    /// Full callee text (e.g. `foo`, `m.sub.foo`, `obj:bar`).
    pub callee_name: LuaSymbol,
    /// Enclosing function's (possibly qualified) name. Empty string
    /// when the call is at file top level.
    pub caller_name: LuaSymbol,
    /// `FunctionSummaryId` of the enclosing function. `None` when the
    /// call is at file top level. Prefer this over `caller_name` for
    /// looking up the caller's `FunctionSummary` — it avoids name-based
    /// ambiguity between local and global functions.
    pub caller_id: Option<FunctionSummaryId>,
    /// Range of the callee identifier / final segment — not the
    /// whole `function_call` node. Preferred by clients as the
    /// highlight target.
    pub range: ByteRange,
}

/// A global name contributed by this file (assignment, function declaration, table extension).
#[derive(Debug, Clone, Serialize)]
pub struct GlobalContribution {
    pub name: LuaSymbol,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: ByteRange,
    pub selection_range: ByteRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum GlobalContributionKind {
    Variable,
    Function,
    TableExtension,
}

/// Summary of a function's type-level contract.
#[derive(Debug, Clone, Serialize)]
pub struct FunctionSummary {
    pub name: LuaSymbol,
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
    pub generic_params: Vec<LuaSymbol>,
}

/// An Emmy type definition (`---@class`, `---@alias`, `---@enum`).
#[derive(Debug, Clone, Serialize)]
pub struct TypeDefinition {
    pub name: LuaSymbol,
    pub kind: TypeDefinitionKind,
    pub parents: Vec<LuaSymbol>,
    pub fields: Vec<TypeFieldDef>,
    pub alias_type: Option<TypeFact>,
    /// Names of generic type parameters (from `---@generic T, K`).
    pub generic_params: Vec<LuaSymbol>,
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
    /// Falls back to `range` when absent.
    pub name_range: Option<ByteRange>,
    /// When the `@class` anchors a local table (`local M = {}`), stores the
    /// shape ID so cross-file consumers can look up fields directly.
    pub anchor_shape_id: Option<crate::table_shape::TableShapeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum TypeDefinitionKind {
    Class,
    Alias,
    Enum,
}

/// A field declared within `---@class` or `---@field`.
#[derive(Debug, Clone, Serialize)]
pub struct TypeFieldDef {
    pub name: LuaSymbol,
    pub type_fact: TypeFact,
    /// Full `---@field ...` line range.
    pub range: ByteRange,
    /// Range of just the field name token (`bar` within
    /// `---@field bar integer`). When `None`, clients should fall
    /// back to `range`.
    pub name_range: Option<ByteRange>,
}

impl DocumentSummary {
    /// Look up a **global** function summary by name using
    /// `function_name_index` (O(1)). Colon-qualified names are
    /// normalized to dot form before lookup.
    ///
    /// This intentionally does NOT search local functions — those
    /// should be accessed via `scope_tree → FunctionRef(id)` →
    /// `function_summaries[id]` instead.
    pub fn get_function_by_name(&self, name: &str) -> Option<&FunctionSummary> {
        let normalized = name.replace(':', ".");
        let symbol = get_lua_symbol(&normalized)?;
        self.function_name_index.get(&symbol)
            .and_then(|id| self.function_summaries.get(id))
    }

    /// Iterate all (ID, FunctionSummary) pairs. Useful for fingerprinting,
    /// aggregation, and diagnostics that need to examine all functions.
    pub fn iter_functions(&self) -> impl Iterator<Item = (FunctionSummaryId, &FunctionSummary)> {
        self.function_summaries
            .iter()
            .map(|(id, fs)| (*id, fs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua_symbol::{intern_lua_symbol, LuaSymbol};

    fn empty_range() -> ByteRange {
        ByteRange {
            start_byte: 0,
            end_byte: 0,
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 0,
        }
    }

    fn assert_symbol(_: LuaSymbol) {}

    #[test]
    fn summary_lua_symbols_serialize_as_strings_and_lookup_normalizes_colon() {
        let function_id = FunctionSummaryId(0);
        let function_summary = FunctionSummary {
            name: intern_lua_symbol("Player.new"),
            signature: FunctionSignature {
                params: Vec::new(),
                returns: Vec::new(),
            },
            range: empty_range(),
            signature_fingerprint: 0,
            emmy_annotated: false,
            overloads: Vec::new(),
            generic_params: vec![intern_lua_symbol("T")],
        };
        assert_symbol(function_summary.name);
        assert_symbol(function_summary.generic_params[0]);

        let mut function_summaries = HashMap::new();
        function_summaries.insert(function_id, function_summary);
        let mut function_name_index = HashMap::new();
        function_name_index.insert(intern_lua_symbol("Player.new"), function_id);

        let summary = DocumentSummary {
            uri: "file:///summary.lua".parse().unwrap(),
            global_contributions: vec![GlobalContribution {
                name: intern_lua_symbol("Player.new"),
                kind: GlobalContributionKind::Function,
                type_fact: TypeFact::Known(crate::type_system::KnownType::FunctionRef(function_id)),
                range: empty_range(),
                selection_range: empty_range(),
            }],
            function_summaries,
            function_name_index,
            type_definitions: vec![TypeDefinition {
                name: intern_lua_symbol("Player"),
                kind: TypeDefinitionKind::Class,
                parents: vec![intern_lua_symbol("Entity")],
                fields: vec![TypeFieldDef {
                    name: intern_lua_symbol("hp"),
                    type_fact: TypeFact::Unknown,
                    range: empty_range(),
                    name_range: None,
                }],
                alias_type: None,
                generic_params: vec![intern_lua_symbol("T")],
                range: empty_range(),
                name_range: None,
                anchor_shape_id: None,
            }],
            table_shapes: HashMap::new(),
            module_return_type: None,
            module_return_range: None,
            signature_fingerprint: 0,
            call_sites: vec![CallSite {
                callee_name: intern_lua_symbol("Player.new"),
                caller_name: intern_lua_symbol(""),
                caller_id: None,
                range: empty_range(),
            }],
            is_meta: true,
            meta_name: Some(intern_lua_symbol("mymeta")),
        };

        assert!(summary.get_function_by_name("Player:new").is_some());

        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["global_contributions"][0]["name"], "Player.new");
        assert_eq!(json["type_definitions"][0]["name"], "Player");
        assert_eq!(json["type_definitions"][0]["parents"][0], "Entity");
        assert_eq!(json["type_definitions"][0]["fields"][0]["name"], "hp");
        assert_eq!(json["function_summaries"]["0"]["name"], "Player.new");
        assert_eq!(json["function_summaries"]["0"]["generic_params"][0], "T");
        assert_eq!(json["function_name_index"]["Player.new"], 0);
        assert_eq!(json["call_sites"][0]["callee_name"], "Player.new");
        assert_eq!(json["call_sites"][0]["caller_name"], "");
        assert_eq!(json["meta_name"], "mymeta");
    }
}
