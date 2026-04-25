use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::config::ReferencesStrategy;
use crate::document::Document;
use crate::references;
use crate::aggregation::WorkspaceAggregation;
use crate::lua_builtins::LUA_KEYWORDS;

pub fn prepare_rename(
    doc: &Document,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let offset = doc.line_index.position_to_byte_offset(doc.text.as_bytes(), position)?;
    let node = crate::util::find_node_at_position(doc.tree.root_node(), offset)?;
    let text = crate::util::node_text(node, doc.text.as_bytes());

    if LUA_KEYWORDS.contains(&text) {
        return None;
    }
    if text == "self" || text == "..." {
        return None;
    }

    let range = doc.line_index.ts_node_to_range(node, doc.text.as_bytes());
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range,
        placeholder: text.to_string(),
    })
}

/// Result of a rename request.
///
/// `Err` carries a user-facing reason the rename was rejected (e.g. invalid
/// identifier). `Ok(None)` means the identifier under the cursor can't be
/// renamed (keyword, `self`, etc.). `Ok(Some(edit))` is the success case.
pub type RenameResult = std::result::Result<Option<WorkspaceEdit>, &'static str>;

pub fn rename(
    doc: &Document,
    uri: &Uri,
    position: Position,
    new_name: &str,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
) -> RenameResult {
    if !is_valid_lua_identifier(new_name) {
        return Err("New name is not a valid Lua identifier");
    }
    if LUA_KEYWORDS.contains(&new_name) {
        return Err("New name collides with a Lua keyword");
    }

    let locations = match references::find_references(
        doc,
        uri,
        position,
        true,
        index,
        all_docs,
        &ReferencesStrategy::Merge,
    ) {
        Some(locs) => locs,
        None => return Ok(None),
    };

    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
    for loc in locations {
        changes.entry(loc.uri).or_default().push(TextEdit {
            range: loc.range,
            new_text: new_name.to_string(),
        });
    }

    Ok(Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    }))
}

/// Return `true` if `s` matches the Lua identifier production:
/// `[A-Za-z_][A-Za-z0-9_]*`. Lua source is restricted to ASCII identifiers.
pub fn is_valid_lua_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::is_valid_lua_identifier;

    #[test]
    fn valid_identifiers() {
        for s in &["x", "foo", "_bar", "_", "a1", "snake_case", "__Mixed9"] {
            assert!(is_valid_lua_identifier(s), "{} should be valid", s);
        }
    }

    #[test]
    fn invalid_identifiers() {
        for s in &["", "1foo", "a-b", "a.b", "a b", "你好", "foo$", "a?"] {
            assert!(!is_valid_lua_identifier(s), "{} should be invalid", s);
        }
    }
}
