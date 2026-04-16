use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::references;
use crate::aggregation::WorkspaceAggregation;

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for",
    "function", "goto", "if", "in", "local", "nil", "not", "or",
    "repeat", "return", "then", "true", "until", "while",
];

pub fn prepare_rename(
    doc: &Document,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let offset = crate::util::position_to_byte_offset(&doc.text, position)?;
    let node = crate::util::find_node_at_position(doc.tree.root_node(), offset)?;
    let text = crate::util::node_text(node, doc.text.as_bytes());

    if LUA_KEYWORDS.contains(&text) {
        return None;
    }
    if text == "self" || text == "..." {
        return None;
    }

    let range = crate::util::ts_node_to_range(node);
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range,
        placeholder: text.to_string(),
    })
}

pub fn rename(
    doc: &Document,
    uri: &Uri,
    position: Position,
    new_name: &str,
    index: &WorkspaceAggregation,
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
