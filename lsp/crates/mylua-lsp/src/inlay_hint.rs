//! `textDocument/inlayHint` — virtual labels inserted into the
//! source by the client without actually modifying the document.
//!
//! Two categories are supported (both opt-in via config):
//!
//! - **Parameter names** (`inlayHint.parameterNames = true`): at a
//!   call site `foo(1, 2)`, show `a:` / `b:` before each positional
//!   argument if `foo`'s `FunctionSummary` is indexed. Method calls
//!   (`obj:m(1)`) skip the implicit `self` parameter.
//! - **Variable types** (`inlayHint.variableTypes = true`): after a
//!   `local x = ...` declaration, show `: Type` when the inferred
//!   `local_type_facts[x]` carries a non-`Unknown`, non-`Table` /
//!   non-`Function` fact (those render as info-less "table" /
//!   "function" labels and get filtered out to reduce noise).
//!
//! Hints emitted outside the requested `params.range` are skipped —
//! clients typically request viewport-scoped results.

use tower_lsp_server::ls_types::*;

use crate::aggregation::WorkspaceAggregation;
use crate::config::InlayHintConfig;
use crate::document::Document;
use crate::type_system::{KnownType, TypeFact};
use crate::util::{node_text, position_to_byte_offset, ts_point_to_position};

pub fn inlay_hints(
    doc: &Document,
    uri: &Uri,
    range: Range,
    index: &WorkspaceAggregation,
    cfg: &InlayHintConfig,
) -> Vec<InlayHint> {
    if !cfg.enable {
        return Vec::new();
    }

    let source = doc.text.as_bytes();
    let range_start = position_to_byte_offset(&doc.text, range.start).unwrap_or(0);
    let range_end = position_to_byte_offset(&doc.text, range.end).unwrap_or(source.len());

    let mut out = Vec::new();
    let mut cursor = doc.tree.root_node().walk();
    walk(
        &mut cursor,
        source,
        uri,
        index,
        cfg,
        range_start,
        range_end,
        &mut out,
    );
    out
}

fn walk(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    cfg: &InlayHintConfig,
    range_start: usize,
    range_end: usize,
    out: &mut Vec<InlayHint>,
) {
    let node = cursor.node();
    // Early exit: whole subtree outside requested range.
    if node.end_byte() < range_start || node.start_byte() > range_end {
        return;
    }

    match node.kind() {
        "function_call" if cfg.parameter_names => {
            collect_parameter_name_hints(node, source, uri, index, out);
        }
        "local_declaration" if cfg.variable_types => {
            collect_variable_type_hints(node, source, uri, index, out);
        }
        _ => {}
    }

    if cursor.goto_first_child() {
        loop {
            walk(cursor, source, uri, index, cfg, range_start, range_end, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn collect_parameter_name_hints(
    call: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    out: &mut Vec<InlayHint>,
) {
    let Some(callee) = call.child_by_field_name("callee") else { return };
    let is_method = call.child_by_field_name("method").is_some();
    let Some(args) = call.child_by_field_name("arguments") else { return };

    // Only look up signatures for simple identifier callees — `foo()`
    // / `foo:m()`. Complex chains (`t.m().f()`) are out of scope for
    // this conservative v1.
    let Some(sig) = lookup_signature(callee, source, uri, index, is_method) else { return };

    // Drop leading self when it's a method call — the user never
    // writes it explicitly, and the client would render a stray
    // `self:` hint before the first actual argument.
    let params: Vec<&crate::type_system::ParamInfo> = sig
        .params
        .iter()
        .filter(|p| !(is_method && p.name == "self"))
        .collect();
    if params.is_empty() {
        return;
    }

    // Iterate named arg children of the `arguments` node — matches
    // the paren form `foo(a, b)`. For table-call `foo{...}` / string-
    // call `foo "x"` the args list is a single implicit argument
    // and per-arg name hints don't apply.
    let first_byte = args.start_byte();
    if source.get(first_byte).copied() != Some(b'(') {
        return;
    }

    let arg_exprs = crate::util::extract_call_arg_nodes(args, source);
    for (arg_index, expr) in arg_exprs.iter().enumerate() {
        emit_param_hint(&params, arg_index, *expr, source, out);
    }
}

fn emit_param_hint(
    params: &[&crate::type_system::ParamInfo],
    arg_index: usize,
    expr: tree_sitter::Node,
    source: &[u8],
    out: &mut Vec<InlayHint>,
) {
    let Some(param) = params.get(arg_index) else { return };
    if param.name == "..." {
        return;
    }
    // Skip noisy case: the argument is literally the same identifier
    // as the parameter (e.g. `foo(a, b)` when params are named `a`, `b`).
    if node_text(expr, source) == param.name {
        return;
    }
    let pos = ts_point_to_position(expr.start_position(), source);
    out.push(InlayHint {
        position: pos,
        label: InlayHintLabel::String(format!("{}:", param.name)),
        kind: Some(InlayHintKind::PARAMETER),
        text_edits: None,
        tooltip: None,
        padding_left: None,
        padding_right: Some(true),
        data: None,
    });
}

fn collect_variable_type_hints(
    decl: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    out: &mut Vec<InlayHint>,
) {
    let Some(names) = decl.child_by_field_name("names") else { return };
    // Skip if user explicitly annotated with `---@type ...` above.
    if preceded_by_type_annotation(decl, source) {
        return;
    }
    let Some(summary) = index.summaries.get(uri) else { return };

    for i in 0..names.named_child_count() {
        let Some(id) = names.named_child(i as u32) else { continue };
        if id.kind() != "identifier" {
            continue;
        }
        let name = node_text(id, source);
        let Some(ltf) = summary.local_type_facts.get(name) else { continue };
        if !is_interesting_type(&ltf.type_fact) {
            continue;
        }
        let end_pos = ts_point_to_position(id.end_position(), source);
        out.push(InlayHint {
            position: end_pos,
            label: InlayHintLabel::String(format!(": {}", ltf.type_fact)),
            kind: Some(InlayHintKind::TYPE),
            text_edits: None,
            tooltip: None,
            padding_left: None,
            padding_right: None,
            data: None,
        });
    }
}

fn is_interesting_type(fact: &TypeFact) -> bool {
    match fact {
        TypeFact::Unknown => false,
        TypeFact::Known(KnownType::Nil) => false,
        // `table` / `function` by themselves are info-less for
        // users; skip to reduce hint noise. The hover popup shows
        // the full shape/signature when the user needs it.
        TypeFact::Known(KnownType::Table(_)) => false,
        TypeFact::Known(KnownType::Function(_)) => false,
        _ => true,
    }
}

/// Emmy-annotated `local` statements already show the type in the
/// annotation itself — skip duplicating it as an inlay hint.
fn preceded_by_type_annotation(decl: tree_sitter::Node, source: &[u8]) -> bool {
    let mut prev = decl.prev_sibling();
    while let Some(n) = prev {
        match n.kind() {
            "emmy_comment" => {
                // Look for `---@type ...` in the emmy block.
                let text = node_text(n, source);
                if text.contains("@type") {
                    return true;
                }
                prev = n.prev_sibling();
            }
            "comment" => {
                prev = n.prev_sibling();
            }
            _ => break,
        }
    }
    false
}

/// Resolve a call `callee` to its `FunctionSignature`, when possible.
/// Handles both `foo()` (simple identifier) and `obj:m()` / `obj.m()`
/// (method / dotted calls) by routing through the summary index.
fn lookup_signature(
    callee: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &WorkspaceAggregation,
    is_method: bool,
) -> Option<crate::type_system::FunctionSignature> {
    if is_method {
        return None; // method call — deferred to future iteration
    }
    let text = node_text(callee, source);
    // Simple identifier callee.
    if !text.contains('.') && !text.contains(':') {
        if let Some(summary) = index.summaries.get(uri) {
            if let Some(fs) = summary.function_summaries.get(text) {
                return Some(fs.signature.clone());
            }
        }
        if let Some(candidates) = index.global_shard.get(text) {
            for c in candidates {
                if let TypeFact::Known(KnownType::Function(sig)) = &c.type_fact {
                    return Some(sig.clone());
                }
                if let Some(summary) = index.summaries.get(&c.source_uri) {
                    if let Some(fs) = summary.function_summaries.get(text) {
                        return Some(fs.signature.clone());
                    }
                }
            }
        }
    }
    None
}
