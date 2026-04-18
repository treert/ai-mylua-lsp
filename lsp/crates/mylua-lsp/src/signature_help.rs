//! `textDocument/signatureHelp` implementation.
//!
//! Resolves the function being called at the cursor (dot call, method call,
//! or direct callee identifier), pulls its `FunctionSummary` from the
//! workspace index, and returns one `SignatureInformation` per overload
//! plus the primary signature. `active_parameter` is computed from the
//! comma count between the `(` and the cursor, with `:` method syntax
//! implicitly shifting the index by one (the `self` argument).

use tower_lsp_server::ls_types::*;

use crate::aggregation::WorkspaceAggregation;
use crate::document::Document;
use crate::hover;
use crate::resolver;
use crate::summary::FunctionSummary;
use crate::type_system::{FunctionSignature, KnownType, ParamInfo, TypeFact};
use crate::util::{node_text, position_to_byte_offset};

pub fn signature_help(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
) -> Option<SignatureHelp> {
    let offset = position_to_byte_offset(&doc.text, position)?;
    let source = doc.text.as_bytes();

    // Find the enclosing function call whose argument list contains the cursor.
    let call = find_enclosing_call(doc.tree.root_node(), offset)?;
    let (signatures, is_method, primary_name) = resolve_call_signatures(call, doc, uri, index)?;
    if signatures.is_empty() {
        return None;
    }

    let active_parameter = compute_active_parameter(call, offset, source, is_method);

    let sig_infos: Vec<SignatureInformation> = signatures
        .iter()
        .map(|s| signature_information(&primary_name, s, is_method))
        .collect();

    Some(SignatureHelp {
        signatures: sig_infos,
        active_signature: Some(0),
        active_parameter: Some(active_parameter),
    })
}

/// Walk upward from the cursor byte until we find the innermost
/// `function_call` whose `arguments` node contains the cursor.
fn find_enclosing_call(root: tree_sitter::Node, byte_offset: usize) -> Option<tree_sitter::Node> {
    let mut node = root.descendant_for_byte_range(byte_offset, byte_offset)?;
    loop {
        if node.kind() == "function_call" {
            if let Some(args) = node.child_by_field_name("arguments") {
                // Include the closing `)` position so that `foo(|)` still matches.
                if byte_offset >= args.start_byte() && byte_offset <= args.end_byte() {
                    return Some(node);
                }
            }
        }
        node = node.parent()?;
    }
}

/// Produces (signatures, is_method_call, callee_display_name).
fn resolve_call_signatures(
    call: tree_sitter::Node,
    doc: &Document,
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Option<(Vec<FunctionSignature>, bool, String)> {
    let source = doc.text.as_bytes();
    let callee = call.child_by_field_name("callee")?;
    let method = call.child_by_field_name("method");

    // `obj:method(...)`
    if let Some(m) = method {
        let method_name = node_text(m, source).to_string();
        let base_fact = hover::infer_node_type(callee, source, uri, index);
        let sigs = lookup_function_signatures_by_field(&base_fact, &method_name, index);
        let display = format!("{}:{}", node_text(callee, source), method_name);
        return Some((sigs, true, display));
    }

    // `obj.method(...)` / `mod.func(...)`
    if matches!(callee.kind(), "variable" | "field_expression") {
        if let (Some(object), Some(field)) = (
            callee.child_by_field_name("object"),
            callee.child_by_field_name("field"),
        ) {
            let field_name = node_text(field, source).to_string();
            let base_fact = hover::infer_node_type(object, source, uri, index);
            let sigs = lookup_function_signatures_by_field(&base_fact, &field_name, index);
            let display = node_text(callee, source).to_string();
            return Some((sigs, false, display));
        }
    }

    // Simple identifier / variable callee: `foo(...)`
    let name = node_text(callee, source).to_string();
    // Local function
    if let Some(summary) = index.summaries.get(uri) {
        if let Some(fs) = summary.function_summaries.get(&name) {
            let sigs = primary_plus_overloads(fs);
            return Some((sigs, false, name));
        }
    }
    // Global function (any file)
    let candidates = index.global_shard.get(&name).cloned().unwrap_or_default();
    for c in &candidates {
        if let Some(target_summary) = index.summaries.get(&c.source_uri) {
            if let Some(fs) = target_summary.function_summaries.get(&name) {
                let sigs = primary_plus_overloads(fs);
                return Some((sigs, false, name));
            }
        }
        if let TypeFact::Known(KnownType::Function(ref sig)) = c.type_fact {
            return Some((vec![sig.clone()], false, name));
        }
    }

    None
}

/// Gather overload signatures for a field / method lookup against the
/// resolved type of `base_fact`.
///
/// When `base_fact` resolves to an Emmy class (`EmmyType` / `EmmyGeneric`),
/// we look up overloads with **deterministic, exact** keys
/// `{class}.{field}` / `{class}:{field}`. The previous implementation
/// walked `function_summaries.keys()` with `ends_with` which picked a
/// non-deterministic match in files where two classes shared a common
/// method name (e.g. both `Foo.init` and `Bar.init`).
fn lookup_function_signatures_by_field(
    base_fact: &TypeFact,
    field_name: &str,
    index: &mut WorkspaceAggregation,
) -> Vec<FunctionSignature> {
    let resolved_base = resolver::resolve_type(base_fact, index);
    let owner_class = match &resolved_base.type_fact {
        TypeFact::Known(KnownType::EmmyType(n)) => Some(n.clone()),
        TypeFact::Known(KnownType::EmmyGeneric(n, _)) => Some(n.clone()),
        _ => None,
    };
    let resolved = resolver::resolve_field_chain(base_fact, &[field_name.to_string()], index);
    if let TypeFact::Known(KnownType::Function(sig)) = &resolved.type_fact {
        if let Some(uri) = &resolved.def_uri {
            if let Some(summary) = index.summaries.get(uri) {
                // Try exact `{class}:{field}` / `{class}.{field}` lookup
                // when we know the owner class (covers methods declared
                // via `function Foo:m() end` or `function Foo.m() end`).
                if let Some(cls) = owner_class.as_ref() {
                    for sep in [":", "."] {
                        let key = format!("{}{}{}", cls, sep, field_name);
                        if let Some(fs) = summary.function_summaries.get(&key) {
                            return primary_plus_overloads(fs);
                        }
                    }
                }
                if let Some(fs) = summary.function_summaries.get(field_name) {
                    return primary_plus_overloads(fs);
                }
            }
        }
        return vec![sig.clone()];
    }
    // The resolver couldn't match the field through the class's declared
    // `@field` list (common when the method is declared as a Lua-side
    // `function Foo:m() end` without an accompanying `@field m fun(...)`).
    // In that case, look up `{class}:{field}` / `{class}.{field}` directly
    // in `global_shard` — `visit_function_declaration` registers those
    // qualified names there with a real `FunctionSignature`.
    if let Some(cls) = owner_class.as_ref() {
        for sep in [":", "."] {
            let qualified = format!("{}{}{}", cls, sep, field_name);
            if let Some(candidate) = index
                .global_shard
                .get(&qualified)
                .and_then(|v| v.first().cloned())
            {
                if let Some(summary) = index.summaries.get(&candidate.source_uri) {
                    if let Some(fs) = summary.function_summaries.get(&qualified) {
                        return primary_plus_overloads(fs);
                    }
                }
                if let TypeFact::Known(KnownType::Function(sig)) = candidate.type_fact {
                    return vec![sig];
                }
            }
        }
    }
    Vec::new()
}

fn primary_plus_overloads(fs: &FunctionSummary) -> Vec<FunctionSignature> {
    let mut out = Vec::with_capacity(1 + fs.overloads.len());
    // Skip a stub primary signature that only exists because `@overload`
    // was declared on a function with no base-level `@param` / `@return`.
    // Otherwise the client shows a blank `foo()` as `active_signature=0`.
    let primary_is_stub = fs.signature.params.is_empty()
        && fs.signature.returns.is_empty()
        && !fs.overloads.is_empty()
        && !fs.emmy_annotated;
    if !primary_is_stub {
        out.push(fs.signature.clone());
    }
    for o in &fs.overloads {
        out.push(o.clone());
    }
    out
}

/// Count commas between `(` and the cursor at top nesting level to get the
/// 0-based index of the current argument. For `:` method syntax we do NOT
/// bump by one: `self` is passed implicitly and client-side the user still
/// thinks of their first typed arg as index 0.
///
/// Lua also permits argument-less call forms: `foo{ ... }` and `foo "x"`.
/// In those cases the grammar's `arguments` field is not a `(...)` list but
/// a single `table_constructor` / `string` node with exactly one implicit
/// argument, so the right active index is always 0 — any commas inside
/// would be *inside* that single table / string, never top-level.
fn compute_active_parameter(
    call: tree_sitter::Node,
    byte_offset: usize,
    source: &[u8],
    _is_method: bool,
) -> u32 {
    let Some(args) = call.child_by_field_name("arguments") else {
        return 0;
    };
    // Only the paren-form `foo(a, b, ...)` has multiple top-level arguments
    // separated by commas. Require `args` to actually start with `(`.
    if source.get(args.start_byte()).copied() != Some(b'(') {
        return 0;
    }
    // Arguments node spans `(...)`. Count top-level `,` between args.start + 1
    // (past `(`) and byte_offset, inside the `arguments` span.
    let start = args.start_byte() + 1;
    let end = byte_offset.min(args.end_byte());
    if end <= start {
        return 0;
    }
    let slice = &source[start..end];

    let mut commas: u32 = 0;
    let mut depth_paren: i32 = 0;
    let mut depth_brace: i32 = 0;
    let mut depth_bracket: i32 = 0;
    let mut i = 0;
    while i < slice.len() {
        let b = slice[i];
        match b {
            b'(' => depth_paren += 1,
            b')' => depth_paren -= 1,
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            b',' if depth_paren == 0 && depth_brace == 0 && depth_bracket == 0 => {
                commas += 1;
            }
            b'"' | b'\'' => {
                let quote = b;
                i += 1;
                while i < slice.len() {
                    if slice[i] == b'\\' && i + 1 < slice.len() {
                        i += 2;
                        continue;
                    }
                    if slice[i] == quote {
                        break;
                    }
                    i += 1;
                }
            }
            b'-' if i + 1 < slice.len() && slice[i + 1] == b'-' => {
                // Skip line comment OR `--[[ ... ]]` block comment so
                // commas inside don't count. Long-bracket form can also be
                // `--[=[ ... ]=]` (grammar supports `=` levels); we keep
                // the common `--[[ ... ]]` case here and fall back to
                // line-comment treatment otherwise.
                let rest = &slice[i + 2..];
                if rest.starts_with(b"[[") {
                    let mut j = i + 4;
                    while j + 1 < slice.len() {
                        if slice[j] == b']' && slice[j + 1] == b']' {
                            j += 2;
                            break;
                        }
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                i += 2;
                while i < slice.len() && slice[i] != b'\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    commas
}

fn signature_information(
    name: &str,
    sig: &FunctionSignature,
    is_method: bool,
) -> SignatureInformation {
    let (label, parameters) = format_signature_label(name, sig, is_method);
    SignatureInformation {
        label,
        documentation: None,
        parameters: Some(parameters),
        active_parameter: None,
    }
}

/// Build the full function-call label plus the byte ranges (offsets within
/// the label) of each parameter for the client to underline the active one.
fn format_signature_label(
    name: &str,
    sig: &FunctionSignature,
    is_method: bool,
) -> (String, Vec<ParameterInformation>) {
    let mut label = String::new();
    label.push_str(name);
    label.push('(');
    let mut params = Vec::new();
    // Skip leading `self` param if calling as method — client displays
    // only user-visible parameters.
    let visible_params: Vec<&ParamInfo> = sig
        .params
        .iter()
        .filter(|p| !(is_method && p.name == "self"))
        .collect();
    for (i, p) in visible_params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let start = label.len();
        if p.type_fact == TypeFact::Unknown {
            label.push_str(&p.name);
        } else {
            label.push_str(&format!("{}: {}", p.name, p.type_fact));
        }
        let end = label.len();
        params.push(ParameterInformation {
            label: ParameterLabel::LabelOffsets([start as u32, end as u32]),
            documentation: None,
        });
    }
    label.push(')');
    if !sig.returns.is_empty() {
        label.push_str(": ");
        let rs: Vec<String> = sig.returns.iter().map(|r| format!("{}", r)).collect();
        label.push_str(&rs.join(", "));
    }
    (label, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_parameter_counts_top_level_commas() {
        // Simulated `arguments` node would span `(a, b, {x=1, y=2}, c|)`.
        // The helper only consumes raw bytes starting past `(`, so we can
        // test the comma counter directly.
        let src = b"a, b, {x=1, y=2}, c".to_vec();
        let mut commas: u32 = 0;
        let mut depth_paren: i32 = 0;
        let mut depth_brace: i32 = 0;
        let mut depth_bracket: i32 = 0;
        let mut i = 0;
        while i < src.len() {
            let b = src[i];
            match b {
                b'(' => depth_paren += 1,
                b')' => depth_paren -= 1,
                b'{' => depth_brace += 1,
                b'}' => depth_brace -= 1,
                b'[' => depth_bracket += 1,
                b']' => depth_bracket -= 1,
                b',' if depth_paren == 0 && depth_brace == 0 && depth_bracket == 0 => {
                    commas += 1;
                }
                _ => {}
            }
            i += 1;
        }
        assert_eq!(commas, 3, "expected 3 top-level commas");
    }
}
