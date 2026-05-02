use std::fmt::Write;
use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::emmy::{collect_preceding_comments, collect_trailing_comment, parse_emmy_comments, format_annotations_markdown};
use crate::resolver;
use crate::type_system::TypeFact;
use crate::types::DefKind;
use crate::type_inference::infer_node_type;
use crate::util::{node_text, find_node_at_position, walk_ancestors, extract_field_chain, LineIndex};
use crate::aggregation::WorkspaceAggregation;

pub fn hover(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let byte_offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;
    if let Some(type_name) = crate::emmy::emmy_type_name_at_byte(doc.source(), byte_offset) {
        return hover_type_name(&type_name, index, all_docs);
    }

    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset)?;
    let ident_text = node_text(ident_node, doc.source());

    // Request-path logs: bounded by user interaction (one hover click),
    // never triggered during cold-start indexing. Invaluable for
    // diagnosing "hover returned nothing / wrong type" bug reports —
    // the log line pinpoints exactly what identifier the LSP saw at
    // the cursor position, its AST kind, and its parent context
    // before any of the resolution branches below can swallow it.
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
    // the `function_name` tail branch below effectively short-circuits.
    // For non-tail identifiers we *deliberately* return `Some(None)`
    // to stop walking (there's no ancestor above `function_name` that
    // would want to match) and hand control to the scope / type_shard
    // / global_shard paths below — so `function A1213:f()` hover on
    // `A1213` falls through to normal variable resolution and (since
    // `A1213` is undefined) returns no hover, rather than impersonating
    // a function. If `hover_at_declaration` ever gains a real failure
    // path, revisit the tail `return Some(...)` too.
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
        // `obj:method(...)` — the `method` identifier on a `function_call`
        // node. Infer the base type and resolve the method as a field so
        // hover shows the method's declaration + type info.
        if p.kind() == "function_call" {
            let method_is_ident = p
                .child_by_field_name("method")
                .map(|m| m.id() == ident_node.id())
                .unwrap_or(false);
            if method_is_ident {
                return Some(hover_method_call(p, doc, uri, index, all_docs));
            }
        }
        if p.kind() == "function_name" {
            if let Some(decl) = p.parent() {
                if decl.kind() == "function_declaration"
                    || decl.kind() == "local_function_declaration"
                {
                    // `function a.b.c()` / `function a:m()` — only short
                    // circuit to the whole-declaration hover when the
                    // cursor is on the *last* identifier (the method
                    // name `m` / tail field `c`, or the bare decl name
                    // `a` when there's no separator). Intermediate or
                    // base identifiers refer to existing table values
                    // and must fall through to scope / global / type
                    // resolution so `A1213:f()` on unknown `A1213`
                    // doesn't claim it's a function.
                    if is_function_name_tail(p, ident_node) {
                        return Some(hover_at_declaration(decl, doc));
                    }
                    // Match hit but the clicked identifier is the base
                    // / intermediate segment — let outer branches try.
                    return Some(None);
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
        let type_info = resolve_local_type_info(uri, ident_text, byte_offset, &doc.scope_tree, index);
        lsp_log!(
            "[hover] scope resolved '{}', type_info={:?}",
            ident_text,
            type_info
        );
        return build_hover_for_definition(&def, all_docs, type_info.as_deref());
    }

    // Check if ident is a type name (e.g. hovering on "Foo" in `---@type Foo`)
    if let Some(candidates) = index.type_shard.get(ident_text) {
        if let Some(candidate) = candidates.first() {
            if let Some(summary) = index.summary(candidate.source_uri()) {
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
                        if let Some(def_doc) = all_docs.get(candidate.source_uri()) {
                            let def_byte = Some(td.range.start_byte);
                            if let Some(db) = def_byte {
                                if let Some(def_node) = def_doc.tree.root_node()
                                    .descendant_for_byte_range(db, db)
                                {
                                    let stmt = find_enclosing_statement(def_node);
                                    let comment_lines = collect_preceding_comments(stmt, def_doc.source());
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
    // The candidate isn't an AST-local decl (scope_tree),
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
            uri: candidate.source_uri().clone(),
        }, candidates.len(), candidate.source_uri().clone()))
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
            let _ = write!(type_info, " ({} definitions)", entry_count);
        }
        if let Some(summary) = index.summary(&source_uri) {
            if let Some(fs) = summary.get_function_by_name(ident_text) {
                if !fs.overloads.is_empty() {
                    type_info.push_str("\n\nOverloads:");
                    for overload in &fs.overloads {
                        let _ = write!(type_info, "\n- `{}`", format_signature(overload));
                    }
                }
            }
        }
        return build_hover_for_definition(&synth_def, all_docs, Some(&type_info));
    }

    None
}

fn hover_type_name(
    name: &str,
    index: &WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let candidates = index.type_shard.get(name)?;
    let candidate = candidates.first()?;
    let summary = index.summary(candidate.source_uri())?;

    for td in &summary.type_definitions {
        if td.name != name {
            continue;
        }

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
        // Include doc comments from the definition site.
        if let Some(def_doc) = all_docs.get(candidate.source_uri()) {
            if let Some(def_node) = def_doc.tree.root_node()
                .descendant_for_byte_range(td.range.start_byte, td.range.start_byte)
            {
                let stmt = find_enclosing_statement(def_node);
                let comment_lines = collect_preceding_comments(stmt, def_doc.source());
                let doc_text = extract_doc_lines(&comment_lines);
                if !doc_text.is_empty() {
                    parts.push(doc_text);
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

    None
}

/// Returns true when `ident` is the tail identifier of `function_name`,
/// i.e. the part that actually names the function being defined:
///   `function foo()`         → tail = `foo`
///   `function a.b.c()`       → tail = `c`  (`a`, `b` are reads)
///   `function obj:method()`  → tail = `method` (`obj` is a read)
/// For the tail we want hover to show the function declaration; for
/// the base / intermediate identifiers we let the caller fall through
/// to ordinary variable resolution so `A1213:f()` with unknown
/// `A1213` surfaces as undefined instead of masquerading as a
/// function signature.
fn is_function_name_tail(
    function_name: tree_sitter::Node,
    ident: tree_sitter::Node,
) -> bool {
    // Grammar: function_name = identifier ( '.' identifier )* ( ':' identifier )?
    // The method form exposes the trailing identifier via the
    // `method` field.
    if let Some(method) = function_name.child_by_field_name("method") {
        return method.id() == ident.id();
    }
    // Dotted / bare form: the tail is the last `identifier` child.
    let mut last_ident: Option<tree_sitter::Node> = None;
    for i in 0..function_name.child_count() {
        if let Some(child) = function_name.child(i as u32) {
            if child.kind() == "identifier" {
                last_ident = Some(child);
            }
        }
    }
    last_ident.is_some_and(|n| n.id() == ident.id())
}

/// Build hover directly from a function/local declaration node at
/// the definition site.
///
/// **Invariant**: currently always returns `Some(Hover)` — the
/// caller in `hover()` relies on this so the `function_name` tail
/// branch of the ancestor walker short-circuits rather than falling
/// through to the scope / type_shard paths. If this function gains
/// a real failure path, update the walker's closure contract comment
/// too.
fn hover_at_declaration(
    decl_node: tree_sitter::Node,
    doc: &Document,
) -> Option<Hover> {
    let source = doc.source();

    let comment_lines = collect_preceding_comments(decl_node, source);
    let trailing = collect_trailing_comment(decl_node, source);
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

    if let Some(trail) = &trailing {
        parts.push(trail.clone());
    }

    let name_node = decl_node.child_by_field_name("name");
    let range = name_node.map(|n| doc.line_index().ts_node_to_range(n, source));

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range,
    })
}

/// AST-driven hover for a method call: `obj:method(...)`. The clicked
/// identifier is the `method` field of a `function_call` node. Infer
/// the type of the callee (the base object) and resolve the method
/// name as a field on that type, then build a hover popup.
fn hover_method_call(
    call_node: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let source = doc.source();
    let base_node = call_node.child_by_field_name("callee")?;
    let name_node = call_node.child_by_field_name("method")?;
    build_field_hover(base_node, name_node, "method", source, uri, &doc.scope_tree, index, all_docs, doc.line_index())
}

/// AST-driven hover for a dotted access: `var_node` is the enclosing
/// `variable` (or `field_expression`) whose `field` is the identifier
/// the user clicked. Handles arbitrary bases via `infer_node_type` which
/// recurses through nested variables / subscripts / call returns.
fn hover_variable_field(
    var_node: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let source = doc.source();
    if let Some((base_node, fields)) = extract_field_chain(var_node, source) {
        let name_node = var_node.child_by_field_name("field")?;
        return build_field_chain_hover(
            base_node,
            fields,
            name_node,
            "field",
            source,
            uri,
            &doc.scope_tree,
            index,
            all_docs,
            doc.line_index(),
        );
    }

    let base_node = var_node.child_by_field_name("object")?;
    let name_node = var_node.child_by_field_name("field")?;
    build_field_hover(base_node, name_node, "field", source, uri, &doc.scope_tree, index, all_docs, doc.line_index())
}

/// Shared hover builder for dotted field access (`a.b`) and method calls
/// (`obj:m()`). Both paths share the same resolve → type_display →
/// synth_def → build_hover_for_definition → fallback pipeline; only the
/// AST child names and the fallback label (`field` vs `method`) differ.
///
/// `kind_label` is `"field"` or `"method"` — used in the fallback hover
/// when no definition site is available.
fn build_field_hover(
    base_node: tree_sitter::Node,
    name_node: tree_sitter::Node,
    kind_label: &str,
    source: &[u8],
    uri: &Uri,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
    line_index: &LineIndex,
) -> Option<Hover> {
    let field_name = node_text(name_node, source).to_string();
    build_field_chain_hover(
        base_node,
        vec![field_name],
        name_node,
        kind_label,
        source,
        uri,
        scope_tree,
        index,
        all_docs,
        line_index,
    )
}

fn build_field_chain_hover(
    base_node: tree_sitter::Node,
    fields: Vec<String>,
    name_node: tree_sitter::Node,
    kind_label: &str,
    source: &[u8],
    uri: &Uri,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
    line_index: &LineIndex,
) -> Option<Hover> {
    let field_name = fields.last()?.clone();

    let base_fact = infer_node_type(base_node, source, uri, scope_tree, index);
    lsp_log!(
        "[hover_{kind}] base='{}' base_fact={:?} fields={:?}",
        node_text(base_node, source),
        base_fact,
        fields,
        kind = kind_label,
    );
    let resolved = resolver::resolve_field_chain_in_file(
        uri, &base_fact, &fields, index,
    );
    lsp_log!("[hover_{kind}] resolved={:?}", resolved.type_fact, kind = kind_label);

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
    parts.push(format!("```lua\n({}) {}\n```", kind_label, field_name));
    if type_display != "unknown" {
        parts.push(format!("Type: `{}`", type_display));
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(line_index.ts_node_to_range(name_node, source)),
    })
}

fn resolve_local_type_info(
    uri: &Uri,
    name: &str,
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<String> {
    // FunctionRef hover fix: resolve to readable signature via scope_tree
    if let Some(type_fact) = scope_tree.resolve_type(byte_offset, name) {
        if let TypeFact::Known(crate::type_system::KnownType::FunctionRef(id)) = type_fact {
            if let Some(summary) = index.summary(uri) {
                if let Some(fs) = summary.function_summaries.get(id) {
                    return Some(format_signature(&fs.signature));
                }
            }
        }
    }

    let resolved = resolver::resolve_local_in_file(uri, name, byte_offset, scope_tree, index);
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
    match fact {
        TypeFact::Known(crate::type_system::KnownType::Function(sig)) => {
            return format_signature(sig);
        }
        TypeFact::Known(crate::type_system::KnownType::FunctionRef(_)) => {
            // FunctionRef is an opaque ID; fall through to Display.
        }
        _ => {}
    }
    format!("{}", fact)
}

fn format_signature(sig: &crate::type_system::FunctionSignature) -> String {
    sig.display_label(None, false)
}

fn build_hover_for_definition(
    def: &crate::types::Definition,
    all_docs: &std::collections::HashMap<Uri, Document>,
    type_info: Option<&str>,
) -> Option<Hover> {
    let doc = all_docs.get(&def.uri)?;
    let source = doc.source();

    let def_start_byte = def.range.start_byte;
    let def_node = doc.tree.root_node().descendant_for_byte_range(def_start_byte, def_start_byte)?;

    if let Some(field_node) = find_enclosing_table_field(def_node) {
        return build_hover_for_table_field(def, field_node, source, type_info);
    }

    let stmt_node = find_enclosing_statement(def_node);

    let comment_lines = collect_preceding_comments(stmt_node, source);
    let trailing = collect_trailing_comment(stmt_node, source);
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

    if let Some(trail) = &trailing {
        parts.push(trail.clone());
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(def.selection_range.into()),
    })
}

fn build_hover_for_table_field(
    def: &crate::types::Definition,
    field_node: tree_sitter::Node,
    source: &[u8],
    type_info: Option<&str>,
) -> Option<Hover> {
    let comment_lines = collect_preceding_comments(field_node, source);
    let trailing = collect_table_field_trailing_comment(field_node, source);
    let comment_text = comment_lines.join("\n");
    let annotations = parse_emmy_comments(&comment_text);
    let emmy_md = format_annotations_markdown(&annotations);

    let def_line = node_text(field_node, source)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();

    let mut parts = Vec::new();
    parts.push(format!("```lua\n{}\n```", def_line));
    parts.push("*field*".to_string());

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

    if let Some(trail) = &trailing {
        parts.push(trail.clone());
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(def.selection_range.into()),
    })
}

fn collect_table_field_trailing_comment(
    field_node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let field_row = field_node.end_position().row;
    let mut next = field_node.next_sibling();
    while let Some(node) = next {
        if node.start_position().row != field_row {
            return None;
        }
        let text = node.utf8_text(source).unwrap_or("").trim();
        if text == "," || text == ";" {
            next = node.next_sibling();
            continue;
        }
        if text.starts_with("---") {
            return None;
        }
        if let Some(rest) = text.strip_prefix("--") {
            let trimmed = rest.trim();
            return (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
        return None;
    }
    None
}

/// Extract plain documentation text from collected comment lines.
/// Strips `---` or `--` prefix, excludes `@`-prefixed annotation lines
/// and `#`-prefixed directive lines (e.g. `---#disable top_keyword`).
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
            if stripped.starts_with('@') || stripped.starts_with('#') || stripped.is_empty() {
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

fn find_enclosing_table_field(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node;
    loop {
        match current.kind() {
            "field" => return Some(current),
            "function_declaration" | "local_function_declaration" | "local_declaration"
            | "assignment_statement" | "function_call_statement" => return None,
            _ => {
                if let Some(parent) = current.parent() {
                    current = parent;
                } else {
                    return None;
                }
            }
        }
    }
}
