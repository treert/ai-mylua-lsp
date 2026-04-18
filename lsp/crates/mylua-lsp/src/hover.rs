use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::emmy::{collect_preceding_comments, parse_emmy_comments, format_annotations_markdown};
use crate::resolver;
use crate::type_system::TypeFact;
use crate::types::DefKind;
use crate::util::{node_text, position_to_byte_offset, find_node_at_position, walk_ancestors};
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

    lsp_log!(
        "[hover] ident='{}' kind='{}' parent='{}'",
        ident_text,
        ident_node.kind(),
        ident_node.parent().map_or("none", |p| p.kind()),
    );

    // Walk ancestors to find a dotted access (`variable` node with
    // object+field fields, or the legacy `field_expression`) where this
    // identifier is the `field`. That path is AST-driven and recurses
    // through the resolver — it correctly handles `a[1].b`, `a:m().c`, etc.
    //
    // Uses the shared `walk_ancestors` helper so we never spin on a
    // malformed tree; if it hits its safety cap it bails out with a
    // log warning rather than looping.
    //
    // Closure contract:
    //   Some(Some(hover)) → walker stops, `hover()` returns that hover
    //   Some(None)        → walker stops, but we fall through to the
    //                       scope / type_shard / global_shard paths
    //                       (the dotted-field / function_name match
    //                       was found but produced no hover content)
    //   None              → keep walking
    //
    // NOTE: `hover_at_declaration` currently always returns `Some`, so
    // the `function_name` branch below effectively short-circuits. If
    // it ever gains a failure path (returning `None`), the current
    // `Some(hover_at_declaration(...))` will silently fall through —
    // update the invariant at `hover_at_declaration` if that changes.
    if let Some(result) = walk_ancestors(ident_node, |p| {
        if matches!(p.kind(), "variable" | "field_expression") {
            let field_is_ident = p
                .child_by_field_name("field")
                .map(|f| f.id() == ident_node.id())
                .unwrap_or(false);
            if field_is_ident {
                return Some(hover_variable_field(p, doc, uri, index, all_docs));
            }
        }
        if p.kind() == "function_name" {
            if let Some(decl) = p.parent() {
                if decl.kind() == "function_declaration"
                    || decl.kind() == "local_function_declaration"
                {
                    return Some(hover_at_declaration(decl, doc));
                }
            }
        }
        None
    }) {
        if result.is_some() {
            return result;
        }
        // Match hit but no hover produced — fall through.
    }

    if let Some(def) = doc.scope_tree.resolve(byte_offset, ident_text, uri) {
        let type_info = resolve_local_type_info(uri, ident_text, index);
        lsp_log!("[hover] scope resolved '{}', type_info={:?}", ident_text, type_info);
        return build_hover_for_definition(&def, all_docs, type_info.as_deref());
    }

    // Check if ident is a type name (e.g. hovering on "Foo" in `---@type Foo`)
    if let Some(candidates) = index.type_shard.get(ident_text) {
        if let Some(candidate) = candidates.first() {
            if let Some(summary) = index.summaries.get(&candidate.source_uri) {
                for td in &summary.type_definitions {
                    if td.name == ident_text {
                        let mut parts = Vec::new();
                        let class_header = match td.kind {
                            crate::summary::TypeDefinitionKind::Alias => {
                                let alias_display = td.alias_type.as_ref()
                                    .map(|t| format!("{}", t))
                                    .unwrap_or_else(|| "unknown".to_string());
                                format!("---@alias {} {}", td.name, alias_display)
                            }
                            crate::summary::TypeDefinitionKind::Enum => {
                                format!("---@enum {}", td.name)
                            }
                            _ => {
                                if td.parents.is_empty() {
                                    format!("---@class {}", td.name)
                                } else {
                                    format!("---@class {} : {}", td.name, td.parents.join(", "))
                                }
                            }
                        };
                        parts.push(format!("```lua\n{}\n```", class_header));
                        let kind_label = match td.kind {
                            crate::summary::TypeDefinitionKind::Class => "class",
                            crate::summary::TypeDefinitionKind::Alias => "alias",
                            crate::summary::TypeDefinitionKind::Enum => "enum",
                        };
                        parts.push(format!("*{}*", kind_label));
                        if !td.fields.is_empty() {
                            let fields_md: Vec<String> = td.fields.iter()
                                .map(|f| format!("- `{}`: `{}`", f.name, f.type_fact))
                                .collect();
                            parts.push(fields_md.join("\n"));
                        }
                        // Include doc comments from the definition site
                        if let Some(def_doc) = all_docs.get(&candidate.source_uri) {
                            let def_byte = crate::util::position_to_byte_offset(
                                &def_doc.text, td.range.start,
                            );
                            if let Some(db) = def_byte {
                                if let Some(def_node) = def_doc.tree.root_node()
                                    .descendant_for_byte_range(db, db)
                                {
                                    let stmt = find_enclosing_statement(def_node);
                                    let comment_lines = collect_preceding_comments(stmt, def_doc.text.as_bytes());
                                    let doc_text = extract_doc_lines(&comment_lines);
                                    if !doc_text.is_empty() {
                                        parts.push(doc_text);
                                    }
                                }
                            }
                        }
                        return Some(Hover {
                            contents: HoverContents::Markup(MarkupContent {
                                kind: MarkupKind::Markdown,
                                value: parts.join("\n\n"),
                            }),
                            range: None,
                        });
                    }
                }
            }
        }
    }

    // Synthesize a `Definition` from the best global candidate so the
    // hover renderer below can reuse `build_hover_for_definition`.
    // The candidate isn't an AST-local decl (`DocumentSummary.local_type_facts`),
    // so we fabricate one carrying the candidate's source location +
    // global kind — purely for the shared formatter.
    let global_info = index.global_shard.get(ident_text).and_then(|candidates| {
        let candidate = candidates.first()?;
        let def_kind = match candidate.kind {
            crate::summary::GlobalContributionKind::Function => crate::types::DefKind::GlobalFunction,
            _ => crate::types::DefKind::GlobalVariable,
        };
        Some((crate::types::Definition {
            name: candidate.name.clone(),
            kind: def_kind,
            range: candidate.range,
            selection_range: candidate.selection_range,
            uri: candidate.source_uri.clone(),
        }, candidates.len(), candidate.source_uri.clone()))
    });
    if let Some((synth_def, entry_count, source_uri)) = global_info {
        let resolved = resolver::resolve_type(
            &TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                name: ident_text.to_string(),
            }),
            index,
        );
        let mut type_info = format_resolved_type(&resolved.type_fact);
        if entry_count > 1 {
            type_info.push_str(&format!(" ({} definitions)", entry_count));
        }
        if let Some(summary) = index.summaries.get(&source_uri) {
            if let Some(fs) = summary.function_summaries.get(ident_text) {
                if !fs.overloads.is_empty() {
                    type_info.push_str("\n\nOverloads:");
                    for overload in &fs.overloads {
                        type_info.push_str(&format!("\n- `{}`", format_signature(overload)));
                    }
                }
            }
        }
        return build_hover_for_definition(&synth_def, all_docs, Some(&type_info));
    }

    None
}

/// Build hover directly from a function/local declaration node at the definition site.
/// Build hover directly from a function/local declaration node at
/// the definition site.
///
/// **Invariant**: currently always returns `Some(Hover)` — the
/// caller in `hover()` relies on this so the `function_name` branch
/// of the ancestor walker short-circuits rather than falling through
/// to the scope / type_shard paths. If this function gains a real
/// failure path, update the walker's closure contract comment too.
fn hover_at_declaration(
    decl_node: tree_sitter::Node,
    doc: &Document,
) -> Option<Hover> {
    let source = doc.text.as_bytes();

    let comment_lines = collect_preceding_comments(decl_node, source);
    let comment_text = comment_lines.join("\n");
    let annotations = parse_emmy_comments(&comment_text);
    let emmy_md = format_annotations_markdown(&annotations);

    let def_line = node_text(decl_node, source)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();

    let kind_label = match decl_node.kind() {
        "local_function_declaration" => "local function",
        _ => "function",
    };

    let mut parts = Vec::new();
    parts.push(format!("```lua\n{}\n```", def_line));
    parts.push(format!("*{}*", kind_label));

    if !emmy_md.is_empty() {
        parts.push(format!("---\n{}", emmy_md));
    }

    let doc_text = extract_doc_lines(&comment_lines);
    if !doc_text.is_empty() {
        parts.push(doc_text);
    }

    let name_node = decl_node.child_by_field_name("name");
    let range = name_node.map(|n| crate::util::ts_node_to_range(n, source));

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range,
    })
}

/// AST-driven hover for a dotted access: `var_node` is the enclosing
/// `variable` (or `field_expression`) whose `field` is the identifier
/// the user clicked. Handles arbitrary bases via `infer_node_type` which
/// recurses through nested variables / subscripts / call returns.
fn hover_variable_field(
    var_node: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let source = doc.text.as_bytes();
    let object = var_node.child_by_field_name("object")?;
    let field = var_node.child_by_field_name("field")?;
    let field_name = node_text(field, source).to_string();

    let base_fact = infer_node_type(object, source, uri, index);
    lsp_log!(
        "[hover_var_field] base='{}' base_fact={:?} field='{}'",
        node_text(object, source),
        base_fact,
        field_name,
    );
    let resolved = resolver::resolve_field_chain(&base_fact, &[field_name.clone()], index);
    lsp_log!("[hover_var_field] resolved={:?}", resolved.type_fact);

    let type_display = format_resolved_type(&resolved.type_fact);

    if let (Some(def_uri), Some(def_range)) = (&resolved.def_uri, &resolved.def_range) {
        if all_docs.contains_key(def_uri) {
            let synth_def = crate::types::Definition {
                name: field_name.clone(),
                kind: DefKind::GlobalVariable,
                range: *def_range,
                selection_range: *def_range,
                uri: def_uri.clone(),
            };
            return build_hover_for_definition(&synth_def, all_docs, Some(&type_display));
        }
    }

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
        range: Some(crate::util::ts_node_to_range(field, source)),
    })
}

/// Recursively infer the type of an AST expression node.
///
/// The mylua grammar uses `variable` nodes for both plain identifiers and
/// dotted access (`a.b.c` is a `variable` whose `object` field is another
/// `variable` and whose `field` field is an identifier). `field_expression`
/// is kept as a legacy alias for future grammar revisions.
///
/// For bases that are *not* purely dotted — e.g. `a[1].b` (subscript) or
/// `a:m().c` (method call) — we return `Unknown` rather than concocting a
/// bogus `GlobalRef("a[1]")` stub. Inferring those bases requires either
/// table `array_element_type` lookup or replaying the call-return logic
/// from `summary_builder`; neither is implemented here yet.
pub fn infer_node_type(
    node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> TypeFact {
    match node.kind() {
        "variable" | "field_expression" => {
            if let (Some(object), Some(field)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("field"),
            ) {
                let base_fact = infer_node_type(object, source, uri, index);
                let field_name = node_text(field, source).to_string();
                let resolved = resolver::resolve_field_chain(&base_fact, &[field_name], index);
                return resolved.type_fact;
            }
            // `variable` wraps either a single identifier or a subscript
            // (`a[1]`). A single identifier has one child whose text equals
            // the whole node text; treat that as local/global lookup.
            // Anything else (subscript, malformed) → Unknown to avoid
            // constructing meaningless `GlobalRef` names.
            if node.named_child_count() == 1 {
                if let Some(child) = node.named_child(0) {
                    if child.kind() == "identifier" {
                        // Use the identifier child's text directly rather
                        // than the full `variable` span — safer under ERROR
                        // recovery where the wrapper's span may include
                        // extraneous characters.
                        let text = node_text(child, source);
                        if let Some(summary) = index.summaries.get(uri) {
                            if let Some(ltf) = summary.local_type_facts.get(text) {
                                return ltf.type_fact.clone();
                            }
                        }
                        return TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                            name: text.to_string(),
                        });
                    }
                }
            }
            TypeFact::Unknown
        }
        "parenthesized_expression" => {
            // `(expr)` — unwrap and recurse into the single enclosed
            // expression so `(foo()).x` style bases resolve correctly.
            node.named_child(0)
                .map(|inner| infer_node_type(inner, source, uri, index))
                .unwrap_or(TypeFact::Unknown)
        }
        "identifier" => {
            let text = node_text(node, source);
            if let Some(summary) = index.summaries.get(uri) {
                if let Some(ltf) = summary.local_type_facts.get(text) {
                    return ltf.type_fact.clone();
                }
            }
            TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                name: text.to_string(),
            })
        }
        _ => TypeFact::Unknown,
    }
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
    // Specialize `Known(Function(sig))` to a fully formatted
    // `fun(a: T, b: U): R` signature — the default `Display` for
    // `KnownType::Function` renders just `"function"` which is
    // info-less on the hover "Type:" line.
    if let TypeFact::Known(crate::type_system::KnownType::Function(sig)) = fact {
        return format_signature(sig);
    }
    format!("{}", fact)
}

fn format_signature(sig: &crate::type_system::FunctionSignature) -> String {
    let params: Vec<String> = sig.params.iter().map(|p| {
        if p.type_fact == TypeFact::Unknown {
            p.name.clone()
        } else {
            format!("{}: {}", p.name, p.type_fact)
        }
    }).collect();
    let returns: Vec<String> = sig.returns.iter().map(|r| format!("{}", r)).collect();
    if returns.is_empty() {
        format!("fun({})", params.join(", "))
    } else {
        format!("fun({}): {}", params.join(", "), returns.join(", "))
    }
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

    let doc_text = extract_doc_lines(&comment_lines);
    if !doc_text.is_empty() {
        parts.push(doc_text);
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(def.selection_range.clone()),
    })
}

/// Extract plain documentation text from collected comment lines.
/// Strips `---` or `--` prefix, excludes `@`-prefixed annotation lines.
fn extract_doc_lines(comment_lines: &[String]) -> String {
    let lines: Vec<&str> = comment_lines
        .iter()
        .filter_map(|l| {
            let stripped = if let Some(s) = l.strip_prefix("---") {
                s.trim()
            } else if let Some(s) = l.strip_prefix("--") {
                s.trim()
            } else {
                return None;
            };
            if stripped.starts_with('@') || stripped.is_empty() {
                None
            } else {
                Some(stripped)
            }
        })
        .collect();
    lines.join("\n")
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
