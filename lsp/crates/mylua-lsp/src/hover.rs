use crate::aggregation::WorkspaceAggregation;
use crate::document::{Document, DocumentLookup};
use crate::emmy::{
    collect_preceding_comments, collect_trailing_comment, collect_trailing_emmy_text,
    is_comment_separator_line, parse_emmy_comments, EmmyAnnotation,
};
use crate::resolver;
use crate::syntax_kind::NodeKindExt;
use crate::type_system::{
    format_resolved_type, format_signature, FunctionSignature, KnownType, TypeFact,
};
use crate::types::DefKind;
use crate::uri_id::{resolve_uri, UriId};
use crate::util::{
    extract_field_chain, find_node_at_position, node_text, percent_decode, walk_ancestors,
    LineIndex,
};
use std::fmt::Write;
use tower_lsp_server::ls_types::*;

pub fn hover(
    doc: &Document,
    uri_id: UriId,
    position: Position,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
) -> Option<Hover> {
    let byte_offset = doc
        .line_index()
        .position_to_byte_offset(doc.source(), position)?;
    if let Some(type_name) = crate::emmy::emmy_type_name_at_byte(doc.source(), byte_offset) {
        return hover_type_name(&type_name, index, all_docs);
    }

    let ident_node = find_node_at_position(doc.root_node()?, byte_offset)?;
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
        ident_node.kind_name(),
        ident_node.parent().map_or("none", |p| p.kind_name()),
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
        if matches!(p.kind_name(), "variable" | "field_expression") {
            let field_is_ident = p
                .child_by_field_name("field")
                .map(|f| f.id() == ident_node.id())
                .unwrap_or(false);
            if field_is_ident {
                return Some(hover_variable_field(p, doc, uri_id, index, all_docs));
            }
        }
        if p.kind_name() == "field" {
            let key_is_ident = p
                .child_by_field_name("key")
                .map(|k| k.id() == ident_node.id())
                .unwrap_or(false);
            if key_is_ident {
                return Some(hover_table_constructor_field(
                    p, doc, uri_id, index, all_docs,
                ));
            }
        }
        // `obj:method(...)` — the `method` identifier on a `function_call`
        // node. Infer the base type and resolve the method as a field so
        // hover shows the method's declaration + type info.
        if p.kind_name() == "function_call" {
            let method_is_ident = p
                .child_by_field_name("method")
                .map(|m| m.id() == ident_node.id())
                .unwrap_or(false);
            if method_is_ident {
                return Some(hover_method_call(p, doc, uri_id, index, all_docs));
            }
        }
        if p.kind_name() == "function_name" {
            if let Some(decl) = p.parent() {
                if decl.kind_name() == "function_declaration"
                    || decl.kind_name() == "local_function_declaration"
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

    if let Some(def) = doc.scope_tree.resolve_id(byte_offset, ident_text, uri_id) {
        let type_info =
            resolve_local_type_info(uri_id, ident_text, byte_offset, &doc.scope_tree, index);
        lsp_log!(
            "[hover] scope resolved '{}', type_info={:?}",
            ident_text,
            type_info
        );
        let hover_range = doc.line_index().ts_node_to_range(ident_node, doc.source());
        return build_hover_for_definition(&def, all_docs, type_info.as_deref(), Some(hover_range));
    }

    // Check if ident is a type name (e.g. hovering on "Foo" in `---@type Foo`).
    if let Some(hover) = hover_type_name(ident_text, index, all_docs) {
        return Some(hover);
    }

    // Synthesize a `Definition` from the best global candidate so the
    // hover renderer below can reuse `build_hover_for_definition`.
    // The candidate isn't an AST-local decl (scope_tree),
    // so we fabricate one carrying the candidate's source location +
    // global kind — purely for the shared formatter.
    let global_info = index.global_shard.get(ident_text).and_then(|candidates| {
        let candidate = candidates.first()?;
        let source_uri = resolve_uri(candidate.source_uri_id());
        let def_kind = match candidate.kind {
            crate::summary::GlobalContributionKind::Function => {
                crate::types::DefKind::GlobalFunction
            }
            _ => crate::types::DefKind::GlobalVariable,
        };
        Some((
            crate::types::Definition {
                name: candidate.name.to_string(),
                kind: def_kind,
                range: candidate.range,
                selection_range: candidate.selection_range,
                uri_id: candidate.source_uri_id(),
                uri: source_uri.clone(),
            },
            candidates.len(),
            source_uri,
            candidate.source_uri_id(),
        ))
    });
    if let Some((synth_def, entry_count, _source_uri, source_uri_id)) = global_info {
        let resolved = resolver::resolve_type(
            source_uri_id,
            &TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
                name: ident_text.into(),
            }),
            index,
        );
        let mut type_info = format_resolved_type(&resolved.type_fact);
        if entry_count > 1 {
            let _ = write!(type_info, " ({} definitions)", entry_count);
        }
        if let Some(summary) = index.summary_by_id(source_uri_id) {
            if let Some(fs) = summary.get_function_by_name(ident_text) {
                if !fs.overloads.is_empty() {
                    type_info.push_str("\n\nOverloads:");
                    for overload in &fs.overloads {
                        let _ = write!(type_info, "\n- `{}`", format_signature(overload));
                    }
                }
            }
        }
        let hover_range = doc.line_index().ts_node_to_range(ident_node, doc.source());
        return build_hover_for_definition(
            &synth_def,
            all_docs,
            Some(&type_info),
            Some(hover_range),
        );
    }

    None
}

fn hover_type_name(
    name: &str,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
) -> Option<Hover> {
    let candidates = index.type_candidates(name)?;
    let candidate = candidates.first()?;
    index.summary_by_id(candidate.source_uri_id())?;
    let summary = index.summary_by_id(candidate.source_uri_id())?;

    for td in &summary.type_definitions {
        if td.name != name {
            continue;
        }

        return build_hover_for_type_definition(td, summary, candidate.source_uri_id(), all_docs);
    }

    None
}

fn build_hover_for_type_definition(
    td: &crate::summary::TypeDefinition,
    summary: &crate::summary::DocumentSummary,
    source_uri_id: UriId,
    all_docs: &impl DocumentLookup,
) -> Option<Hover> {
    let mut code_lines = Vec::new();
    match td.kind {
        crate::summary::TypeDefinitionKind::Alias => {
            let alias_display = td
                .alias_type
                .as_ref()
                .map(|t| format_type_fact_for_hover(t, summary))
                .unwrap_or_else(|| "unknown".to_string());
            code_lines.push(format!("---@alias {} {}", td.name, alias_display));
        }
        crate::summary::TypeDefinitionKind::Enum => {
            code_lines.push(format!("---@enum {}", td.name));
        }
        crate::summary::TypeDefinitionKind::Class => {
            if td.parents.is_empty() {
                code_lines.push(format!("---@class {}", td.name));
            } else {
                let parents = td
                    .parents
                    .iter()
                    .map(|parent| parent.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                code_lines.push(format!("---@class {} : {}", td.name, parents));
            }
        }
    }
    for field in &td.fields {
        code_lines.push(format!(
            "---@field {} {}",
            field.name,
            format_type_fact_for_class_field_hover(&field.type_fact, summary, td.name.as_str())
        ));
    }

    let mut parts = vec![lua_code_block(&code_lines)];
    let kind_label = match td.kind {
        crate::summary::TypeDefinitionKind::Class => "class",
        crate::summary::TypeDefinitionKind::Alias => "alias",
        crate::summary::TypeDefinitionKind::Enum => "enum",
    };
    parts.push(format!("*{}*", kind_label));

    if let Some(def_doc) = all_docs.get_document_by_id(source_uri_id) {
        let doc_text = definition_doc_text_at_byte(def_doc, td.range.start_byte);
        if !doc_text.is_empty() {
            parts.push(doc_text);
        }
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: None,
    })
}

fn format_type_fact_for_hover(
    fact: &TypeFact,
    summary: &crate::summary::DocumentSummary,
) -> String {
    format_type_fact_for_hover_inner(fact, summary, 0, None)
}

fn format_type_fact_for_class_field_hover(
    fact: &TypeFact,
    summary: &crate::summary::DocumentSummary,
    class_name: &str,
) -> String {
    format_type_fact_for_hover_inner(fact, summary, 0, Some(class_name))
}

fn format_type_fact_for_hover_inner(
    fact: &TypeFact,
    summary: &crate::summary::DocumentSummary,
    depth: usize,
    method_self_class: Option<&str>,
) -> String {
    if depth > 8 {
        return format!("{}", fact);
    }
    match fact {
        TypeFact::Known(kind) => {
            format_known_type_for_hover(kind, summary, depth + 1, method_self_class)
        }
        TypeFact::Union(types) => types
            .iter()
            .map(|ty| format_type_fact_for_hover_inner(ty, summary, depth + 1, method_self_class))
            .collect::<Vec<_>>()
            .join(" | "),
        TypeFact::Stub(_) | TypeFact::Unknown => format!("{}", fact),
    }
}

fn format_known_type_for_hover(
    kind: &KnownType,
    summary: &crate::summary::DocumentSummary,
    depth: usize,
    method_self_class: Option<&str>,
) -> String {
    match kind {
        KnownType::Nil => "nil".to_string(),
        KnownType::Boolean => "boolean".to_string(),
        KnownType::Number => "number".to_string(),
        KnownType::Integer => "integer".to_string(),
        KnownType::String => "string".to_string(),
        KnownType::Table(id) => format!("table<{}>", id.0),
        KnownType::Function(sig) => format_signature_for_hover(sig, summary, depth + 1, None),
        KnownType::FunctionRef(id) => summary
            .function_summaries
            .get(id)
            .map(|fs| format_function_summary_for_hover(fs, summary, depth + 1, method_self_class))
            .unwrap_or_else(|| format!("function<{}>", id)),
        KnownType::EmmyType(name) => name.to_string(),
        KnownType::EmmyGeneric(name, params) => {
            if name.as_str() == "__array" && params.len() == 1 {
                format!(
                    "{}[]",
                    format_type_fact_for_hover_inner(
                        &params[0],
                        summary,
                        depth + 1,
                        method_self_class
                    )
                )
            } else {
                format!(
                    "{}<{}>",
                    name,
                    params
                        .iter()
                        .map(|param| {
                            format_type_fact_for_hover_inner(
                                param,
                                summary,
                                depth + 1,
                                method_self_class,
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}

fn format_function_summary_for_hover(
    fs: &crate::summary::FunctionSummary,
    summary: &crate::summary::DocumentSummary,
    depth: usize,
    method_self_class: Option<&str>,
) -> String {
    let name = fs.name.as_str();
    let class_name = if name.contains(':') {
        method_self_class.unwrap_or_else(|| crate::type_system::class_prefix_of(name))
    } else {
        ""
    };
    let has_self_param = fs
        .signature
        .params
        .first()
        .is_some_and(|param| param.name.as_str() == "self");
    let synthetic_self_class = (!class_name.is_empty() && !has_self_param).then_some(class_name);
    format_signature_for_hover(&fs.signature, summary, depth + 1, synthetic_self_class)
}

fn format_signature_for_hover(
    sig: &FunctionSignature,
    summary: &crate::summary::DocumentSummary,
    depth: usize,
    synthetic_self_class: Option<&str>,
) -> String {
    let mut label = String::from("fun(");
    let mut first = true;
    if let Some(class_name) = synthetic_self_class {
        let _ = write!(label, "self: {}", class_name);
        first = false;
    }
    for param in &sig.params {
        if !first {
            label.push_str(", ");
        }
        first = false;
        let param_name = if param.optional && param.name.as_str() != "..." {
            format!("{}?", param.name)
        } else {
            param.name.to_string()
        };
        if param.type_fact == TypeFact::Unknown {
            label.push_str(&param_name);
        } else {
            let _ = write!(
                label,
                "{}: {}",
                param_name,
                format_type_fact_for_hover_inner(&param.type_fact, summary, depth + 1, None)
            );
        }
    }
    label.push(')');
    if !sig.returns.is_empty() {
        label.push_str(": ");
        label.push_str(
            &sig.returns
                .iter()
                .map(|ret| format_type_fact_for_hover_inner(ret, summary, depth + 1, None))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    label
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
fn is_function_name_tail(function_name: tree_sitter::Node, ident: tree_sitter::Node) -> bool {
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
            if child.kind_name() == "identifier" {
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
fn hover_at_declaration(decl_node: tree_sitter::Node, doc: &Document) -> Option<Hover> {
    let source = doc.source();

    let comment_lines = collect_preceding_comments(decl_node, source);
    let trailing = collect_trailing_comment(decl_node, source);
    let comment_text = comment_lines.join("\n");
    let mut annotations = parse_emmy_comments(&comment_text);
    if let Some(trailing_emmy) = collect_trailing_emmy_text(decl_node, source) {
        annotations.extend(parse_emmy_comments(&trailing_emmy));
    }

    let def_line = node_text(decl_node, source)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();

    let kind_label = match decl_node.kind_name() {
        "local_function_declaration" => "local function",
        _ => "function",
    };

    let mut parts = Vec::new();
    parts.push(lua_code_block(&code_lines_for_definition(
        &annotations,
        def_line,
        None,
    )));
    parts.push(format!("*{}*", kind_label));

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
    uri_id: UriId,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
) -> Option<Hover> {
    let source = doc.source();
    let base_node = call_node.child_by_field_name("callee")?;
    let name_node = call_node.child_by_field_name("method")?;
    if let Some((root_node, mut fields)) = extract_field_chain(base_node, source) {
        fields.push(node_text(name_node, source).to_string());
        return build_field_chain_hover(
            root_node,
            fields,
            name_node,
            "method",
            source,
            uri_id,
            &doc.scope_tree,
            index,
            all_docs,
            doc.line_index(),
        );
    }
    build_field_hover(
        base_node,
        name_node,
        "method",
        source,
        uri_id,
        &doc.scope_tree,
        index,
        all_docs,
        doc.line_index(),
    )
}

/// AST-driven hover for a dotted access: `var_node` is the enclosing
/// `variable` (or `field_expression`) whose `field` is the identifier
/// the user clicked. Handles arbitrary bases via `infer_node_type` which
/// recurses through nested variables / subscripts / call returns.
fn hover_variable_field(
    var_node: tree_sitter::Node,
    doc: &Document,
    uri_id: UriId,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
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
            uri_id,
            &doc.scope_tree,
            index,
            all_docs,
            doc.line_index(),
        );
    }

    let base_node = var_node.child_by_field_name("object")?;
    let name_node = var_node.child_by_field_name("field")?;
    build_field_hover(
        base_node,
        name_node,
        "field",
        source,
        uri_id,
        &doc.scope_tree,
        index,
        all_docs,
        doc.line_index(),
    )
}

fn hover_table_constructor_field(
    field_node: tree_sitter::Node,
    doc: &Document,
    uri_id: UriId,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
) -> Option<Hover> {
    let source = doc.source();
    let key_node = field_node.child_by_field_name("key")?;
    if key_node.kind_name() != "identifier" || key_node.start_byte() != field_node.start_byte() {
        return None;
    }

    let field_name = node_text(key_node, source).to_string();
    let def_range = doc.line_index().ts_node_to_byte_range(field_node, source);
    let selection_range = doc.line_index().ts_node_to_byte_range(key_node, source);
    let hover_range = doc.line_index().ts_node_to_range(key_node, source);
    let type_info = table_constructor_field_type_info(
        field_node,
        &field_name,
        def_range,
        source,
        uri_id,
        &doc.scope_tree,
        index,
    );

    let synth_def = crate::types::Definition {
        name: field_name,
        kind: DefKind::GlobalVariable,
        range: def_range,
        selection_range,
        uri_id,
        uri: resolve_uri(uri_id),
    };

    build_hover_for_definition(
        &synth_def,
        all_docs,
        type_info.as_deref(),
        Some(hover_range),
    )
}

fn table_constructor_field_type_info(
    field_node: tree_sitter::Node,
    field_name: &str,
    def_range: crate::util::ByteRange,
    source: &[u8],
    uri_id: UriId,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<String> {
    let indexed_type = index.summary_by_id(uri_id).and_then(|summary| {
        summary.table_shapes.values().find_map(|shape| {
            let info = shape.get_field(field_name)?;
            (info.def_range == Some(def_range)).then(|| info.type_fact.clone())
        })
    });

    let type_fact = indexed_type.or_else(|| {
        field_node.child_by_field_name("value").map(|value| {
            crate::type_inference::infer_node_type_in_file_id(
                value, source, uri_id, scope_tree, index,
            )
        })
    })?;
    let resolved = resolver::resolve_type(uri_id, &type_fact, index);
    let display = format_resolved_type(&resolved.type_fact);
    (display != "unknown").then_some(display)
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
    uri_id: UriId,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
    line_index: &LineIndex,
) -> Option<Hover> {
    let field_name = node_text(name_node, source).to_string();
    build_field_chain_hover(
        base_node,
        vec![field_name],
        name_node,
        kind_label,
        source,
        uri_id,
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
    uri_id: UriId,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
    line_index: &LineIndex,
) -> Option<Hover> {
    let field_name = fields.last()?.clone();

    let base_fact = crate::type_inference::infer_node_type_in_file_id(
        base_node, source, uri_id, scope_tree, index,
    );
    lsp_log!(
        "[hover_{kind}] base='{}' base_fact={:?} fields={:?}",
        node_text(base_node, source),
        base_fact,
        fields,
        kind = kind_label,
    );
    let resolved = resolver::resolve_field_chain_in_file_id(uri_id, &base_fact, &fields, index);
    lsp_log!(
        "[hover_{kind}] resolved={:?}",
        resolved.type_fact,
        kind = kind_label
    );

    let type_display = format_resolved_type(&resolved.type_fact);

    let synth_def = (|| {
        let Some(location) = resolved.def_location else {
            return None;
        };
        let def_range = location.range;
        if !all_docs.contains_document_id(location.uri_id) {
            return None;
        }
        Some(crate::types::Definition {
            name: field_name.clone(),
            kind: DefKind::GlobalVariable,
            range: def_range,
            selection_range: def_range,
            uri_id: location.uri_id,
            uri: crate::uri_id::resolve_uri(location.uri_id),
        })
    })();
    if let Some(synth_def) = synth_def {
        let hover_range = line_index.ts_node_to_range(name_node, source);
        return build_hover_for_definition(
            &synth_def,
            all_docs,
            Some(&type_display),
            Some(hover_range),
        );
    }

    fallback_field_hover(
        kind_label,
        &field_name,
        &type_display,
        name_node,
        source,
        line_index,
    )
}

fn fallback_field_hover(
    kind_label: &str,
    field_name: &str,
    type_display: &str,
    name_node: tree_sitter::Node,
    source: &[u8],
    line_index: &LineIndex,
) -> Option<Hover> {
    let mut code_lines = Vec::new();
    if type_display != "unknown" {
        code_lines.push(format!("---@type {}", type_display));
    }
    code_lines.push(field_name.to_string());

    let mut parts = Vec::new();
    parts.push(lua_code_block(&code_lines));
    parts.push(format!("*{}*", kind_label));

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(line_index.ts_node_to_range(name_node, source)),
    })
}

fn resolve_local_type_info(
    uri_id: UriId,
    name: &str,
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<String> {
    // FunctionRef hover fix: resolve to readable signature via scope_tree
    if let Some(type_fact) = scope_tree.resolve_type(byte_offset, name) {
        if let TypeFact::Known(crate::type_system::KnownType::FunctionRef(id)) = type_fact {
            if let Some(summary) = index.summary_by_id(uri_id) {
                if let Some(fs) = summary.function_summaries.get(id) {
                    return Some(format_signature(&fs.signature));
                }
            }
        }
    }

    let resolved = resolver::resolve_local_in_file(uri_id, name, byte_offset, scope_tree, index);
    let display = format_resolved_type(&resolved.type_fact);
    if display == "unknown" {
        None
    } else {
        Some(display)
    }
}

fn lua_code_block(lines: &[String]) -> String {
    format!("```lua\n{}\n```", lines.join("\n"))
}

fn simple_type_info(type_info: Option<&str>) -> Option<&str> {
    type_info.filter(|ti| *ti != "unknown" && !ti.contains('\n'))
}

fn kind_label_with_origin(kind_label: &str, def: &crate::types::Definition) -> String {
    match definition_origin_link(def) {
        Some(origin) => format!("*{}* · {}", kind_label, origin),
        None => format!("*{}*", kind_label),
    }
}

fn definition_origin_link(def: &crate::types::Definition) -> Option<String> {
    let uri = def.uri.as_str();
    let raw_filename = uri.rsplit('/').next()?.split('?').next().unwrap_or("");
    if raw_filename.is_empty() {
        return None;
    }

    let filename = escape_markdown_link_text(&percent_decode(raw_filename));
    let target = definition_link_target(def);
    Some(format!("[{}]({})", filename, target))
}

fn definition_link_target(def: &crate::types::Definition) -> String {
    escape_markdown_link_target(&format!(
        "{}#L{}",
        def.uri.as_str(),
        def.selection_range.start_row + 1
    ))
}

fn escape_markdown_link_target(target: &str) -> String {
    target.replace('(', "%28").replace(')', "%29")
}

fn escape_markdown_link_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '\\' | '[' | ']' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn emmy_desc_suffix(desc: &str) -> String {
    if desc.is_empty() {
        String::new()
    } else {
        format!(" @ {}", desc)
    }
}

fn format_annotations_lua(annotations: &[EmmyAnnotation]) -> Vec<String> {
    let mut lines = Vec::new();
    for ann in annotations {
        match ann {
            EmmyAnnotation::Param {
                name,
                type_expr,
                optional,
                desc,
            } => {
                let opt = if *optional { "?" } else { "" };
                let suffix = emmy_desc_suffix(desc);
                lines.push(format!("---@param {}{} {}{}", name, opt, type_expr, suffix));
            }
            EmmyAnnotation::Return {
                return_types,
                name,
                desc,
            } => {
                let types_str = return_types
                    .iter()
                    .map(|t| format!("{}", t))
                    .collect::<Vec<_>>()
                    .join(", ");
                let suffix = name.as_ref().map(|n| format!(" {}", n)).unwrap_or_default();
                let desc_suffix = emmy_desc_suffix(desc);
                lines.push(format!("---@return {}{}{}", types_str, suffix, desc_suffix));
            }
            EmmyAnnotation::Type { type_expr, desc } => {
                let suffix = emmy_desc_suffix(desc);
                lines.push(format!("---@type {}{}", type_expr, suffix));
            }
            EmmyAnnotation::Class {
                name,
                parents,
                generic_params,
                desc,
            } => {
                let generic_suffix = if generic_params.is_empty() {
                    String::new()
                } else {
                    format!(
                        "<{}>",
                        generic_params
                            .iter()
                            .map(|param| match &param.constraint {
                                Some(constraint) => format!("{} : {}", param.name, constraint),
                                None => param.name.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let suffix = emmy_desc_suffix(desc);
                if parents.is_empty() {
                    lines.push(format!("---@class {}{}{}", name, generic_suffix, suffix));
                } else {
                    lines.push(format!(
                        "---@class {}{} : {}{}",
                        name,
                        generic_suffix,
                        parents.join(", "),
                        suffix
                    ));
                }
            }
            EmmyAnnotation::Field {
                visibility,
                name,
                type_expr,
                is_method,
                desc,
            } => {
                let visibility = visibility
                    .as_ref()
                    .map(|v| format!("{} ", v))
                    .unwrap_or_default();
                let type_prefix = if *is_method { ":" } else { "" };
                let suffix = emmy_desc_suffix(desc);
                lines.push(format!(
                    "---@field {}{} {}{}{}",
                    visibility, name, type_prefix, type_expr, suffix
                ));
            }
            EmmyAnnotation::Alias { name, type_expr } => {
                lines.push(format!("---@alias {} {}", name, type_expr));
            }
            EmmyAnnotation::Generic { params } => {
                let params = params
                    .iter()
                    .map(|p| {
                        if let Some(constraint) = &p.constraint {
                            format!("{}: {}", p.name, constraint)
                        } else {
                            p.name.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!("---@generic {}", params));
            }
            EmmyAnnotation::Overload { fun_type } => {
                lines.push(format!("---@overload {}", fun_type));
            }
            EmmyAnnotation::Vararg { type_expr } => {
                lines.push(format!("---@vararg {}", type_expr));
            }
            EmmyAnnotation::Deprecated { desc } => {
                if desc.is_empty() {
                    lines.push("---@deprecated".to_string());
                } else {
                    lines.push(format!("---@deprecated {}", desc));
                }
            }
            _ => {}
        }
    }
    lines
}

fn annotations_have_type_info(annotations: &[EmmyAnnotation]) -> bool {
    annotations.iter().any(|ann| {
        matches!(
            ann,
            EmmyAnnotation::Param { .. }
                | EmmyAnnotation::Return { .. }
                | EmmyAnnotation::Type { .. }
                | EmmyAnnotation::Class { .. }
                | EmmyAnnotation::Field { .. }
                | EmmyAnnotation::Alias { .. }
                | EmmyAnnotation::Overload { .. }
                | EmmyAnnotation::Vararg { .. }
        )
    })
}

fn code_lines_for_definition(
    annotations: &[EmmyAnnotation],
    def_line: String,
    type_info: Option<&str>,
) -> Vec<String> {
    let mut code_lines = format_annotations_lua(annotations);
    if !annotations_have_type_info(annotations) {
        if let Some(ti) = simple_type_info(type_info) {
            code_lines.push(format!("---@type {}", ti));
        }
    }
    code_lines.push(def_line);
    code_lines
}

fn code_lines_for_field_definition(
    annotations: &[EmmyAnnotation],
    def_line: String,
    type_info: Option<&str>,
) -> Vec<String> {
    let Some(ti) = type_info.filter(|ti| *ti != "unknown") else {
        return code_lines_for_definition(annotations, def_line, type_info);
    };

    let mut code_lines = Vec::new();
    for ann in annotations {
        if let EmmyAnnotation::Field {
            visibility,
            name,
            is_method,
            desc,
            ..
        } = ann
        {
            let visibility = visibility
                .as_ref()
                .map(|v| format!("{} ", v))
                .unwrap_or_default();
            let type_prefix = if *is_method && ti.starts_with("fun(") {
                ":"
            } else {
                ""
            };
            let suffix = emmy_desc_suffix(desc);
            code_lines.push(format!(
                "---@field {}{} {}{}{}",
                visibility, name, type_prefix, ti, suffix
            ));
        }
    }

    if code_lines.is_empty() {
        return code_lines_for_definition(annotations, def_line, type_info);
    }
    code_lines.push(def_line);
    code_lines
}

fn build_hover_for_definition(
    def: &crate::types::Definition,
    all_docs: &impl DocumentLookup,
    type_info: Option<&str>,
    hover_range: Option<Range>,
) -> Option<Hover> {
    let doc = all_docs.get_document_by_id(def.uri_id)?;
    let source = doc.source();

    let def_start_byte = def.range.start_byte;
    let temporary_tree;
    let root = if let Some(root) = doc.root_node() {
        root
    } else {
        temporary_tree = doc.parse_tree();
        temporary_tree.as_ref()?.root_node()
    };
    let def_node = root.descendant_for_byte_range(def_start_byte, def_start_byte)?;

    if let Some(emmy_line) = find_enclosing_emmy_line(def_node) {
        if let Some(hover) =
            build_hover_for_emmy_field(def, emmy_line, source, type_info, hover_range)
        {
            return Some(hover);
        }
    }

    if let Some(field_node) = find_enclosing_table_field(def_node) {
        return build_hover_for_table_field(def, field_node, source, type_info, hover_range);
    }

    let stmt_node = find_enclosing_statement(def_node);

    if def.kind == DefKind::Parameter {
        return build_hover_for_parameter(def, stmt_node, source, type_info, hover_range);
    }

    let comment_lines = collect_preceding_comments(stmt_node, source);

    let trailing = collect_trailing_comment(stmt_node, source);
    let comment_text = comment_lines.join("\n");
    let mut annotations = parse_emmy_comments(&comment_text);
    if let Some(trailing_emmy) = collect_trailing_emmy_text(stmt_node, source) {
        annotations.extend(parse_emmy_comments(&trailing_emmy));
    }

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
    parts.push(lua_code_block(&code_lines_for_definition(
        &annotations,
        def_line,
        type_info,
    )));
    parts.push(kind_label_with_origin(kind_label, def));

    if let Some(ti) = type_info {
        if simple_type_info(Some(ti)).is_none() && ti != "unknown" {
            parts.push(format!("Type: `{}`", ti));
        }
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
        range: Some(hover_range.unwrap_or_else(|| def.selection_range.into())),
    })
}

fn build_hover_for_emmy_field(
    def: &crate::types::Definition,
    emmy_line: tree_sitter::Node,
    source: &[u8],
    type_info: Option<&str>,
    hover_range: Option<Range>,
) -> Option<Hover> {
    let line_text = node_text(emmy_line, source).to_string();
    let field_annotations: Vec<_> = parse_emmy_comments(&line_text)
        .into_iter()
        .filter(|ann| matches!(ann, EmmyAnnotation::Field { name, .. } if name == &def.name))
        .collect();
    if field_annotations.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    parts.push(lua_code_block(&code_lines_for_field_definition(
        &field_annotations,
        def.name.to_string(),
        type_info,
    )));
    parts.push(kind_label_with_origin("field", def));

    if let Some(ti) = type_info {
        if simple_type_info(Some(ti)).is_none() && ti != "unknown" {
            parts.push(format!("Type: `{}`", ti));
        }
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(hover_range.unwrap_or_else(|| def.selection_range.into())),
    })
}

fn build_hover_for_parameter(
    def: &crate::types::Definition,
    stmt_node: tree_sitter::Node,
    source: &[u8],
    type_info: Option<&str>,
    hover_range: Option<Range>,
) -> Option<Hover> {
    let comment_lines = collect_preceding_comments(stmt_node, source);
    let comment_text = comment_lines.join("\n");
    let mut annotations = parse_emmy_comments(&comment_text);
    if let Some(trailing_emmy) = collect_trailing_emmy_text(stmt_node, source) {
        annotations.extend(parse_emmy_comments(&trailing_emmy));
    }
    let parameter_annotations: Vec<_> = annotations
        .into_iter()
        .filter(|ann| {
            matches!(
                ann,
                crate::emmy::EmmyAnnotation::Param { name, .. } if name == &def.name
            )
        })
        .collect();

    let mut parts = Vec::new();
    parts.push(lua_code_block(&code_lines_for_definition(
        &parameter_annotations,
        def.name.to_string(),
        type_info,
    )));
    parts.push(kind_label_with_origin("parameter", def));

    if let Some(ti) = type_info {
        if simple_type_info(Some(ti)).is_none() && ti != "unknown" {
            parts.push(format!("Type: `{}`", ti));
        }
    }

    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: parts.join("\n\n"),
        }),
        range: Some(hover_range.unwrap_or_else(|| def.selection_range.into())),
    })
}

fn definition_doc_text_at_byte(doc: &Document, byte: usize) -> String {
    let temporary_tree;
    let root = if let Some(root) = doc.root_node() {
        root
    } else {
        temporary_tree = doc.parse_tree();
        let Some(tree) = temporary_tree.as_ref() else {
            return String::new();
        };
        tree.root_node()
    };
    let Some(def_node) = root.descendant_for_byte_range(byte, byte) else {
        return String::new();
    };
    let stmt = find_enclosing_statement(def_node);
    let comment_lines = collect_preceding_comments(stmt, doc.source());
    extract_doc_lines(&comment_lines)
}

fn build_hover_for_table_field(
    def: &crate::types::Definition,
    field_node: tree_sitter::Node,
    source: &[u8],
    type_info: Option<&str>,
    hover_range: Option<Range>,
) -> Option<Hover> {
    let comment_lines = collect_table_field_preceding_comments(field_node, source);
    let trailing = collect_table_field_trailing_comment(field_node, source);
    let comment_text = comment_lines.join("\n");
    let mut annotations = parse_emmy_comments(&comment_text);
    if let Some(trailing_emmy) = collect_table_field_trailing_emmy_text(field_node, source) {
        annotations.extend(parse_emmy_comments(&trailing_emmy));
    }

    let def_line = node_text(field_node, source)
        .lines()
        .next()
        .unwrap_or("")
        .to_string();

    let mut parts = Vec::new();
    parts.push(lua_code_block(&code_lines_for_definition(
        &annotations,
        def_line,
        type_info,
    )));
    parts.push(kind_label_with_origin("field", def));

    if let Some(ti) = type_info {
        if simple_type_info(Some(ti)).is_none() && ti != "unknown" {
            parts.push(format!("Type: `{}`", ti));
        }
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
        range: Some(hover_range.unwrap_or_else(|| def.selection_range.into())),
    })
}

fn collect_table_field_trailing_comment(
    field_node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let trailing = trailing_table_field_segment(field_node, source)?;
    let text = strip_leading_table_field_separator(trailing);
    if text.starts_with("---") {
        return None;
    }
    let rest = text.strip_prefix("--")?;
    let trimmed = rest.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn collect_table_field_preceding_comments(
    field_node: tree_sitter::Node,
    source: &[u8],
) -> Vec<String> {
    let current_line_start = source[..field_node.start_byte()]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    if !source[current_line_start..field_node.start_byte()]
        .iter()
        .all(|b| b.is_ascii_whitespace())
    {
        return Vec::new();
    }

    let mut comments = Vec::new();
    let mut line_end = current_line_start.saturating_sub(1);

    while line_end > 0 {
        let line_start = source[..line_end]
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|idx| idx + 1)
            .unwrap_or(0);
        let Ok(line) = std::str::from_utf8(&source[line_start..line_end]) else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || is_comment_separator_line(trimmed) {
            break;
        }
        if !trimmed.starts_with("--") {
            break;
        }
        comments.push(trimmed.to_string());
        line_end = line_start.saturating_sub(1);
    }

    comments.reverse();
    comments
}

fn collect_table_field_trailing_emmy_text(
    field_node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let trailing = trailing_table_field_segment(field_node, source)?;
    let text = strip_leading_table_field_separator(trailing);
    text.starts_with("---@").then(|| text.to_string())
}

fn trailing_table_field_segment<'a>(
    field_node: tree_sitter::Node,
    source: &'a [u8],
) -> Option<&'a str> {
    let field_row = field_node.end_position().row;
    let line_end = source[field_node.end_byte()..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|offset| field_node.end_byte() + offset)
        .unwrap_or(source.len());
    let mut segment_end = line_end;
    let mut next = field_node.next_sibling();
    while let Some(node) = next {
        if node.start_position().row != field_row {
            break;
        }
        if node.kind_name() == "field" {
            segment_end = segment_end.min(node.start_byte());
            break;
        }
        next = node.next_sibling();
    }
    std::str::from_utf8(&source[field_node.end_byte()..segment_end]).ok()
}

fn strip_leading_table_field_separator(text: &str) -> &str {
    let text = text.trim_start();
    let text = text
        .strip_prefix(',')
        .or_else(|| text.strip_prefix(';'))
        .unwrap_or(text);
    text.trim_start()
}

/// Format plain documentation text from collected comment lines.
/// Strips `---` or `--` prefix, excludes `@`-prefixed annotation lines
/// and `#`-prefixed directive lines (e.g. `---#disable top_keyword`).
///
/// Empty comment lines are preserved as Markdown paragraph breaks, and
/// adjacent non-empty comment lines use Markdown hard breaks so hover keeps
/// the source comment layout instead of rendering everything as one paragraph.
/// Leading indentation after the comment marker is rendered with `&nbsp;`
/// because Markdown collapses ordinary leading spaces.
fn extract_doc_lines(comment_lines: &[String]) -> String {
    let mut out = String::new();
    let mut saw_text = false;
    let mut pending_blank = false;

    for line in comment_lines {
        let Some(content) = strip_doc_comment_prefix_preserving_indent(line) else {
            continue;
        };
        let content = trim_one_comment_padding_space(content).trim_end();
        let logical = content.trim_start();
        if logical.starts_with('@') || logical.starts_with('#') {
            continue;
        }
        if logical.is_empty() {
            if saw_text {
                pending_blank = true;
            }
            continue;
        }

        if saw_text {
            if pending_blank {
                out.push_str("\n\n");
            } else {
                out.push_str("  \n");
            }
        }
        out.push_str(&format_markdown_preserving_leading_indent(content));
        saw_text = true;
        pending_blank = false;
    }

    out
}

fn strip_doc_comment_prefix_preserving_indent(line: &str) -> Option<&str> {
    if let Some(rest) = line.strip_prefix("---") {
        if !rest.is_empty() && rest.chars().all(|ch| ch == '-') {
            return line.strip_prefix("--");
        }
        return Some(rest);
    }
    line.strip_prefix("--")
}

fn trim_one_comment_padding_space(text: &str) -> &str {
    text.strip_prefix(' ')
        .or_else(|| text.strip_prefix('\t'))
        .unwrap_or(text)
}

fn format_markdown_preserving_leading_indent(text: &str) -> String {
    let mut out = String::new();
    let mut rest_start = 0;
    for (idx, ch) in text.char_indices() {
        match ch {
            ' ' => {
                out.push_str("&nbsp;");
                rest_start = idx + ch.len_utf8();
            }
            '\t' => {
                out.push_str("&nbsp;&nbsp;&nbsp;&nbsp;");
                rest_start = idx + ch.len_utf8();
            }
            _ => break,
        }
    }
    out.push_str(&text[rest_start..]);
    out
}

fn find_enclosing_statement(node: tree_sitter::Node) -> tree_sitter::Node {
    let mut current = node;
    loop {
        match current.kind_name() {
            "function_declaration"
            | "local_function_declaration"
            | "local_declaration"
            | "assignment_statement"
            | "function_call_statement" => return current,
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

fn find_enclosing_emmy_line(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node;
    loop {
        match current.kind_name() {
            "emmy_line" => return Some(current),
            "function_declaration"
            | "local_function_declaration"
            | "local_declaration"
            | "assignment_statement"
            | "function_call_statement" => return None,
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

fn find_enclosing_table_field(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut current = node;
    loop {
        match current.kind_name() {
            "field" => return Some(current),
            "function_declaration"
            | "local_function_declaration"
            | "local_declaration"
            | "assignment_statement"
            | "function_call_statement" => return None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua_symbol::intern_lua_symbol;
    use crate::summary::{CallSite, DocumentSummary, FunctionSummary, GlobalContribution};
    use crate::type_system::{FunctionSummaryId, ParamInfo};
    use crate::types::{DefKind, Definition};
    use crate::uri_id::intern_uri;
    use crate::util::ByteRange;
    use std::collections::HashMap;

    #[test]
    fn field_definition_lines_use_resolved_method_self_type() {
        let annotations = parse_emmy_comments("---@field miscFunc :fun(x: number): self");
        let lines = code_lines_for_field_definition(
            &annotations,
            "miscFunc".to_string(),
            Some("fun(self: MiscManager, x: number): MiscManager"),
        );

        assert_eq!(
            lines[0],
            "---@field miscFunc :fun(self: MiscManager, x: number): MiscManager"
        );
        assert!(!lines[0].contains("self: self"));
    }

    fn empty_range() -> ByteRange {
        ByteRange::default()
    }

    fn summary_with_function(
        func_id: FunctionSummaryId,
        name: &str,
        signature: FunctionSignature,
    ) -> DocumentSummary {
        let mut function_summaries = HashMap::new();
        function_summaries.insert(
            func_id,
            FunctionSummary {
                name: intern_lua_symbol(name),
                signature,
                range: empty_range(),
                signature_fingerprint: 0,
                emmy_annotated: true,
                overloads: Vec::new(),
                generic_params: Vec::new(),
            },
        );
        DocumentSummary {
            uri: "file:///test.lua".parse().unwrap(),
            global_contributions: Vec::<GlobalContribution>::new(),
            function_summaries,
            function_name_index: HashMap::new(),
            type_definitions: Vec::new(),
            table_shapes: HashMap::new(),
            module_return_type: None,
            module_return_range: None,
            signature_fingerprint: 0,
            call_sites: Vec::<CallSite>::new(),
            is_meta: false,
            meta_name: None,
        }
    }

    #[test]
    fn hover_type_display_expands_function_ref_signature() {
        let func_id = FunctionSummaryId(7);
        let summary = summary_with_function(
            func_id,
            "Audit.log",
            FunctionSignature {
                params: vec![
                    ParamInfo {
                        name: intern_lua_symbol("action"),
                        type_fact: TypeFact::Known(KnownType::String),
                        optional: false,
                    },
                    ParamInfo {
                        name: intern_lua_symbol("enabled"),
                        type_fact: TypeFact::Known(KnownType::Boolean),
                        optional: true,
                    },
                ],
                returns: vec![TypeFact::Known(KnownType::String)],
            },
        );

        assert_eq!(
            format_type_fact_for_hover(&TypeFact::Known(KnownType::FunctionRef(func_id)), &summary),
            "fun(action: string, enabled?: boolean): string"
        );
    }

    #[test]
    fn hover_type_display_expands_colon_method_with_implicit_self() {
        let func_id = FunctionSummaryId(8);
        let summary = summary_with_function(
            func_id,
            "Audit:init",
            FunctionSignature {
                params: vec![ParamInfo {
                    name: intern_lua_symbol("action"),
                    type_fact: TypeFact::Known(KnownType::String),
                    optional: false,
                }],
                returns: vec![TypeFact::Known(KnownType::Boolean)],
            },
        );

        assert_eq!(
            format_type_fact_for_hover(&TypeFact::Known(KnownType::FunctionRef(func_id)), &summary),
            "fun(self: Audit, action: string): boolean"
        );
    }

    #[test]
    fn hover_type_display_uses_class_owner_for_bound_colon_method_self() {
        let func_id = FunctionSummaryId(9);
        let summary = summary_with_function(
            func_id,
            "M:init",
            FunctionSignature {
                params: vec![ParamInfo {
                    name: intern_lua_symbol("action"),
                    type_fact: TypeFact::Known(KnownType::String),
                    optional: false,
                }],
                returns: Vec::new(),
            },
        );

        assert_eq!(
            format_type_fact_for_class_field_hover(
                &TypeFact::Known(KnownType::FunctionRef(func_id)),
                &summary,
                "Audit"
            ),
            "fun(self: Audit, action: string)"
        );
    }

    #[test]
    fn hover_type_display_does_not_duplicate_explicit_self_param() {
        let func_id = FunctionSummaryId(10);
        let summary = summary_with_function(
            func_id,
            "Audit:init",
            FunctionSignature {
                params: vec![
                    ParamInfo {
                        name: intern_lua_symbol("self"),
                        type_fact: TypeFact::Known(KnownType::EmmyType(intern_lua_symbol("Audit"))),
                        optional: false,
                    },
                    ParamInfo {
                        name: intern_lua_symbol("action"),
                        type_fact: TypeFact::Known(KnownType::String),
                        optional: false,
                    },
                ],
                returns: Vec::new(),
            },
        );

        assert_eq!(
            format_type_fact_for_class_field_hover(
                &TypeFact::Known(KnownType::FunctionRef(func_id)),
                &summary,
                "Audit"
            ),
            "fun(self: Audit, action: string)"
        );
    }

    #[test]
    fn definition_origin_link_escapes_markdown_target_parentheses() {
        let uri: Uri = "file:///test/foo(bar).lua".parse().unwrap();
        let uri_id = intern_uri(&uri);
        let def = Definition {
            name: "value".to_string(),
            kind: DefKind::LocalVariable,
            range: ByteRange::default(),
            selection_range: ByteRange {
                start_row: 6,
                ..ByteRange::default()
            },
            uri_id,
            uri,
        };

        assert_eq!(
            definition_origin_link(&def).as_deref(),
            Some("[foo(bar).lua](file:///test/foo%28bar%29.lua#L7)")
        );
    }

    #[test]
    fn extract_doc_lines_preserves_comment_layout_as_markdown() {
        let comments = vec![
            "--- first line".to_string(),
            "--- second line".to_string(),
            "---Returns text without padding space.".to_string(),
            "---[`View online doc`](https://example.test)".to_string(),
            "---".to_string(),
            "-- USAGE:".to_string(),
            "--   json.encode(o)".to_string(),
            "--     Returns a JSON string.".to_string(),
            "--".to_string(),
            "-- CHANGELOG".to_string(),
            "--    1.0.1 Introduced plugin info.".to_string(),
            "-- \t\tIntroduced json.null.".to_string(),
            "-----".to_string(),
            "---@type string".to_string(),
            "---#disable top_keyword".to_string(),
        ];

        assert_eq!(
            extract_doc_lines(&comments),
            "first line  \nsecond line  \nReturns text without padding space.  \n[`View online doc`](https://example.test)\n\nUSAGE:  \n&nbsp;&nbsp;json.encode(o)  \n&nbsp;&nbsp;&nbsp;&nbsp;Returns a JSON string.\n\nCHANGELOG  \n&nbsp;&nbsp;&nbsp;1.0.1 Introduced plugin info.  \n&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;Introduced json.null.  \n---"
        );
    }
}
