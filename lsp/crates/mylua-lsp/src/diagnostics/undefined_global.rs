use crate::aggregation::WorkspaceAggregation;
use crate::scope::ScopeTree;
use crate::syntax_kind::{kind, NodeKindExt};
use crate::util::{node_text, LineIndex};
use std::collections::HashSet;
use tower_lsp_server::ls_types::*;

pub(super) fn check_undefined_globals(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    builtins: &HashSet<&str>,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let node = cursor.node();

    if node.is_kind(kind::IDENTIFIER) && super::free_name::is_free_name_reference(node) {
        let name = node_text(node, source);
        let byte_offset = node.start_byte();
        let is_local = scope_tree.resolve_decl(byte_offset, name).is_some();
        // A free name is `_ENV.name`. Once a user-declared `_ENV` is
        // in scope the name is a field of that table, not a global,
        // so "undefined global" no longer applies — whether the field
        // exists is `diagnostics::env_field`'s business.
        let env_redirected =
            crate::type_inference::env_field_base_fact_in_scope(name, byte_offset, scope_tree)
                .is_some();
        if !is_local
            && !env_redirected
            && !builtins.contains(name)
            && !index.global_shard.contains_key(name)
        {
            diagnostics.push(Diagnostic {
                range: line_index.ts_node_to_range(node, source),
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!("Undefined global '{}'", name),
                ..Default::default()
            });
        }
    }

    if cursor.goto_first_child() {
        loop {
            check_undefined_globals(
                cursor,
                source,
                builtins,
                index,
                scope_tree,
                diagnostics,
                severity,
                line_index,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
