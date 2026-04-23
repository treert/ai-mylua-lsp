use tower_lsp_server::ls_types::*;
use crate::document::Document;
use crate::emmy::{collect_preceding_comments, collect_trailing_comment, parse_emmy_comments, format_annotations_markdown};
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
        let type_info = resolve_local_type_info(uri, ident_text, index);
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
    let source = doc.text.as_bytes();

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
    let range = name_node.map(|n| crate::util::ts_node_to_range(n, source));

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
    index: &mut WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let source = doc.text.as_bytes();
    let base_node = call_node.child_by_field_name("callee")?;
    let name_node = call_node.child_by_field_name("method")?;
    build_field_hover(base_node, name_node, "method", source, uri, index, all_docs)
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
    let base_node = var_node.child_by_field_name("object")?;
    let name_node = var_node.child_by_field_name("field")?;
    build_field_hover(base_node, name_node, "field", source, uri, index, all_docs)
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
    index: &mut WorkspaceAggregation,
    all_docs: &std::collections::HashMap<Uri, Document>,
) -> Option<Hover> {
    let field_name = node_text(name_node, source).to_string();

    let base_fact = infer_node_type(base_node, source, uri, index);
    lsp_log!(
        "[hover_{kind}] base='{}' base_fact={:?} {kind}='{}'",
        node_text(base_node, source),
        base_fact,
        field_name,
        kind = kind_label,
    );
    let resolved = resolver::resolve_field_chain_in_file(
        uri, &base_fact, &[field_name.clone()], index,
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
        range: Some(crate::util::ts_node_to_range(name_node, source)),
    })
}

/// Recursively infer the type of an AST expression node.
///
/// The mylua grammar uses `variable` nodes for both plain identifiers and
/// dotted access (`a.b.c` is a `variable` whose `object` field is another
/// `variable` and whose `field` field is an identifier). `field_expression`
/// is kept as a legacy alias for future grammar revisions.
///
/// Handles:
/// - Pure dotted chains (`a.b.c`) via recursive `variable.object/.field`.
/// - Array-style subscripts (`a[1]`, `a[k]`) via `array_element_type` on
///   the base's file-local Table shape.
/// - Call returns (`foo()`, `mod.f()`, `obj:m()`) by reconstructing a
///   `CallReturn` stub so the resolver can track declared `@return`
///   types through the chain — this is what makes `make().field` hover
///   work when `make`'s summary has `@return Foo`.
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
                let resolved = resolver::resolve_field_chain_in_file(
                    uri, &base_fact, &[field_name], index,
                );
                return resolved.type_fact;
            }
            // Subscript variant: `variable { object, index }` — look up
            // the base's shape `array_element_type` so chains like
            // `a[1].field` can continue with a real element type.
            if let (Some(object), Some(_index_node)) = (
                node.child_by_field_name("object"),
                node.child_by_field_name("index"),
            ) {
                let base_fact = infer_node_type(object, source, uri, index);
                if let TypeFact::Known(crate::type_system::KnownType::Table(shape_id)) = &base_fact {
                    if let Some(summary) = index.summaries.get(uri) {
                        if let Some(shape) = summary.table_shapes.get(shape_id) {
                            if let Some(elem) = &shape.array_element_type {
                                return elem.clone();
                            }
                        }
                    }
                }
                return TypeFact::Unknown;
            }
            // `variable` wrapping a single identifier — local/global lookup.
            if node.named_child_count() == 1 {
                if let Some(child) = node.named_child(0) {
                    if child.kind() == "identifier" {
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
        "function_call" => {
            // Reconstruct a `CallReturn` stub (or `RequireRef` for
            // `require("…")`) so the resolver can pick up declared
            // `@return` types. Mirrors the logic in
            // `summary_builder::infer_call_return_type` but works off the
            // workspace aggregation + summary cache rather than the
            // per-file `BuildContext`.
            infer_call_return_fact(node, source, uri, index)
        }
        "parenthesized_expression" => {
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
        // Literal types — needed for function-level generic inference
        // so that `identity("abc")` can infer `T = string`.
        "number" => TypeFact::Known(crate::type_system::KnownType::Number),
        "string" => TypeFact::Known(crate::type_system::KnownType::String),
        "true" | "false" => TypeFact::Known(crate::type_system::KnownType::Boolean),
        "nil" => TypeFact::Known(crate::type_system::KnownType::Nil),
        _ => TypeFact::Unknown,
    }
}

/// Collect the inferred types of actual arguments at a function call site.
/// Used by function-level generic inference in the hover path.
fn collect_hover_call_arg_types(
    call_node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Vec<TypeFact> {
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    crate::util::extract_call_arg_nodes(args, source)
        .into_iter()
        .map(|e| infer_node_type(e, source, uri, index))
        .collect()
}

/// Build a `TypeFact` for the return value of a `function_call` node.
/// Handles three shapes:
/// - `require("mod")`  → `SymbolicStub::RequireRef { module_path }`
/// - `obj:m(...)`      → `CallReturn { base: <obj-fact-as-stub>, func_name: "m" }`
/// - `callee(...)` where callee is a `variable` (identifier or dotted) →
///   `CallReturn { base: <callee-base-as-stub>, func_name }`
/// - Plain local/global function call → look up `FunctionSummary.returns[0]`
///   in the workspace to return the declared first return type.
fn infer_call_return_fact(
    node: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> TypeFact {
    use crate::type_system::{SymbolicStub, KnownType};

    let callee = match node.child_by_field_name("callee") {
        Some(c) => c,
        None => return TypeFact::Unknown,
    };

    // `require("mod")` — note callee is a plain identifier.
    if callee.kind() == "identifier" && node_text(callee, source) == "require" {
        if let Some(args) = node.child_by_field_name("arguments") {
            if let Some(first_arg) = args.named_child(0) {
                if let Some(module_path) = extract_string_literal(first_arg, source) {
                    return TypeFact::Stub(SymbolicStub::RequireRef { module_path });
                }
            }
        }
        return TypeFact::Unknown;
    }

    // `obj:m()` — grammar sets `method` field on the call node itself.
    if let Some(method_node) = node.child_by_field_name("method") {
        let method_name = node_text(method_node, source).to_string();
        let base_fact = infer_node_type(callee, source, uri, index);

        // When the base is a generic class instance (e.g. `Stack<string>`),
        // resolve the method's return type eagerly and substitute generic
        // parameters. A `CallReturn` stub would lose the actual type args.
        if let TypeFact::Known(KnownType::EmmyGeneric(ref type_name, ref actual_params)) = base_fact {
            let field_result = resolver::resolve_field_chain_in_file(
                uri, &base_fact, &[method_name.clone()], index,
            );
            // If the field resolved to a function, extract its first return
            // type (already substituted by resolve_field_chain_in_file's
            // EmmyGeneric branch).
            if let TypeFact::Known(KnownType::Function(ref sig)) = field_result.type_fact {
                if let Some(ret) = sig.returns.first() {
                    return ret.clone();
                }
            }
            // Fallback: look up the method in function_summaries and
            // substitute generics on the raw return type.
            let ret_fact = resolver::resolve_method_return_with_generics(
                type_name, &method_name, actual_params, index,
            );
            if ret_fact != TypeFact::Unknown {
                return ret_fact;
            }
        }

        let (base_stub, generic_args) = type_fact_to_stub_for_call_base(&base_fact, callee, source);
        return TypeFact::Stub(SymbolicStub::CallReturn {
            base: Box::new(base_stub),
            func_name: method_name,
            generic_args,
        });
    }

    // Dotted call `mod.f()` — callee is a `variable` with `object`+`field`.
    if matches!(callee.kind(), "variable" | "field_expression") {
        if let (Some(base_node), Some(field_node)) = (
            callee.child_by_field_name("object"),
            callee.child_by_field_name("field"),
        ) {
            let func_name = node_text(field_node, source).to_string();
            let base_fact = infer_node_type(base_node, source, uri, index);
            let (base_stub, generic_args) = type_fact_to_stub_for_call_base(&base_fact, base_node, source);
            return TypeFact::Stub(SymbolicStub::CallReturn {
                base: Box::new(base_stub),
                func_name,
                generic_args,
            });
        }
    }

    // Plain local/global call — pick up the declared first return type
    // from the callee's FunctionSummary (if any).
    let callee_text = node_text(callee, source);

    // Extract function summary data first (immutable borrow of index),
    // then release the borrow before calling collect_hover_call_arg_types
    // (which needs mutable borrow).
    let fs_data = index.summaries.get(uri).and_then(|summary| {
        summary.function_summaries.get(callee_text).map(|fs| {
            (
                fs.generic_params.clone(),
                fs.signature.params.clone(),
                fs.signature.returns.clone(),
            )
        })
    });

    if let Some((generic_params, formal_params, returns)) = fs_data {
        // Function-level generic inference: if the callee has @generic params,
        // try to unify them from the actual argument types at the call site.
        if !generic_params.is_empty() {
            let actual_arg_types = collect_hover_call_arg_types(node, source, uri, index);
            if let Some(substituted_returns) = resolver::unify_function_generics(
                &generic_params,
                &formal_params,
                &actual_arg_types,
                &returns,
            ) {
                if let Some(ret) = substituted_returns.first() {
                    return ret.clone();
                }
            }
        }
        if let Some(ret) = returns.first() {
            // `@return T` gives us an EmmyType stub; keep it as-is
            // so the resolver can look up `T`'s fields.
            return match ret {
                TypeFact::Known(KnownType::EmmyType(name)) => {
                    TypeFact::Stub(SymbolicStub::TypeRef { name: name.clone() })
                }
                other => other.clone(),
            };
        }
    }
    TypeFact::Unknown
}

/// Best-effort conversion of a base expression's inferred `TypeFact`
/// into a `SymbolicStub` suitable for `CallReturn.base`. Mirrors the
/// build-time logic in `summary_builder::infer_call_return_type`.
fn type_fact_to_stub_for_call_base(
    base_fact: &TypeFact,
    base_node: tree_sitter::Node,
    source: &[u8],
) -> (crate::type_system::SymbolicStub, Vec<TypeFact>) {
    use crate::type_system::{SymbolicStub, KnownType};
    match base_fact {
        TypeFact::Stub(s) => (s.clone(), vec![]),
        TypeFact::Known(KnownType::EmmyType(type_name)) => {
            (SymbolicStub::TypeRef { name: type_name.clone() }, vec![])
        }
        TypeFact::Known(KnownType::EmmyGeneric(type_name, params)) => {
            (SymbolicStub::TypeRef { name: type_name.clone() }, params.clone())
        }
        _ => (SymbolicStub::GlobalRef {
            name: node_text(base_node, source).to_string(),
        }, vec![]),
    }
}

/// Extract the string literal payload from a `string` tree-sitter node
/// (strips surrounding quotes). Returns None for non-string expressions.
fn extract_string_literal(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let text = node_text(node, source);
    // Strip single matching pair of quotes (handles "..." and '...'), no
    // long bracket support needed because `require("...")` canonically
    // uses short strings.
    if text.len() >= 2 {
        let bytes = text.as_bytes();
        let first = bytes[0];
        let last = bytes[text.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return Some(text[1..text.len() - 1].to_string());
        }
    }
    None
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
        range: Some(def.selection_range.clone()),
    })
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
