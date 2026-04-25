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
use crate::type_inference;
use crate::resolver;
use crate::summary::FunctionSummary;
use crate::type_system::{FunctionSignature, KnownType, TypeFact};
use crate::util::node_text;

pub fn signature_help(
    doc: &Document,
    uri: &Uri,
    position: Position,
    index: &mut WorkspaceAggregation,
) -> Option<SignatureHelp> {
    let offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;
    let source = doc.source();

    // Find the enclosing function call whose argument list contains the cursor.
    let call = find_enclosing_call(doc.tree.root_node(), offset)?;
    let (signatures, is_method, primary_name) =
        resolve_call_signatures(call, source, uri, index)?;
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
///
/// Exposed at `pub(crate)` so diagnostics (argument-count / -type
/// mismatch checks) can reuse the exact same resolution rules used
/// to populate `signatureHelp`, keeping the two features from
/// drifting on edge cases like cross-file `@field`+impl merging or
/// `@overload` handling.
pub(crate) fn resolve_call_signatures(
    call: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
) -> Option<(Vec<FunctionSignature>, bool, String)> {
    let callee = call.child_by_field_name("callee")?;
    let method = call.child_by_field_name("method");

    // `obj:method(...)`
    if let Some(m) = method {
        let method_name = node_text(m, source).to_string();
        let base_fact = type_inference::infer_node_type(callee, source, uri, index);
        let sigs = lookup_function_signatures_by_field(uri, &base_fact, &method_name, index);
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
            let base_fact = type_inference::infer_node_type(object, source, uri, index);
            let sigs = lookup_function_signatures_by_field(uri, &base_fact, &field_name, index);
            let display = node_text(callee, source).to_string();
            return Some((sigs, false, display));
        }
    }

    // Simple identifier / variable callee: `foo(...)`
    let name = node_text(callee, source).to_string();
    // Local function declaration (`local function f() end`) — rich
    // FunctionSummary with overloads registered in `function_summaries`.
    if let Some(summary) = index.summaries.get(uri) {
        if let Some(fs) = summary.function_summaries.get(&name) {
            let sigs = primary_plus_overloads(fs);
            return Some((sigs, false, name));
        }
    }
    // Local variable bound to a function expression (`local f =
    // function(a, b) ... end` or cross-file `require` returning a
    // callable). Resolve via the type system so inferred / Emmy-
    // enriched signatures from `infer_expression_type` are picked up.
    let base_fact = type_inference::infer_node_type(callee, source, uri, index);
    let resolved = resolver::resolve_type(&base_fact, index);
    if let TypeFact::Known(KnownType::Function(ref sig)) = resolved.type_fact {
        return Some((vec![sig.clone()], false, name));
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
///
/// When the `@class` / `@field` declaration and the actual
/// `function Class:method() end` implementation live in different files
/// (a common pattern when type stubs and runtime code are separated),
/// we additionally look the qualified name up in `global_shard` and
/// merge the implementation file's overloads with the `@field`-declared
/// signature. This ensures `---@overload` annotations sitting above the
/// implementation are not lost. Equal / visually-empty (self-only) impl
/// primaries are filtered to avoid duplicate or blank entries in the
/// client's signature popup.
fn lookup_function_signatures_by_field(
    caller_uri: &Uri,
    base_fact: &TypeFact,
    field_name: &str,
    index: &mut WorkspaceAggregation,
) -> Vec<FunctionSignature> {
    let resolved_base = resolver::resolve_type(base_fact, index);
    let owner_class = match &resolved_base.type_fact {
        TypeFact::Known(KnownType::EmmyType(n) | KnownType::EmmyGeneric(n, _)) => Some(n.clone()),
        _ => None,
    };
    // URI-aware chain resolve so `a.b.c` style callers whose base is a
    // per-file Table shape can still find the signature.
    let resolved = resolver::resolve_field_chain_in_file(
        caller_uri, base_fact, &[field_name.to_string()], index,
    );
    if let TypeFact::Known(KnownType::Function(sig)) = &resolved.type_fact {
        if let Some(def_uri) = &resolved.def_uri {
            if let Some(summary) = index.summaries.get(def_uri) {
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
                // Intentionally no bare `function_summaries.get(field_name)`
                // fallback here: `function_summaries` is keyed by the
                // fully-qualified declaration name, so a bare-name key
                // only exists for top-level `function field_name() end`
                // or `local function field_name() end` — neither of which
                // has any semantic relationship to the `obj.field()` /
                // `obj:field()` call we're resolving. For an Emmy class
                // with an `@field m fun(...)` but no `function Class:m()`
                // body in the same file (P0-R3), pulling that unrelated
                // top-level FunctionSummary would shadow the correctly-
                // resolved `@field` signature. Instead we fall through to
                // the resolver's `sig` below (or to the cross-file impl
                // merge via `lookup_overloads_via_global_shard`).
            }
        }
        // The @field-declared signature lives in `def_uri`, but the actual
        // Lua body + extra `@overload` annotations may live in a different
        // file. Look up `{class}:{field}` / `{class}.{field}` in
        // `global_shard` and merge any overloads we find there.
        if let Some(cls) = owner_class.as_ref() {
            if let Some((impl_uri, impl_sigs)) =
                lookup_overloads_via_global_shard(cls, field_name, index)
            {
                if Some(&impl_uri) != resolved.def_uri.as_ref() {
                    let mut merged = vec![sig.clone()];
                    for s in impl_sigs {
                        // Skip entries that would render as duplicates of
                        // the `@field` primary (same params & returns) or
                        // as a visually-empty method stub (self-only, no
                        // returns) once the client hides `self` for `:`
                        // calls.
                        if s == *sig || is_self_only_method_stub(&s) {
                            continue;
                        }
                        merged.push(s);
                    }
                    return merged;
                }
            }
        }
        // Either `def_uri` was missing, or the impl file was the same as
        // `def_uri` (already exhausted by the block above) and its summary
        // carried no `function_summaries` entry for this name. Fall back
        // to the single signature we already have from `@field`.
        return vec![sig.clone()];
    }
    // The resolver couldn't match the field through the class's declared
    // `@field` list (common when the method is declared as a Lua-side
    // `function Foo:m() end` without an accompanying `@field m fun(...)`).
    // In that case, look up `{class}:{field}` / `{class}.{field}` directly
    // in `global_shard` — `visit_function_declaration` registers those
    // qualified names there with a real `FunctionSignature`.
    if let Some(cls) = owner_class.as_ref() {
        if let Some((_uri, sigs)) = lookup_overloads_via_global_shard(cls, field_name, index) {
            return sigs;
        }
    }
    Vec::new()
}

/// Resolve `{class}:{field}` / `{class}.{field}` against `global_shard` and
/// return the implementation file's `primary+overloads` when available.
/// Used to enrich or rescue overload lookups when the class declaration
/// (`@class` + `@field`) and its implementation (`function Class:method()`)
/// live in different files.
fn lookup_overloads_via_global_shard(
    cls: &str,
    field_name: &str,
    index: &WorkspaceAggregation,
) -> Option<(Uri, Vec<FunctionSignature>)> {
    for sep in [":", "."] {
        let qualified = format!("{}{}{}", cls, sep, field_name);
        if let Some(candidate) = index
            .global_shard
            .get(&qualified)
            .and_then(|v| v.first().cloned())
        {
            if let Some(summary) = index.summaries.get(&candidate.source_uri) {
                if let Some(fs) = summary.function_summaries.get(&qualified) {
                    return Some((candidate.source_uri.clone(), primary_plus_overloads(fs)));
                }
            }
            if let TypeFact::Known(KnownType::Function(sig)) = candidate.type_fact {
                return Some((candidate.source_uri.clone(), vec![sig]));
            }
        }
    }
    None
}

/// True for a primary signature like `function Foo:init() end` that only
/// carries the implicit `self` parameter and no return types: when
/// rendered for a `:` method call we'd hide `self`, leaving the user with
/// a blank `obj:init()` entry that duplicates the real `@field` sig.
fn is_self_only_method_stub(sig: &FunctionSignature) -> bool {
    sig.returns.is_empty() && sig.params.len() == 1 && sig.params[0].name == "self"
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
    count_top_level_commas(&source[start..end])
}

/// Count top-level (depth-0) `,` bytes in the given slice, treating
/// Lua short / long string literals and both line (`--`) and block
/// (`--[[ ... ]]`) comments as opaque regions where commas do not count.
///
/// Exposed at crate scope so it can be unit-tested directly against
/// handcrafted slices (including unterminated-comment edge cases that
/// tree-sitter's error recovery won't surface as a `function_call` at
/// the integration level).
pub(crate) fn count_top_level_commas(slice: &[u8]) -> u32 {
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
                    let mut closed = false;
                    while j + 1 < slice.len() {
                        if slice[j] == b']' && slice[j + 1] == b']' {
                            j += 2;
                            closed = true;
                            break;
                        }
                        j += 1;
                    }
                    if !closed {
                        // Unterminated `--[[ ...`: treat the rest of the
                        // slice as comment and stop. Without this break,
                        // the outer loop would re-enter with
                        // `i = slice.len() - 1`, and if that last byte is
                        // a top-level `,` the comma count gets bumped by
                        // one (P0-R2 regression).
                        break;
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
    let (label, offsets) = sig.display_label_with_offsets(name, is_method);
    let params = offsets
        .into_iter()
        .map(|[start, end]| ParameterInformation {
            label: ParameterLabel::LabelOffsets([start, end]),
            documentation: None,
        })
        .collect();
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
        assert_eq!(
            count_top_level_commas(b"a, b, {x=1, y=2}, c"),
            3,
            "3 top-level commas (the ones inside {{ ... }} must not count)",
        );
    }

    #[test]
    fn unterminated_block_comment_does_not_miscount_trailing_comma() {
        // P0-R2 regression: the slice `1, --[[unclosed,` has one top-level
        // `,` (after `1`) and one comma sitting inside an unterminated
        // `--[[` block comment. The bug was that after the inner `j` loop
        // failed to find `]]`, `j` ended at `slice.len() - 1`; setting
        // `i = j; continue;` let the outer loop re-enter and process the
        // final byte — if that byte was the top-level `,` we'd count it
        // twice. The fix breaks out of the outer loop once `--[[` is
        // detected as unterminated.
        assert_eq!(
            count_top_level_commas(b"1, --[[unclosed,"),
            1,
            "trailing `,` inside unterminated --[[ must not count",
        );

        // Companion case: slice where the unterminated `--[[` sits at
        // the very end with no comma; still must not panic and must
        // return only the commas counted before the `--[[`.
        assert_eq!(
            count_top_level_commas(b"1, 2, --[[unclosed"),
            2,
        );

        // Properly-terminated block comment should not drop the
        // top-level `,` that follows it.
        assert_eq!(
            count_top_level_commas(b"1, --[[note]], 3"),
            2,
            "terminated block comment preserves outer top-level commas",
        );
    }
}
