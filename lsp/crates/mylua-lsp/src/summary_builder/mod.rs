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

use call_sites::collect_call_sites;
use fingerprint::{hash_bytes, compute_signature_fingerprint};
use visitors::visit_top_level;

// Re-export the public API so external callers don't need to change.
pub(crate) use visitors::enclosing_statement_for_function_expr;

/// Build a `DocumentSummary` from a parsed AST.
///
/// This is the core of single-file inference (index-architecture.md §3).
/// Zero cross-file dependencies: all unresolved references become `SymbolicStub`s.
pub fn build_summary(uri: &Uri, tree: &tree_sitter::Tree, source: &[u8], line_index: &LineIndex) -> DocumentSummary {
    let mut ctx = BuildContext {
        source,
        line_index,
        require_bindings: Vec::new(),
        global_contributions: Vec::new(),
        function_summaries: HashMap::new(),
        type_definitions: Vec::new(),
        local_type_facts: HashMap::new(),
        table_shapes: HashMap::new(),
        next_shape_id: 0,
        pending_type_annotation: None,
        pending_class: None,
        pending_generic_params: Vec::new(),
        module_return_type: None,
        module_return_range: None,
    };

    let root = tree.root_node();
    visit_top_level(&mut ctx, root);

    let content_hash = hash_bytes(source);
    let signature_fingerprint = compute_signature_fingerprint(&ctx);
    let call_sites = collect_call_sites(root, source, &line_index);
    let (is_meta, meta_name) = detect_meta_annotation(root, source);

    DocumentSummary {
        uri: uri.clone(),
        content_hash,
        require_bindings: ctx.require_bindings,
        global_contributions: ctx.global_contributions,
        function_summaries: ctx.function_summaries,
        type_definitions: ctx.type_definitions,
        local_type_facts: ctx.local_type_facts,
        table_shapes: ctx.table_shapes,
        module_return_type: ctx.module_return_type,
        module_return_range: ctx.module_return_range,
        signature_fingerprint,
        call_sites,
        is_meta,
        meta_name,
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
    pub(crate) function_summaries: HashMap<String, FunctionSummary>,
    pub(crate) type_definitions: Vec<TypeDefinition>,
    pub(crate) local_type_facts: HashMap<String, LocalTypeFact>,
    pub(crate) table_shapes: HashMap<TableShapeId, TableShape>,
    pub(crate) next_shape_id: u32,
    /// `---@type X` annotation pending attachment to the next local declaration.
    pub(crate) pending_type_annotation: Option<EmmyType>,
    /// Class being built across consecutive emmy_comment nodes.
    pub(crate) pending_class: Option<PendingClass>,
    /// Buffer for `@generic` params that arrive before `@class`.
    pub(crate) pending_generic_params: Vec<String>,
    /// Type of the file-level `return` statement (module export).
    pub(crate) module_return_type: Option<TypeFact>,
    /// Source range of the file-level `return` statement.
    pub(crate) module_return_range: Option<crate::util::ByteRange>,
}

impl<'a> BuildContext<'a> {
    pub(crate) fn alloc_shape_id(&mut self) -> TableShapeId {
        let id = TableShapeId(self.next_shape_id);
        self.next_shape_id += 1;
        id
    }

    pub(crate) fn take_pending_type(&mut self) -> Option<EmmyType> {
        self.pending_type_annotation.take()
    }
}
