use crate::util::{node_text, LineIndex};
use tower_lsp_server::ls_types::*;

/// Walk every `table_constructor` and report keys that appear more
/// than once. Only named keys (`{ a = 1, a = 2 }`) and static
/// bracket-key literals (`{ [1] = 'x', [1] = 'y' }`) can be reliably
/// compared at summary-build time; dynamic `[expr]` keys are skipped.
pub(super) fn check_duplicate_table_keys(
    root: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let mut cursor = root.walk();
    check_duplicate_keys_recursive(&mut cursor, source, diagnostics, severity, line_index);
}

fn check_duplicate_keys_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    if node.kind() == "table_constructor" {
        // Skip bracket-key-only tables — they are data-mapping tables
        // where duplicate key checking is not useful and would be
        // expensive for thousands of entries.
        if crate::util::is_bracket_key_only_table(node) {
            return;
        }
        let mut seen: std::collections::HashMap<String, Range> = std::collections::HashMap::new();
        for i in 0..node.named_child_count() {
            let Some(field_list) = node.named_child(i as u32) else {
                continue;
            };
            let fields = if field_list.kind() == "field_list" {
                field_list
            } else {
                continue;
            };
            for j in 0..fields.named_child_count() {
                let Some(field) = fields.named_child(j as u32) else {
                    continue;
                };
                if field.kind() != "field" {
                    continue;
                }
                let Some(key_text) = extract_field_key(field, source) else {
                    continue;
                };
                if let Some(first_range) = seen.get(&key_text) {
                    let range = line_index.ts_node_to_range(field, source);
                    diagnostics.push(Diagnostic {
                        range,
                        severity: Some(severity),
                        source: Some("mylua".to_string()),
                        message: format!(
                            "Duplicate table key '{}' (first defined at line {})",
                            key_text,
                            first_range.start.line + 1,
                        ),
                        ..Default::default()
                    });
                } else {
                    seen.insert(key_text, line_index.ts_node_to_range(field, source));
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            check_duplicate_keys_recursive(cursor, source, diagnostics, severity, line_index);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn extract_field_key(field: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Identifier key: `a = 1`
    if let Some(key) = field.child_by_field_name("key") {
        match key.kind() {
            "identifier" => {
                return Some(node_text(key, source).to_string());
            }
            "string" => {
                // Bracket string key: `["a"] = 1` — normalize by text
                // content excluding quotes so that `["a"]` vs `['a']`
                // dedup.
                let t = node_text(key, source);
                return Some(
                    t.trim_matches(|c| c == '"' || c == '\'' || c == '[' || c == ']')
                        .to_string(),
                );
            }
            "number" => {
                return Some(format!("num:{}", node_text(key, source)));
            }
            _ => return None,
        }
    }
    None
}
