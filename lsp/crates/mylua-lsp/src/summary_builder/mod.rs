mod call_sites;
mod emmy_visitors;
pub(crate) mod fingerprint;
pub(crate) mod table_extract;
pub(crate) mod type_infer;
pub(crate) mod visitors;

use std::collections::HashMap;

use tower_lsp_server::ls_types::Uri;

use crate::emmy::{parse_emmy_comments, EmmyAnnotation, EmmyType};
use crate::summary::*;
use crate::table_shape::{TableShape, TableShapeId};
use crate::type_system::*;
use crate::util::{node_text, LineIndex};
use crate::scope::{Scope, ScopeKind, ScopeDecl, ScopeTree};

use call_sites::collect_call_sites;
use fingerprint::{hash_bytes, compute_signature_fingerprint};
use visitors::visit_top_level;

// Re-export the public API so external callers don't need to change.
pub(crate) use visitors::enclosing_statement_for_function_expr;

/// Build a `DocumentSummary` and `ScopeTree` from a parsed AST.
///
/// This is the core of single-file inference (index-architecture.md §3).
/// Zero cross-file dependencies: all unresolved references become `SymbolicStub`s.
pub fn build_file_analysis(
    uri: &Uri,
    tree: &tree_sitter::Tree,
    source: &[u8],
    line_index: &LineIndex,
) -> (DocumentSummary, ScopeTree) {
    let mut ctx = BuildContext {
        source,
        line_index,
        require_bindings: Vec::new(),
        global_contributions: Vec::new(),
        function_summaries: HashMap::new(),
        function_name_to_id: HashMap::new(),
        function_name_index: HashMap::new(),
        type_definitions: Vec::new(),
        table_shapes: HashMap::new(),
        next_shape_id: 0,
        next_function_id: 0,
        pending_type_annotation: None,
        pending_class: None,
        pending_generic_params: Vec::new(),
        pending_class_name: None,
        global_class_bindings: HashMap::new(),
        module_return_type: None,
        module_return_range: None,
        scopes: Vec::new(),
        scope_stack: Vec::new(),
        last_emmy_end_row: None,
    };

    let root = tree.root_node();
    visit_top_level(&mut ctx, root);

    // Backfill anchor_shape_id on TypeDefinitions whose anchor is a local
    // variable with a Table shape. flush_pending_class runs before
    // visit_local_declaration, so the shape doesn't exist at class creation
    // time — we scan the scope-registered declarations after traversal.
    backfill_anchor_shape_ids(&mut ctx);

    let content_hash = hash_bytes(source);
    let signature_fingerprint = compute_signature_fingerprint(&ctx);
    let call_sites = collect_call_sites(root, source, &line_index);
    let (is_meta, meta_name) = detect_meta_annotation(root, source);

    let scope_tree = ctx.take_scope_tree();

    // Collect type names referenced by scope declarations. Pre-computed
    // here so aggregation::collect_referenced_type_names can use this field.
    let referenced_local_type_names = collect_scope_type_names(&scope_tree);

    let summary = DocumentSummary {
        uri: uri.clone(),
        content_hash,
        require_bindings: ctx.require_bindings,
        global_contributions: ctx.global_contributions,
        function_summaries: ctx.function_summaries,
        function_name_index: ctx.function_name_index,
        type_definitions: ctx.type_definitions,
        table_shapes: ctx.table_shapes,
        module_return_type: ctx.module_return_type,
        module_return_range: ctx.module_return_range,
        signature_fingerprint,
        call_sites,
        is_meta,
        meta_name,
        referenced_local_type_names,
    };

    (summary, scope_tree)
}

/// Walk all scope declarations and collect Emmy type names referenced by
/// their type_facts. Mirrors the `walk` helper in
/// `aggregation::collect_referenced_type_names` (item 1).
fn collect_scope_type_names(scope_tree: &ScopeTree) -> std::collections::HashSet<String> {
    use crate::type_system::{KnownType, SymbolicStub, TypeFact};
    let mut names = std::collections::HashSet::new();

    fn walk(fact: &TypeFact, out: &mut std::collections::HashSet<String>) {
        match fact {
            TypeFact::Known(KnownType::EmmyType(n)) => { out.insert(n.clone()); }
            TypeFact::Known(KnownType::EmmyGeneric(n, params)) => {
                out.insert(n.clone());
                for p in params { walk(p, out); }
            }
            TypeFact::Known(KnownType::Function(sig)) => {
                for p in &sig.params { walk(&p.type_fact, out); }
                for r in &sig.returns { walk(r, out); }
            }
            TypeFact::Stub(SymbolicStub::TypeRef { name }) => { out.insert(name.clone()); }
            TypeFact::Stub(SymbolicStub::FieldOf { base, .. }) => { walk(base, out); }
            TypeFact::Union(parts) => { for p in parts { walk(p, out); } }
            _ => {}
        }
    }

    for decl in scope_tree.all_declarations() {
        if let Some(tf) = &decl.type_fact {
            walk(tf, &mut names);
        }
    }
    names
}

/// Backfill `anchor_shape_id` on `TypeDefinition`s whose anchor is a local
/// variable with a `Table` shape. `flush_pending_class` runs before
/// `visit_local_declaration`, so the shape doesn't exist at class creation
/// time — we scan the scope-registered declarations after traversal.
fn backfill_anchor_shape_ids(ctx: &mut BuildContext) {
    // Collect name → shape_id from scope declarations.
    let shape_map: HashMap<String, TableShapeId> = ctx.scopes.iter()
        .flat_map(|s| s.declarations.iter())
        .filter_map(|decl| {
            if let Some(TypeFact::Known(KnownType::Table(sid))) = &decl.type_fact {
                Some((decl.name.clone(), *sid))
            } else {
                None
            }
        })
        .collect();

    for td in &mut ctx.type_definitions {
        if td.kind != TypeDefinitionKind::Class || td.anchor_shape_id.is_some() {
            continue;
        }
        if let Some(&sid) = shape_map.get(&td.name) {
            td.anchor_shape_id = Some(sid);
        }
    }
}

/// Scan the first few top-level statements for a `---@meta [name]`
/// annotation. Following Lua-LS convention the directive lives at
/// the top of the file; we allow it to appear after a shebang or
/// initial comments but stop looking once any real statement
/// (`local_declaration` / `function_declaration` / `assignment_statement`
/// / `return_statement`) precedes it, since `---@meta` placed after
/// runtime code is almost certainly an authoring mistake.
fn detect_meta_annotation(root: tree_sitter::Node, source: &[u8]) -> (bool, Option<String>) {
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i as u32) else { continue };
        match child.kind() {
            "emmy_comment" => {
                for j in 0..child.named_child_count() {
                    let Some(line) = child.named_child(j as u32) else { continue };
                    if line.kind() != "emmy_line" {
                        continue;
                    }
                    let text = node_text(line, source);
                    let anns = parse_emmy_comments(text);
                    for ann in anns {
                        if let EmmyAnnotation::Meta { name } = ann {
                            return (true, name);
                        }
                    }
                }
            }
            // Any non-emmy sibling that represents real code tells us
            // there's no leading `---@meta`.
            "local_declaration"
            | "local_function_declaration"
            | "function_declaration"
            | "assignment_statement"
            | "return_statement" => return (false, None),
            _ => continue,
        }
    }
    (false, None)
}

/// Intermediate state for a class being built across consecutive
/// emmy_comment nodes: (name, parents, fields, generic_params,
/// name_range of the `---@class <Name>` identifier token).
type PendingClass = (
    String,
    Vec<String>,
    Vec<TypeFieldDef>,
    Vec<String>,
    crate::util::ByteRange,
);

pub(crate) struct BuildContext<'a> {
    pub(crate) source: &'a [u8],
    pub(crate) line_index: &'a LineIndex,
    pub(crate) require_bindings: Vec<RequireBinding>,
    pub(crate) global_contributions: Vec<GlobalContribution>,
    pub(crate) function_summaries: HashMap<FunctionSummaryId, FunctionSummary>,
    /// Reverse mapping: function name → FunctionSummaryId.
    /// Built during `visit_top_level` and used by expression type inference.
    pub(crate) function_name_to_id: HashMap<String, FunctionSummaryId>,
    /// Exported reverse index: global function name (colon→dot normalized) → FunctionSummaryId.
    /// Populated by `visit_function_declaration` for global functions only.
    /// Transferred to `DocumentSummary::function_name_index` at build completion.
    pub(crate) function_name_index: HashMap<String, FunctionSummaryId>,
    pub(crate) type_definitions: Vec<TypeDefinition>,
    pub(crate) table_shapes: HashMap<TableShapeId, TableShape>,
    pub(crate) next_shape_id: u32,
    /// Counter for allocating unique `FunctionSummaryId`s per file.
    pub(crate) next_function_id: u32,
    /// `---@type X` annotation pending attachment to the next local declaration.
    pub(crate) pending_type_annotation: Option<EmmyType>,
    /// Class being built across consecutive emmy_comment nodes.
    pub(crate) pending_class: Option<PendingClass>,
    /// Buffer for `@generic` params that arrive before `@class`.
    pub(crate) pending_generic_params: Vec<String>,
    /// Class name from the most recently flushed `@class`, consumed by
    /// the immediately following `local` or assignment to bind the variable
    /// to its class definition (Phase 2: class anchor binding).
    pub(crate) pending_class_name: Option<String>,
    /// Global variable → class name binding. When `---@class Foo` is followed
    /// by `Foo = class()` (global assignment), record the binding here so
    /// `function Foo:method()` can resolve Foo's class and write fields back.
    pub(crate) global_class_bindings: HashMap<String, String>,
    /// Type of the file-level `return` statement (module export).
    pub(crate) module_return_type: Option<TypeFact>,
    /// Source range of the file-level `return` statement.
    pub(crate) module_return_range: Option<crate::util::ByteRange>,
    /// Scope stack for building the ScopeTree alongside the summary.
    pub(crate) scopes: Vec<Scope>,
    /// Stack of scope indices — top is the current innermost scope.
    pub(crate) scope_stack: Vec<usize>,
    /// End row of the last emmy_comment node visited in `visit_top_level`.
    /// Used to detect blank-line gaps between annotations and the following
    /// statement — when a gap exists, pending class/type bindings are cleared.
    pub(crate) last_emmy_end_row: Option<u32>,
}

impl<'a> BuildContext<'a> {
    pub(crate) fn alloc_shape_id(&mut self) -> TableShapeId {
        let id = TableShapeId(self.next_shape_id);
        self.next_shape_id += 1;
        id
    }

    pub(crate) fn alloc_function_id(&mut self) -> FunctionSummaryId {
        let id = FunctionSummaryId(self.next_function_id);
        self.next_function_id += 1;
        id
    }

    pub(crate) fn take_pending_type(&mut self) -> Option<EmmyType> {
        self.pending_type_annotation.take()
    }

    /// Push a new scope onto the stack. Returns the scope id.
    pub(crate) fn push_scope(&mut self, kind: ScopeKind, start: usize, end: usize) -> usize {
        let id = self.scopes.len();
        let parent = self.scope_stack.last().copied();
        self.scopes.push(Scope {
            kind,
            byte_start: start,
            byte_end: end,
            parent,
            children: Vec::new(),
            declarations: Vec::new(),
        });
        if let Some(pid) = parent {
            self.scopes[pid].children.push(id);
        }
        self.scope_stack.push(id);
        id
    }

    /// Pop the current scope from the stack.
    pub(crate) fn pop_scope(&mut self) {
        self.scope_stack.pop();
    }

    /// Add a declaration to the current scope.
    pub(crate) fn add_scoped_decl(&mut self, decl: ScopeDecl) {
        if let Some(&scope_id) = self.scope_stack.last() {
            self.scopes[scope_id].declarations.push(decl);
        }
    }

    /// Resolve a name by walking the scope stack from innermost to outermost.
    /// This is the build-time equivalent of `ScopeTree::resolve_decl`.
    pub(crate) fn resolve_in_build_scopes(&self, name: &str) -> Option<&ScopeDecl> {
        for &scope_id in self.scope_stack.iter().rev() {
            let scope = &self.scopes[scope_id];
            let mut best: Option<&ScopeDecl> = None;
            for decl in &scope.declarations {
                if decl.name == name {
                    best = Some(decl);
                }
            }
            if best.is_some() {
                return best;
            }
        }
        None
    }

    /// Resolve the `bound_class` for a variable name. Checks locals first
    /// (via scope stack), then falls back to `global_class_bindings`.
    /// Implements the strictly-layered lookup from Phase 2 §4.2.
    pub(crate) fn resolve_bound_class_for(&self, name: &str) -> Option<&str> {
        if let Some(decl) = self.resolve_in_build_scopes(name) {
            return decl.bound_class.as_deref();
        }
        self.global_class_bindings.get(name).map(|s| s.as_str())
    }

    /// Extract the built scopes into a ScopeTree, consuming the scope data.
    pub(crate) fn take_scope_tree(&mut self) -> ScopeTree {
        ScopeTree::from_scopes(std::mem::take(&mut self.scopes))
    }
}
