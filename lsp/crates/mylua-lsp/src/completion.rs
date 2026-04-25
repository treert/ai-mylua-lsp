use std::collections::HashSet;
use std::fmt::Write;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::type_inference;
use crate::resolver;
use crate::util::{node_text, position_to_byte_offset, walk_ancestors};
use crate::aggregation::WorkspaceAggregation;
use crate::lua_builtins::LUA_KEYWORDS;

/// Build the resolve-payload attached to a completion item so that
/// `completion_resolve` can re-locate the symbol on demand.
fn resolve_data(kind: &str, uri: Option<&Uri>, name: &str) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    obj.insert("kind".into(), serde_json::Value::String(kind.to_string()));
    obj.insert("name".into(), serde_json::Value::String(name.to_string()));
    if let Some(u) = uri {
        obj.insert("uri".into(), serde_json::Value::String(u.to_string()));
    }
    serde_json::Value::Object(obj)
}

/// EmmyLua annotation tags that can appear after `---@`.
const EMMY_TAGS: &[&str] = &[
    "class", "field", "param", "return", "type", "alias", "enum",
    "generic", "overload", "vararg", "deprecated", "async", "nodiscard",
    "see", "meta", "diagnostic", "cast", "operator", "private", "protected",
    "package", "public", "readonly", "version",
];

pub fn complete(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
) -> Vec<CompletionItem> {
    // `require("<here>")` string-literal completion — highest priority.
    if let Some(items) = try_require_path_completion(doc, position, index) {
        return items;
    }

    // `---@<tag>` completion inside emmy comments.
    if let Some(items) = try_emmy_tag_completion(doc, position) {
        return items;
    }

    // Dot / method completion.
    if let Some(items) = try_dot_completion_ast(doc, uri, position, index) {
        return items;
    }

    // Fallback: identifier prefix completion (locals + globals + keywords).
    let prefix = get_prefix(doc, position);
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    collect_scope_completions(doc, uri, position, &prefix, &mut items, &mut seen);
    collect_global_completions(index, &prefix, &mut items, &mut seen);
    collect_keyword_completions(&prefix, &mut items, &mut seen);

    items
}

// ---------------------------------------------------------------------------
// `---@` annotation tag completion
// ---------------------------------------------------------------------------

/// Returns completions for EmmyLua annotation tags when the cursor sits
/// right after `---@` (with optional partial tag text). Triggered both via
/// the `@` trigger character and manual invocation.
fn try_emmy_tag_completion(doc: &Document, position: Position) -> Option<Vec<CompletionItem>> {
    let offset = position_to_byte_offset(&doc.text, position)?;
    let bytes = doc.text.as_bytes();
    // EmmyLua tags are lowercase ASCII letters only (`class`, `param`, …).
    // We deliberately do NOT skip digits / underscores here; doing so would
    // let the cursor "eat" into an adjacent Lua identifier abutting the
    // cursor, producing bogus matches.
    let mut i = offset;
    while i > 0 && bytes[i - 1].is_ascii_alphabetic() {
        i -= 1;
    }
    if i == 0 || bytes[i - 1] != b'@' {
        return None;
    }
    // Require the current line (trim_start) to begin with `--`, covering
    // both `---@tag` (EmmyLua canonical) and `-- @tag` (occasional).
    let at_pos = i - 1;
    let line_start = bytes[..at_pos]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    let prefix_to_at = std::str::from_utf8(&bytes[line_start..at_pos]).ok()?.trim_start();
    if !prefix_to_at.starts_with("--") {
        return None;
    }

    let partial = std::str::from_utf8(&bytes[i..offset]).ok()?;
    let items = EMMY_TAGS
        .iter()
        .filter(|t| t.starts_with(partial))
        .map(|t| CompletionItem {
            label: t.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            detail: Some("EmmyLua annotation".to_string()),
            insert_text: Some(t.to_string()),
            ..Default::default()
        })
        .collect();
    Some(items)
}

// ---------------------------------------------------------------------------
// `require("<here>")` path completion
// ---------------------------------------------------------------------------

/// If the cursor is inside the string literal argument of a `require(...)`
/// call, return completion items for every module path registered in the
/// workspace's `require_map`.
fn try_require_path_completion(
    doc: &Document,
    position: Position,
    index: &WorkspaceAggregation,
) -> Option<Vec<CompletionItem>> {
    let offset = position_to_byte_offset(&doc.text, position)?;
    let node = doc
        .tree
        .root_node()
        .descendant_for_byte_range(offset, offset)?;

    // Walk up looking for a string node whose ancestor is `require("...")`.
    // `walk_ancestors` caps depth with a shared safety limit + logs on
    // overflow (see `util::ANCESTOR_WALK_LIMIT`).
    let source = doc.text.as_bytes();
    let string_node = if matches!(node.kind(), "short_string" | "string") {
        Some(node)
    } else {
        walk_ancestors(node, |p| {
            if matches!(p.kind(), "short_string" | "string") {
                Some(p)
            } else {
                None
            }
        })
    };
    let string_node = string_node?;
    if !is_inside_require_call(string_node, source) {
        return None;
    }
    let mut items: Vec<CompletionItem> = index
        .all_module_names()
        .into_iter()
        .map(|m| CompletionItem {
            label: m.clone(),
            kind: Some(CompletionItemKind::MODULE),
            detail: Some("require target".to_string()),
            insert_text: Some(m),
            ..Default::default()
        })
        .collect();
    items.sort_by(|a, b| a.label.cmp(&b.label));
    Some(items)
}

/// Returns true if `string_node` is an argument of a `require(...)`
/// call — its nearest `function_call` ancestor has `callee` reading
/// `require`. Uses `walk_ancestors` for depth-capped traversal.
fn is_inside_require_call(string_node: tree_sitter::Node, source: &[u8]) -> bool {
    walk_ancestors(string_node, |p| {
        if p.kind() == "function_call" {
            let callee_is_require = p
                .child_by_field_name("callee")
                .map(|callee| node_text(callee, source) == "require")
                .unwrap_or(false);
            Some(callee_is_require)
        } else {
            None
        }
    })
    .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Dot / method completion — AST-driven
// ---------------------------------------------------------------------------

/// AST-driven dot/method completion. Replaces the old string `splitn('.')`
/// base analysis so that bases like `a.b.c`, method returns (`foo():x`) and
/// subscripts (`arr[1].`) are treated via the resolver chain.
fn try_dot_completion_ast(
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
    // Walk back past any partial identifier after the dot/colon.
    let mut dot_pos = offset;
    while dot_pos > 0
        && (bytes[dot_pos - 1].is_ascii_alphanumeric() || bytes[dot_pos - 1] == b'_')
    {
        dot_pos -= 1;
    }
    if dot_pos == 0 || (bytes[dot_pos - 1] != b'.' && bytes[dot_pos - 1] != b':') {
        return None;
    }
    let is_method = bytes[dot_pos - 1] == b':';
    let base_end = dot_pos - 1;
    let prefix = std::str::from_utf8(&bytes[dot_pos..offset]).ok()?.to_string();

    // Find the AST node representing the base expression — the node ending
    // exactly at `base_end`.
    let base_node = find_base_expression_node(doc.tree.root_node(), base_end)?;
let base_fact = type_inference::infer_node_type(base_node, bytes, uri, index);

    let fields = resolver::get_fields_for_type(&base_fact, Some(uri), index);
    if fields.is_empty() {
        return None;
    }

    let items: Vec<CompletionItem> = fields
        .into_iter()
        .filter(|f| prefix.is_empty() || f.name.starts_with(&prefix))
        .filter(|f| {
            if is_method {
                f.is_function || f.type_display == "unknown"
            } else {
                true
            }
        })
        .map(|f| {
            let kind = if is_method || f.is_function {
                CompletionItemKind::METHOD
            } else {
                CompletionItemKind::FIELD
            };
            CompletionItem {
                label: f.name.clone(),
                kind: Some(kind),
                detail: if f.type_display != "unknown" {
                    Some(f.type_display)
                } else {
                    None
                },
                ..Default::default()
            }
        })
        .collect();

    Some(items)
}

/// Locate the AST node whose span ends at `end_byte` — this is the base
/// expression to the left of the dot/colon. Prefers the largest such node
/// (e.g. the full `a.b.c` variable, not the trailing identifier `c`).
/// `parenthesized_expression` is accepted so `(foo()).x` / `(x or y).name`
/// style bases are resolved through `type_inference::infer_node_type`.
fn find_base_expression_node(
    root: tree_sitter::Node,
    end_byte: usize,
) -> Option<tree_sitter::Node> {
    if end_byte == 0 {
        return None;
    }
    let mut n = root.descendant_for_byte_range(end_byte - 1, end_byte - 1)?;
    while let Some(parent) = n.parent() {
        if parent.end_byte() == end_byte && is_base_expr_kind(parent.kind()) {
            n = parent;
            continue;
        }
        break;
    }
    if is_base_expr_kind(n.kind()) {
        Some(n)
    } else {
        None
    }
}

fn is_base_expr_kind(kind: &str) -> bool {
    matches!(
        kind,
        "variable"
            | "field_expression"
            | "function_call"
            | "identifier"
            | "parenthesized_expression"
    )
}

// ---------------------------------------------------------------------------
// Identifier / keyword / global fallbacks
// ---------------------------------------------------------------------------

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
    uri: &Uri,
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
                data: Some(resolve_data("local", Some(uri), &decl.name)),
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
    for (name, candidates) in &index.global_shard {
        if name.starts_with(prefix) && !seen.contains(name) {
            seen.insert(name.clone());
            let kind = if candidates.iter().any(|c| {
                matches!(c.kind, crate::summary::GlobalContributionKind::Function)
            }) {
                CompletionItemKind::FUNCTION
            } else {
                CompletionItemKind::VARIABLE
            };
            items.push(CompletionItem {
                label: name.clone(),
                kind: Some(kind),
                data: Some(resolve_data("global", None, name)),
                ..Default::default()
            });
        }
    }
}

/// `completionItem/resolve` — enrich an item with `documentation`
/// / `detail` on demand. Called by the client only when the user
/// actually highlights the item, so we can defer expensive work
/// (type-fact resolution, cross-file signature lookup) out of the
/// initial completion response.
///
/// Reads `item.data` produced by `resolve_data` to re-locate the
/// symbol. Missing or unrecognized `data` → return the item
/// unchanged (e.g. keyword items, EmmyLua tags, `require` path
/// items all carry their own `detail` already and don't need
/// further resolve).
pub fn resolve_completion(
    item: CompletionItem,
    index: &WorkspaceAggregation,
) -> CompletionItem {
    // Extract fields up front (owned Strings) so we can freely
    // hand `item` into the per-kind helpers without clashing with
    // the borrow.
    let (kind, name, uri_str) = match item.data.as_ref() {
        Some(data) => (
            data.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            data.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            data.get("uri").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        ),
        None => return item,
    };

    match kind.as_str() {
        "global" => resolve_global_item(item, index, &name),
        "local" => {
            let Ok(uri) = uri_str.parse::<Uri>() else {
                return item;
            };
            resolve_local_item(item, index, &uri, &name)
        }
        _ => item,
    }
}

fn resolve_global_item(
    mut item: CompletionItem,
    index: &WorkspaceAggregation,
    name: &str,
) -> CompletionItem {
    let Some(candidates) = index.global_shard.get(name) else {
        return item;
    };
    let Some(best) = candidates.first() else {
        return item;
    };

    let mut detail_parts: Vec<String> = Vec::new();
    // Type summary from the candidate's type_fact.
    detail_parts.push(format!("{}", best.type_fact));
    // File origin — trailing path segment for brevity.
    let origin = best
        .source_uri
        .as_str()
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string();
    if !origin.is_empty() {
        detail_parts.push(format!("(in {})", origin));
    }
    item.detail = Some(detail_parts.join(" "));

    // Pull richer cross-file function summary documentation when the
    // target file is indexed.
    if let Some(summary) = index.summaries.get(&best.source_uri) {
        if let Some(fs) = summary.function_summaries.get(name) {
            let mut md = String::new();
            md.push_str("```lua\n");
            let _ = write!(md, "function {}(", name);
            let params: Vec<String> = fs
                .signature
                .params
                .iter()
                .map(|p| p.name.clone())
                .collect();
            md.push_str(&params.join(", "));
            md.push_str(")\n```");
            if !fs.overloads.is_empty() {
                let _ = write!(md, "\n\n+{} overload(s)", fs.overloads.len());
            }
            item.documentation = Some(Documentation::MarkupContent(MarkupContent {
                kind: MarkupKind::Markdown,
                value: md,
            }));
        }
    }
    item
}

fn resolve_local_item(
    mut item: CompletionItem,
    index: &WorkspaceAggregation,
    uri: &Uri,
    name: &str,
) -> CompletionItem {
    let Some(summary) = index.summaries.get(uri) else {
        return item;
    };
    if let Some(ltf) = summary.local_type_facts.get(name) {
        item.detail = Some(format!("local {}: {}", name, ltf.type_fact));
    } else {
        item.detail = Some(format!("local {}", name));
    }
    item
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

