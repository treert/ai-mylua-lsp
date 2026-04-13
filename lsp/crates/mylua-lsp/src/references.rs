use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::scope;
use crate::types::DefKind;
use crate::util::{node_text, ts_node_to_range, position_to_byte_offset, find_node_at_position};
use crate::workspace_index::WorkspaceIndex;

pub fn find_references(
    doc: &Document,
    uri: &Uri,
    position: Position,
    include_declaration: bool,
    index: &WorkspaceIndex,
    all_docs: &HashMap<Uri, Document>,
) -> Option<Vec<Location>> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, doc.text.as_bytes());

    if let Some(def) = scope::resolve_at_position(&doc.tree, &doc.text, position, uri) {
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

    let scope_node = find_scope_for_definition(&doc.tree, def);

    if include_declaration {
        locations.push(Location {
            uri: uri.clone(),
            range: def.selection_range.clone(),
        });
    }

    if let Some(scope) = scope_node {
        collect_identifier_occurrences(scope, name, source, uri, &mut locations, def);
    }

    locations
}

fn find_scope_for_definition<'a>(
    tree: &'a tree_sitter::Tree,
    def: &crate::types::Definition,
) -> Option<tree_sitter::Node<'a>> {
    let start_byte = def.range.start.line as usize * 1000 + def.range.start.character as usize;
    let _ = start_byte;

    match def.kind {
        DefKind::Parameter | DefKind::ForVariable => {
            let def_pos = tree_sitter::Point {
                row: def.range.start.line as usize,
                column: def.range.start.character as usize,
            };
            let node = tree.root_node().descendant_for_point_range(def_pos, def_pos)?;
            let mut current = node;
            loop {
                match current.kind() {
                    "function_body" | "for_numeric_statement" | "for_generic_statement" => {
                        return Some(current);
                    }
                    _ => {
                        current = current.parent()?;
                    }
                }
            }
        }
        DefKind::LocalVariable | DefKind::LocalFunction => {
            let def_pos = tree_sitter::Point {
                row: def.range.start.line as usize,
                column: def.range.start.character as usize,
            };
            let node = tree.root_node().descendant_for_point_range(def_pos, def_pos)?;
            let mut current = node;
            loop {
                match current.kind() {
                    "source_file" | "function_body" | "do_statement" | "while_statement"
                    | "repeat_statement" | "if_statement" | "elseif_clause" | "else_clause" => {
                        return Some(current);
                    }
                    _ => {
                        if let Some(parent) = current.parent() {
                            current = parent;
                        } else {
                            return Some(tree.root_node());
                        }
                    }
                }
            }
        }
        _ => Some(tree.root_node()),
    }
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
    index: &WorkspaceIndex,
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
