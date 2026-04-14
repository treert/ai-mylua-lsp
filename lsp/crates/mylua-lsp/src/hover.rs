use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::emmy::{collect_preceding_comments, parse_emmy_comments, format_annotations_markdown};
use crate::resolver;
use crate::scope;
use crate::type_system::TypeFact;
use crate::types::DefKind;
use crate::util::{node_text, position_to_byte_offset, find_node_at_position};
use crate::aggregation::WorkspaceAggregation;

pub fn hover(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let ident_text = node_text(ident_node, doc.text.as_bytes());

    // Try field expression hover: `obj.field` → resolve chain
    if let Some(parent) = ident_node.parent() {
        if parent.kind() == "field_expression" {
            if let Some(result) = hover_field_expression(parent, doc, uri, index, all_docs) {
                return Some(result);
            }
        }
    }

    if let Some(def) = scope::resolve_at_position(&doc.tree, &doc.text, position, uri) {
        let type_info = resolve_local_type_info(uri, ident_text, index);
        return build_hover_for_definition(&def, all_docs, type_info.as_deref());
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
            let resolved = resolver::resolve_type(
                &TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                    name: ident_text.to_string(),
                }),
                index,
            );
            let type_info = format_resolved_type(&resolved.type_fact);
            return build_hover_for_definition(&fake_def, all_docs, Some(&type_info));
        }
    }

    None
}

fn hover_field_expression(
    field_expr: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let source = doc.text.as_bytes();
    let object = field_expr.child_by_field_name("object")?;
    let field = field_expr.child_by_field_name("field")?;
    let field_name = node_text(field, source).to_string();

    let base_fact = infer_node_type(object, source, uri, index);
    let resolved = resolver::resolve_field_chain(&base_fact, &[field_name.clone()], index);

    let type_display = format_resolved_type(&resolved.type_fact);

    // If we have a definition location, show full hover from there
    if let (Some(def_uri), Some(def_range)) = (&resolved.def_uri, &resolved.def_range) {
        if let Some(_def_doc) = all_docs.get(def_uri) {
            let fake_def = crate::types::Definition {
                name: field_name.clone(),
                kind: DefKind::GlobalVariable,
                range: *def_range,
                selection_range: *def_range,
                uri: def_uri.clone(),
            };
            return build_hover_for_definition(&fake_def, all_docs, Some(&type_display));
        }
    }

    // Fallback: show just the type
    let mut parts = Vec::new();
    parts.push(format!("```lua\n(field) {}\n```", field_name));
    if type_display != "unknown" {
        parts.push(format!("Type: `{}`", type_display));
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(crate::util::ts_node_to_range(field)),
    })
}

fn infer_node_type(
    node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> TypeFact {
    let text = node_text(node, source);

    // Check if it's a known local in the summary
    if let Some(summary) = index.summaries.get(uri) {
        if let Some(ltf) = summary.local_type_facts.get(text) {
            return ltf.type_fact.clone();
        }
    }

    TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
        name: text.to_string(),
    })
}

fn resolve_local_type_info(
    uri: &Uri,
    name: &str,
    index: &mut WorkspaceAggregation,
) -> Option<String> {
    let resolved = resolver::resolve_local_in_file(uri, name, index);
    let display = format_resolved_type(&resolved.type_fact);
    if display == "unknown" {
        None
    } else {
        Some(display)
    }
}

fn format_resolved_type(fact: &TypeFact) -> String {
    format!("{}", fact)
}

fn build_hover_for_definition(
    def: &crate::types::Definition,
    all_docs: &std::collections::HashMap<Uri, Document>,
    type_info: Option<&str>,
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

    if let Some(ti) = type_info {
        if ti != "unknown" {
            parts.push(format!("Type: `{}`", ti));
        }
    }

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
