use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::references;
use crate::workspace_index::WorkspaceIndex;

pub fn prepare_rename(
    doc: &Document,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let offset = crate::util::position_to_byte_offset(&doc.text, position)?;
    let node = crate::util::find_node_at_position(doc.tree.root_node(), offset)?;
    let range = crate::util::ts_node_to_range(node);
    let text = crate::util::node_text(node, doc.text.as_bytes()).to_string();
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range,
        placeholder: text,
    })
}

pub fn rename(
    doc: &Document,
    uri: &Uri,
    position: Position,
    new_name: &str,
    index: &WorkspaceIndex,
    all_docs: &HashMap<Uri, Document>,
) -> Option<WorkspaceEdit> {
    let locations = references::find_references(
        doc,
        uri,
        position,
        true,
        index,
        all_docs,
    )?;

    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for loc in locations {
        changes.entry(loc.uri).or_default().push(TextEdit {
            range: loc.range,
            new_text: new_name.to_string(),
        });
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}
