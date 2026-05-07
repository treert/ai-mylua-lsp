use std::collections::HashMap;
use std::mem;

use crate::aggregation::WorkspaceAggregation;
use crate::document::Document;
use crate::lua_symbol;
use crate::uri_id::{path_uri, UriId};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DocumentMemoryStats {
    pub document_count: usize,
    pub source_bytes: usize,
    pub line_start_count: usize,
    pub line_index_bytes: usize,
    pub tree_node_count: usize,
    pub scope_count: usize,
    pub scope_declaration_count: usize,
    pub scope_child_link_count: usize,
}

impl DocumentMemoryStats {
    pub fn from_documents(documents: &HashMap<UriId, Document>) -> Self {
        let mut stats = DocumentMemoryStats {
            document_count: documents.len(),
            ..Default::default()
        };

        for doc in documents.values() {
            stats.source_bytes += doc.source().len();
            stats.line_start_count += doc.lua_source.line_start_count();
            if let Some(root) = doc.root_node() {
                stats.tree_node_count += root.descendant_count();
            }

            let scope_stats = doc.scope_tree.stats();
            stats.scope_count += scope_stats.scope_count;
            stats.scope_declaration_count += scope_stats.declaration_count;
            stats.scope_child_link_count += scope_stats.child_link_count;
        }
        stats.line_index_bytes = stats.line_start_count * mem::size_of::<usize>();

        stats
    }
}

pub fn enabled() -> bool {
    std::env::var("MYLUA_MEM_PROFILE")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn log_document_profile(documents: &HashMap<UriId, Document>) {
    let doc_stats = DocumentMemoryStats::from_documents(documents);
    lsp_log!(
        "[mem] documents: count={} source_bytes={} line_starts={} line_index_bytes={} tree_nodes={} scopes={} scope_decls={} scope_child_links={}",
        doc_stats.document_count,
        doc_stats.source_bytes,
        doc_stats.line_start_count,
        doc_stats.line_index_bytes,
        doc_stats.tree_node_count,
        doc_stats.scope_count,
        doc_stats.scope_declaration_count,
        doc_stats.scope_child_link_count,
    );
    log_top_documents(documents);
}

pub fn log_index_profile(index: &WorkspaceAggregation) {
    let agg_stats = index.stats();
    lsp_log!(
        "[mem] summaries: count={} globals={} functions={} function_name_index={} type_defs={} type_fields={} table_shapes={} table_fields={} call_sites={}",
        agg_stats.summary_count,
        agg_stats.global_contribution_count,
        agg_stats.function_summary_count,
        agg_stats.function_name_index_count,
        agg_stats.type_definition_count,
        agg_stats.type_field_count,
        agg_stats.table_shape_count,
        agg_stats.table_field_count,
        agg_stats.call_site_count,
    );
    lsp_log!(
        "[mem] aggregation: global_roots={} global_nodes={} global_candidates={} global_reverse_paths={} type_names={} type_candidates={} module_last_segments={} module_entries={} require_aliases={}",
        agg_stats.global_root_count,
        agg_stats.global_node_count,
        agg_stats.global_candidate_count,
        agg_stats.global_reverse_path_count,
        agg_stats.type_name_count,
        agg_stats.type_candidate_count,
        agg_stats.module_last_segment_count,
        agg_stats.module_entry_count,
        agg_stats.require_alias_count,
    );
}

pub fn log_symbol_profile() {
    let symbol_stats = lua_symbol::lua_symbol_stats();
    lsp_log!(
        "[mem] lua_symbols: count={} string_bytes={} arena_bytes={}",
        symbol_stats.symbol_count,
        symbol_stats.string_bytes,
        symbol_stats.arena_bytes,
    );
}

fn log_top_documents(documents: &HashMap<UriId, Document>) {
    let mut rows: Vec<(usize, usize, usize, usize, usize, UriId)> = documents
        .iter()
        .map(|(uri_id, doc)| {
            let scope_stats = doc.scope_tree.stats();
            (
                doc.root_node().map(|root| root.descendant_count()).unwrap_or(0),
                doc.source().len(),
                scope_stats.declaration_count,
                scope_stats.scope_count,
                doc.lua_source.line_start_count(),
                *uri_id,
            )
        })
        .collect();
    rows.sort_by(|left, right| right.cmp(left));
    for (rank, (tree_nodes, source_bytes, scope_decls, scopes, line_starts, uri_id)) in
        rows.into_iter().take(10).enumerate()
    {
        lsp_log!(
            "[mem] top_tree_file rank={} tree_nodes={} source_bytes={} scope_decls={} scopes={} line_starts={} uri={}",
            rank + 1,
            tree_nodes,
            source_bytes,
            scope_decls,
            scopes,
            line_starts,
            path_uri(uri_id),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_parser;
    use crate::summary_builder;
    use crate::util::LuaSource;
    use tower_lsp_server::ls_types::Uri;

    #[test]
    fn document_stats_count_source_lines_tree_and_scopes() {
        let src = "local x = 1\nprint(x)\n";
        let mut parser = new_parser();
        let tree = parser.parse(src.as_bytes(), None).unwrap();
        let lua_source = LuaSource::new(src.to_string());
        let uri: Uri = "file:///memory_profile.lua".parse().unwrap();
        let (_, scope_tree) = summary_builder::build_file_analysis(
            &uri,
            &tree,
            lua_source.source(),
            lua_source.line_index(),
        );
        let uri_id = crate::uri_id::intern_uri(&uri);

        let mut documents = HashMap::new();
        documents.insert(
            uri_id,
            Document {
                lua_source,
                tree: Some(tree),
                scope_tree,
                last_diagnostic_signature: None,
            },
        );

        let stats = DocumentMemoryStats::from_documents(&documents);
        assert_eq!(stats.document_count, 1);
        assert_eq!(stats.source_bytes, src.len());
        assert!(stats.line_start_count >= 2);
        assert!(stats.line_index_bytes >= stats.line_start_count * mem::size_of::<usize>());
        assert!(stats.tree_node_count > 0);
        assert!(stats.scope_count > 0);
    }
}
