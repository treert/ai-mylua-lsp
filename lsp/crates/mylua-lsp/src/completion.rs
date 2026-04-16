use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::resolver;
use crate::type_system::{TypeFact, SymbolicStub};
use crate::util::position_to_byte_offset;
use crate::aggregation::WorkspaceAggregation;

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end",
    "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return",
    "then", "true", "until", "while",
];

pub fn complete(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
) -> Vec<CompletionItem> {
    // Check for dot-completion: `expr.`
    if let Some(items) = try_dot_completion(doc, uri, position, index) {
        return items;
    }

    let prefix = get_prefix(doc, position);
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    collect_scope_completions(doc, uri, position, &prefix, &mut items, &mut seen);
    collect_global_completions(index, &prefix, &mut items, &mut seen);
    collect_keyword_completions(&prefix, &mut items, &mut seen);

    items
}

fn try_dot_completion(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
) -> Option<Vec<CompletionItem>> {
    let offset = position_to_byte_offset(&doc.text, position)?;
    if offset == 0 {
        return None;
    }

    let bytes = doc.text.as_bytes();
    // Walk back past any partial identifier after the dot
    let mut dot_pos = offset;
    while dot_pos > 0 && (bytes[dot_pos - 1].is_ascii_alphanumeric() || bytes[dot_pos - 1] == b'_') {
        dot_pos -= 1;
    }
    if dot_pos == 0 || (bytes[dot_pos - 1] != b'.' && bytes[dot_pos - 1] != b':') {
        return None;
    }
    let is_method = bytes[dot_pos - 1] == b':';
    let _ = is_method;

    // Find the base expression before the dot
    let base_end = dot_pos - 1;
    let mut base_start = base_end;
    while base_start > 0 && (bytes[base_start - 1].is_ascii_alphanumeric() || bytes[base_start - 1] == b'_' || bytes[base_start - 1] == b'.') {
        base_start -= 1;
    }
    if base_start == base_end {
        return None;
    }

    let base_text = std::str::from_utf8(&bytes[base_start..base_end]).ok()?;
    let prefix = std::str::from_utf8(&bytes[dot_pos..offset]).ok()?.to_string();

    let base_fact = if let Some(summary) = index.summaries.get(uri) {
        if let Some(ltf) = summary.local_type_facts.get(base_text) {
            ltf.type_fact.clone()
        } else {
            TypeFact::Stub(SymbolicStub::GlobalRef { name: base_text.to_string() })
        }
    } else {
        TypeFact::Stub(SymbolicStub::GlobalRef { name: base_text.to_string() })
    };

    let resolved = resolver::resolve_type(&base_fact, index);
    let fields = resolver::get_fields_for_type(&resolved.type_fact, index);

    if fields.is_empty() {
        return None;
    }

    let items: Vec<CompletionItem> = fields
        .into_iter()
        .filter(|f| prefix.is_empty() || f.name.starts_with(&prefix))
        .map(|f| CompletionItem {
            label: f.name.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: if f.type_display != "unknown" {
                Some(f.type_display)
            } else {
                None
            },
            ..Default::default()
        })
        .collect();

    Some(items)
}

fn get_prefix(doc: &Document, position: Position) -> String {
    let Some(offset) = position_to_byte_offset(&doc.text, position) else {
        return String::new();
    };
    let bytes = doc.text.as_bytes();
    let mut start = offset;
    while start > 0 {
        let b = bytes[start - 1];
        if b.is_ascii_alphanumeric() || b == b'_' {
            start -= 1;
        } else {
            break;
        }
    }
    String::from_utf8_lossy(&bytes[start..offset]).to_string()
}

fn collect_scope_completions(
    doc: &Document,
    _uri: &Uri,
    position: Position,
    prefix: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    let Some(offset) = position_to_byte_offset(&doc.text, position) else {
        return;
    };
    for decl in doc.scope_tree.visible_locals(offset) {
        if decl.name.starts_with(prefix) && !seen.contains(&decl.name) {
            seen.insert(decl.name.clone());
            let kind = match decl.kind {
                crate::types::DefKind::LocalFunction => CompletionItemKind::FUNCTION,
                _ => CompletionItemKind::VARIABLE,
            };
            items.push(CompletionItem {
                label: decl.name.clone(),
                kind: Some(kind),
                ..Default::default()
            });
        }
    }
}

fn collect_global_completions(
    index: &WorkspaceAggregation,
    prefix: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    for (name, entries) in &index.globals {
        if name.starts_with(prefix) && !seen.contains(name) {
            seen.insert(name.clone());
            let kind = if entries.iter().any(|e| matches!(e.kind, crate::types::DefKind::GlobalFunction)) {
                CompletionItemKind::FUNCTION
            } else {
                CompletionItemKind::VARIABLE
            };
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(kind),
                ..Default::default()
            });
        }
    }
}

fn collect_keyword_completions(
    prefix: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut HashSet<String>,
) {
    for kw in LUA_KEYWORDS {
        if kw.starts_with(prefix) && !seen.contains(*kw) {
            seen.insert(kw.to_string());
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }
}
