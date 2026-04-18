use tower_lsp_server::ls_types::*;
use crate::util::{ts_node_to_range, node_text};

pub fn collect_document_symbols(root: tree_sitter::Node, source: &[u8]) -> Vec<DocumentSymbol> {
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    if !cursor.goto_first_child() {
        return symbols;
    }

    loop {
        let node = cursor.node();
        match node.kind() {
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    #[allow(deprecated)]
                    symbols.push(DocumentSymbol {
                        name,
                        detail: None,
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        range: ts_node_to_range(node, source),
                        selection_range: ts_node_to_range(name_node, source),
                        children: None,
                    });
                }
            }
            "local_function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = node_text(name_node, source).to_string();
                    #[allow(deprecated)]
                    symbols.push(DocumentSymbol {
                        name,
                        detail: Some("local".to_string()),
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        range: ts_node_to_range(node, source),
                        selection_range: ts_node_to_range(name_node, source),
                        children: None,
                    });
                }
            }
            "local_declaration" => {
                if let Some(names_node) = node.child_by_field_name("names") {
                    for i in 0..names_node.named_child_count() {
                        if let Some(id_node) = names_node.named_child(i as u32) {
                            if id_node.kind() == "identifier" {
                                let name = node_text(id_node, source).to_string();
                                #[allow(deprecated)]
                                symbols.push(DocumentSymbol {
                                    name,
                                    detail: Some("local".to_string()),
                                    kind: SymbolKind::VARIABLE,
                                    tags: None,
                                    deprecated: None,
                                    range: ts_node_to_range(node, source),
                                    selection_range: ts_node_to_range(id_node, source),
                                    children: None,
                                });
                            }
                        }
                    }
                }
            }
            "assignment_statement" => {
                if let Some(left_node) = node.child_by_field_name("left") {
                    if let Some(first_var) = left_node.named_child(0) {
                        let name = node_text(first_var, source).to_string();
                        #[allow(deprecated)]
                        symbols.push(DocumentSymbol {
                            name,
                            detail: None,
                            kind: SymbolKind::VARIABLE,
                            tags: None,
                            deprecated: None,
                            range: ts_node_to_range(node, source),
                            selection_range: ts_node_to_range(first_var, source),
                            children: None,
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

    symbols
}
