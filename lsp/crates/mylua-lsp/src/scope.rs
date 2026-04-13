use tower_lsp_server::ls_types::{Position, Uri};
use crate::types::{DefKind, Definition};
use crate::util::{node_text, ts_node_to_range, position_to_byte_offset, find_node_at_position};

pub fn resolve_at_position(
    tree: &tree_sitter::Tree,
    source: &str,
    position: Position,
    uri: &Uri,
) -> Option<Definition> {
    let byte_offset = position_to_byte_offset(source, position)?;
    let ident_node = find_node_at_position(tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, source.as_bytes());

    let mut node = ident_node;
    loop {
        if let Some(parent) = node.parent() {
            if let Some(def) = search_scope(parent, name, ident_node, source.as_bytes(), uri) {
                return Some(def);
            }
            node = parent;
        } else {
            break;
        }
    }

    None
}

fn search_scope(
    scope_node: tree_sitter::Node,
    name: &str,
    reference_node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
) -> Option<Definition> {
    match scope_node.kind() {
        "source_file" | "do_statement" | "while_statement" | "repeat_statement"
        | "if_statement" | "elseif_clause" | "else_clause" => {
            search_block_for_local(scope_node, name, reference_node, source, uri)
        }
        "for_numeric_statement" => {
            if let Some(name_node) = scope_node.child_by_field_name("name") {
                if node_text(name_node, source) == name
                    && name_node.end_byte() <= reference_node.start_byte()
                {
                    return Some(Definition {
                        name: name.to_string(),
                        kind: DefKind::ForVariable,
                        range: ts_node_to_range(scope_node),
                        selection_range: ts_node_to_range(name_node),
                        uri: uri.clone(),
                    });
                }
            }
            None
        }
        "for_generic_statement" => {
            if let Some(names_node) = scope_node.child_by_field_name("names") {
                for i in 0..names_node.named_child_count() {
                    if let Some(id_node) = names_node.named_child(i as u32) {
                        if id_node.kind() == "identifier"
                            && node_text(id_node, source) == name
                            && id_node.end_byte() <= reference_node.start_byte()
                        {
                            return Some(Definition {
                                name: name.to_string(),
                                kind: DefKind::ForVariable,
                                range: ts_node_to_range(scope_node),
                                selection_range: ts_node_to_range(id_node),
                                uri: uri.clone(),
                            });
                        }
                    }
                }
            }
            None
        }
        "function_body" => {
            if let Some(params) = scope_node.child_by_field_name("parameters") {
                for i in 0..params.named_child_count() {
                    if let Some(child) = params.named_child(i as u32) {
                        if child.kind() == "identifier" && node_text(child, source) == name {
                            return Some(Definition {
                                name: name.to_string(),
                                kind: DefKind::Parameter,
                                range: ts_node_to_range(child),
                                selection_range: ts_node_to_range(child),
                                uri: uri.clone(),
                            });
                        }
                        if child.kind() == "name_list" {
                            for j in 0..child.named_child_count() {
                                if let Some(id) = child.named_child(j as u32) {
                                    if id.kind() == "identifier"
                                        && node_text(id, source) == name
                                    {
                                        return Some(Definition {
                                            name: name.to_string(),
                                            kind: DefKind::Parameter,
                                            range: ts_node_to_range(id),
                                            selection_range: ts_node_to_range(id),
                                            uri: uri.clone(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn search_block_for_local(
    block_node: tree_sitter::Node,
    name: &str,
    reference_node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
) -> Option<Definition> {
    let mut cursor = block_node.walk();
    if !cursor.goto_first_child() {
        return None;
    }

    let mut last_match: Option<Definition> = None;

    loop {
        let child = cursor.node();
        if child.start_byte() >= reference_node.start_byte() {
            break;
        }

        match child.kind() {
            "local_declaration" => {
                if let Some(names_node) = child.child_by_field_name("names") {
                    for i in 0..names_node.named_child_count() {
                        if let Some(id_node) = names_node.named_child(i as u32) {
                            if id_node.kind() == "identifier"
                                && node_text(id_node, source) == name
                            {
                                last_match = Some(Definition {
                                    name: name.to_string(),
                                    kind: DefKind::LocalVariable,
                                    range: ts_node_to_range(child),
                                    selection_range: ts_node_to_range(id_node),
                                    uri: uri.clone(),
                                });
                            }
                        }
                    }
                }
            }
            "local_function_declaration" => {
                if let Some(name_node) = child.child_by_field_name("name") {
                    if node_text(name_node, source) == name {
                        last_match = Some(Definition {
                            name: name.to_string(),
                            kind: DefKind::LocalFunction,
                            range: ts_node_to_range(child),
                            selection_range: ts_node_to_range(name_node),
                            uri: uri.clone(),
                        });
                    }
                }
            }
            _ => {}
        }

        if !cursor.goto_next_sibling() {
            break;
        }
    }

    last_match
}
