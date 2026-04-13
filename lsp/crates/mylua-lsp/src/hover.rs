use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::emmy::{collect_preceding_comments, parse_emmy_comments, format_annotations_markdown};
use crate::scope;
use crate::types::DefKind;
use crate::util::{node_text, position_to_byte_offset, find_node_at_position};
use crate::workspace_index::WorkspaceIndex;

pub fn hover(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceIndex,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let ident_text = node_text(ident_node, doc.text.as_bytes());

    if let Some(def) = scope::resolve_at_position(&doc.tree, &doc.text, position, uri) {
        return build_hover_for_definition(&def, all_docs);
    }

    if let Some(entries) = index.globals.get(ident_text) {
        if let Some(entry) = entries.first() {
            let fake_def = crate::types::Definition {
                name: entry.name.clone(),
                kind: entry.kind.clone(),
                range: entry.range.clone(),
                selection_range: entry.selection_range.clone(),
                uri: entry.uri.clone(),
            };
            return build_hover_for_definition(&fake_def, all_docs);
        }
    }

    None
}

fn build_hover_for_definition(
    def: &crate::types::Definition,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let doc = all_docs.get(&def.uri)?;
    let source = doc.text.as_bytes();

    let def_start = def.range.start;
    let def_byte = crate::util::position_to_byte_offset(&doc.text, def_start)?;
    let def_node = doc.tree.root_node().descendant_for_byte_range(def_byte, def_byte)?;

    let stmt_node = find_enclosing_statement(def_node);

    let comment_lines = collect_preceding_comments(stmt_node, source);
    let comment_text = comment_lines.join("\n");
    let annotations = parse_emmy_comments(&comment_text);
    let emmy_md = format_annotations_markdown(&annotations);

    let def_line = node_text(stmt_node, source)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();

    let kind_label = match def.kind {
        DefKind::LocalVariable => "local variable",
        DefKind::LocalFunction => "local function",
        DefKind::GlobalVariable => "global variable",
        DefKind::GlobalFunction => "function",
        DefKind::Parameter => "parameter",
        DefKind::ForVariable => "for variable",
    };

    let mut parts = Vec::new();
    parts.push(format!("```lua\n{}\n```", def_line));
    parts.push(format!("*{}*", kind_label));

    if !emmy_md.is_empty() {
        parts.push(format!("---\n{}", emmy_md));
    }

    let doc_lines: Vec<&str> = comment_lines
        .iter()
        .filter_map(|l| {
            let stripped = l.strip_prefix("---")?.trim();
            if stripped.starts_with('@') {
                None
            } else if stripped.is_empty() {
                None
            } else {
                Some(stripped)
            }
        })
        .collect();
    if !doc_lines.is_empty() {
        parts.push(doc_lines.join("\n"));
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(def.selection_range.clone()),
    })
}

fn find_enclosing_statement(node: tree_sitter::Node) -> tree_sitter::Node {
    let mut current = node;
    loop {
        match current.kind() {
            "function_declaration" | "local_function_declaration" | "local_declaration"
            | "assignment_statement" | "function_call_statement" => return current,
            _ => {
                if let Some(parent) = current.parent() {
                    current = parent;
                } else {
                    return current;
                }
            }
        }
    }
}
