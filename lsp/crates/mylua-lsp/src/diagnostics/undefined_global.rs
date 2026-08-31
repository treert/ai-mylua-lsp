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
    uri_id: crate::uri_id::UriId,
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
        // A free name is `_ENV.name`. When the environment answers the name —
        // either because its field set is exhaustive, or because it records
        // this particular field — "undefined global" no longer applies and
        // whether the field exists is `diagnostics::env_field`'s business.
        //
        // Keyed off the same `name_resolution` predicate the navigation side
        // uses, so the two cannot disagree. Note what this *does not* suppress:
        // a name missing from a sandbox that carries a metatable. Under the
        // documented convention such an environment is assumed to be
        // `{ __index = _G }` (§1.3), so the name is looked up in the global
        // index and reported when absent there too — silence would hide a real
        // `nil` at run time.
        let answered_by_env = crate::name_resolution::is_known_env_field(
            name,
            byte_offset,
            uri_id,
            scope_tree,
            index,
        );
        if !is_local
            && !answered_by_env
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
                uri_id,
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
