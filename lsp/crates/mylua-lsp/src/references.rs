use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::util::{node_text, ts_node_to_range, position_to_byte_offset, find_node_at_position};
use crate::aggregation::WorkspaceAggregation;

pub fn find_references(
    doc: &Document,
    uri: &Uri,
    position: Position,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
) -> Option<Vec<Location>> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, doc.text.as_bytes());

    if let Some(def) = doc.scope_tree.resolve(byte_offset, name, uri) {
        return Some(find_local_references(
            doc,
            uri,
            name,
            &def,
            include_declaration,
        ));
    }

    Some(find_global_references(
        name,
        include_declaration,
        index,
        all_docs,
    ))
}

fn find_local_references(
    doc: &Document,
    uri: &Uri,
    name: &str,
    def: &crate::types::Definition,
    include_declaration: bool,
) -> Vec<Location> {
    let mut locations = Vec::new();
    let source = doc.text.as_bytes();

    if include_declaration {
        locations.push(Location {
            uri: uri.clone(),
            range: def.selection_range.clone(),
        });
    }

    let def_byte = position_to_byte_offset(&doc.text, def.selection_range.start)
        .unwrap_or(0);
    let scope_range = doc.scope_tree.scope_byte_range_for_def(def_byte, name);
    let scope_node = if let Some((start, end)) = scope_range {
        doc.tree.root_node().descendant_for_byte_range(start, end.saturating_sub(1))
    } else {
        Some(doc.tree.root_node())
    };

    if let Some(scope) = scope_node {
        collect_identifier_occurrences(scope, name, source, uri, &mut locations, def);
    }

    locations
}

fn collect_identifier_occurrences(
    scope: tree_sitter::Node,
    name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
    def: &crate::types::Definition,
) {
    let mut cursor = scope.walk();
    collect_idents_recursive(&mut cursor, name, source, uri, locations, def);
}

fn collect_idents_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
    def: &crate::types::Definition,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && node_text(node, source) == name {
        let range = ts_node_to_range(node);
        if range != def.selection_range {
            let is_after_def = range.start.line > def.selection_range.start.line
                || (range.start.line == def.selection_range.start.line
                    && range.start.character >= def.selection_range.end.character);
            if is_after_def {
                locations.push(Location {
                    uri: uri.clone(),
                    range,
                });
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_idents_recursive(cursor, name, source, uri, locations, def);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn find_global_references(
    name: &str,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
) -> Vec<Location> {
    let mut locations = Vec::new();

    if include_declaration {
        if let Some(entries) = index.globals.get(name) {
            for entry in entries {
                locations.push(Location {
                    uri: entry.uri.clone(),
                    range: entry.selection_range.clone(),
                });
            }
        }
    }

    for (doc_uri, doc) in all_docs {
        let source = doc.text.as_bytes();
        let mut cursor = doc.tree.root_node().walk();
        collect_global_name_occurrences(&mut cursor, name, source, doc_uri, &mut locations);
    }

    locations
}

fn collect_global_name_occurrences(
    cursor: &mut tree_sitter::TreeCursor,
    name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && node_text(node, source) == name {
        if let Some(parent) = node.parent() {
            if parent.kind() == "variable" {
                let is_bare_name = parent.named_child_count() == 0
                    || (parent.child_count() == 1);
                if is_bare_name {
                    locations.push(Location {
                        uri: uri.clone(),
                        range: ts_node_to_range(node),
                    });
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_global_name_occurrences(cursor, name, source, uri, locations);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
