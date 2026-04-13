use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::scope;
use crate::workspace_index::WorkspaceIndex;
use crate::util::{node_text, position_to_byte_offset, find_node_at_position};

pub fn goto_definition(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceIndex,
) -> Option<GotoDefinitionResponse> {
    if let Some(def) = scope::resolve_at_position(&doc.tree, &doc.text, position, uri) {
        return Some(GotoDefinitionResponse::Scalar(Location {
            uri: def.uri,
            range: def.selection_range,
        }));
    }

    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let name = node_text(ident_node, doc.text.as_bytes());

    if let Some(target) = try_require_goto(doc, ident_node, index) {
        return Some(target);
    }

    if let Some(entries) = index.globals.get(name) {
        let locations: Vec<Location> = entries
            .iter()
            .map(|e| Location {
                uri: e.uri.clone(),
                range: e.selection_range.clone(),
            })
            .collect();

        if locations.len() == 1 {
            return Some(GotoDefinitionResponse::Scalar(locations.into_iter().next().unwrap()));
        } else if !locations.is_empty() {
            return Some(GotoDefinitionResponse::Array(locations));
        }
    }

    None
}

fn try_require_goto(
    doc: &Document,
    ident_node: tree_sitter::Node,
    index: &WorkspaceIndex,
) -> Option<GotoDefinitionResponse> {
    let parent = ident_node.parent()?;
    if parent.kind() != "variable" {
        return None;
    }
    let decl = parent.parent()?;
    if decl.kind() != "local_declaration" {
        return None;
    }
    let values = decl.child_by_field_name("values")?;
    let first_val = values.named_child(0)?;
    if first_val.kind() != "function_call" {
        return None;
    }
    let callee = first_val.child_by_field_name("callee")?;
    let callee_text = node_text(callee, doc.text.as_bytes());
    if callee_text != "require" {
        return None;
    }
    let args = first_val.child_by_field_name("arguments")?;
    let arg = args.named_child(0)?;

    let module_path = extract_string_content(arg, doc.text.as_bytes())?;

    let target_uri = index.require_map.get(&module_path)?;

    Some(GotoDefinitionResponse::Scalar(Location {
        uri: target_uri.clone(),
        range: Range::default(),
    }))
}

fn extract_string_content(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    fn find_string_content(n: tree_sitter::Node, source: &[u8]) -> Option<String> {
        if n.kind().starts_with("short_string_content") {
            return Some(node_text(n, source).to_string());
        }
        for i in 0..n.named_child_count() {
            if let Some(child) = n.named_child(i as u32) {
                if let Some(s) = find_string_content(child, source) {
                    return Some(s);
                }
            }
        }
        None
    }

    if node.kind() == "expression_list" {
        if let Some(first) = node.named_child(0) {
            return find_string_content(first, source);
        }
    }
    find_string_content(node, source)
}
