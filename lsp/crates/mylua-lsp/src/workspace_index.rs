use std::collections::HashMap;
use tower_lsp_server::ls_types::Uri;
use crate::types::{DefKind, GlobalEntry};
use crate::util::{ts_node_to_range, node_text};

pub struct WorkspaceIndex {
    pub globals: HashMap<String, Vec<GlobalEntry>>,
    pub require_map: HashMap<String, Uri>,
}

impl WorkspaceIndex {
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            require_map: HashMap::new(),
        }
    }

    pub fn update_document(
        &mut self,
        uri: &Uri,
        tree: &tree_sitter::Tree,
        source: &[u8],
    ) {
        self.remove_document(uri);
        self.scan_globals(uri, tree.root_node(), source);
    }

    pub fn remove_document(&mut self, uri: &Uri) {
        self.globals.retain(|_, entries| {
            entries.retain(|e| &e.uri != uri);
            !entries.is_empty()
        });
    }

    fn scan_globals(
        &mut self,
        uri: &Uri,
        root: tree_sitter::Node,
        source: &[u8],
    ) {
        let mut cursor = root.walk();
        if !cursor.goto_first_child() {
            return;
        }

        loop {
            let node = cursor.node();
            match node.kind() {
                "function_declaration" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let name = node_text(name_node, source).to_string();
                        self.globals.entry(name.clone()).or_default().push(GlobalEntry {
                            name,
                            kind: DefKind::GlobalFunction,
                            range: ts_node_to_range(node),
                            selection_range: ts_node_to_range(name_node),
                            uri: uri.clone(),
                        });
                    }
                }
                "assignment_statement" => {
                    if let Some(left_node) = node.child_by_field_name("left") {
                        if let Some(first_var) = left_node.named_child(0) {
                            let name = node_text(first_var, source).to_string();
                            self.globals.entry(name.clone()).or_default().push(GlobalEntry {
                                name,
                                kind: DefKind::GlobalVariable,
                                range: ts_node_to_range(node),
                                selection_range: ts_node_to_range(first_var),
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
    }

    pub fn set_require_mapping(&mut self, module_path: String, uri: Uri) {
        self.require_map.insert(module_path, uri);
    }
}
