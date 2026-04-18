use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use tower_lsp_server::ls_types::Uri;

use crate::emmy::{collect_preceding_comments, parse_emmy_comments, emmy_type_to_fact, parse_type_from_str, EmmyAnnotation, EmmyType};
use crate::summary::*;
use crate::table_shape::{FieldInfo, TableShape, TableShapeId, MAX_TABLE_SHAPE_DEPTH};
use crate::type_system::*;
use crate::util::{node_text, ts_node_to_range};

/// Build a `DocumentSummary` from a parsed AST.
///
/// This is the core of single-file inference (index-architecture.md §3).
/// Zero cross-file dependencies: all unresolved references become `SymbolicStub`s.
pub fn build_summary(uri: &Uri, tree: &tree_sitter::Tree, source: &[u8]) -> DocumentSummary {
    let mut ctx = BuildContext {
        source,
        require_bindings: Vec::new(),
        global_contributions: Vec::new(),
        function_summaries: HashMap::new(),
        type_definitions: Vec::new(),
        local_type_facts: HashMap::new(),
        table_shapes: HashMap::new(),
        next_shape_id: 0,
        pending_type_annotation: None,
        pending_class: None,
        pending_generic_params: Vec::new(),
        module_return_type: None,
        module_return_range: None,
    };

    let root = tree.root_node();
    visit_top_level(&mut ctx, root);

    let content_hash = hash_bytes(source);
    let signature_fingerprint = compute_signature_fingerprint(&ctx);
    let call_sites = collect_call_sites(root, source);
    let (is_meta, meta_name) = detect_meta_annotation(root, source);

    DocumentSummary {
        uri: uri.clone(),
        content_hash,
        require_bindings: ctx.require_bindings,
        global_contributions: ctx.global_contributions,
        function_summaries: ctx.function_summaries,
        type_definitions: ctx.type_definitions,
        local_type_facts: ctx.local_type_facts,
        table_shapes: ctx.table_shapes,
        module_return_type: ctx.module_return_type,
        module_return_range: ctx.module_return_range,
        signature_fingerprint,
        call_sites,
        is_meta,
        meta_name,
    }
}

/// Scan the first few top-level statements for a `---@meta [name]`
/// annotation. Following Lua-LS convention the directive lives at
/// the top of the file; we allow it to appear after a shebang or
/// initial comments but stop looking once any real statement
/// (`local_declaration` / `function_declaration` / `assignment_statement`
/// / `return_statement`) precedes it, since `---@meta` placed after
/// runtime code is almost certainly an authoring mistake.
fn detect_meta_annotation(root: tree_sitter::Node, source: &[u8]) -> (bool, Option<String>) {
    for i in 0..root.named_child_count() {
        let Some(child) = root.named_child(i as u32) else { continue };
        match child.kind() {
            "emmy_comment" => {
                for j in 0..child.named_child_count() {
                    let Some(line) = child.named_child(j as u32) else { continue };
                    if line.kind() != "emmy_line" {
                        continue;
                    }
                    let text = node_text(line, source);
                    let anns = parse_emmy_comments(text);
                    for ann in anns {
                        if let EmmyAnnotation::Meta { name } = ann {
                            return (true, name);
                        }
                    }
                }
            }
            // Any non-emmy sibling that represents real code tells us
            // there's no leading `---@meta`.
            "local_declaration"
            | "local_function_declaration"
            | "function_declaration"
            | "assignment_statement"
            | "return_statement" => return (false, None),
            _ => continue,
        }
    }
    (false, None)
}

/// Second single-pass over the AST to collect `CallSite` records
/// scoped to their enclosing function. Uses its own walk (rather
/// than threading through the main visitor) because the main
/// visitor is already cluttered with type-inference state and the
/// call-site concern is mostly independent.
fn collect_call_sites(root: tree_sitter::Node, source: &[u8]) -> Vec<crate::summary::CallSite> {
    let mut out = Vec::new();
    collect_calls_in_scope(root, source, "", &mut out);
    out
}

/// Walk `node` emitting every `function_call` encountered, tagging
/// its enclosing function name via `caller_name`. Entering a nested
/// function updates `caller_name` for the subtree.
fn collect_calls_in_scope(
    node: tree_sitter::Node,
    source: &[u8],
    caller_name: &str,
    out: &mut Vec<crate::summary::CallSite>,
) {
    match node.kind() {
        "function_declaration" | "local_function_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source).to_string())
                .unwrap_or_default();
            if let Some(body) = node.child_by_field_name("body") {
                collect_calls_in_scope(body, source, &name, out);
            }
            return;
        }
        "function_definition" => {
            // Anonymous function — caller_name for its body is the
            // binding anchor if we can identify one, else inherit
            // from the outer scope. We only set a meaningful name
            // when the function expression is the direct RHS of a
            // local_declaration / assignment_statement with a
            // simple bare-identifier LHS; this matches what
            // `enclosing_statement_for_function_expr` already
            // produces.
            let inferred = infer_anon_caller_name(node, source);
            let sub_caller = inferred.as_deref().unwrap_or(caller_name);
            if let Some(body) = node.child_by_field_name("body") {
                collect_calls_in_scope(body, source, sub_caller, out);
            }
            return;
        }
        "function_call" => {
            if let Some(cs) = crate::call_hierarchy::extract_call_site(node, source, caller_name) {
                out.push(cs);
            }
            // Still recurse — arguments may contain nested calls
            // (e.g. `foo(bar(1))`) whose callee we also want to
            // record, with the same caller context.
        }
        _ => {}
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_calls_in_scope(child, source, caller_name, out);
        }
    }
}

/// Best-effort: given a `function_definition` node, return the
/// binding name if it sits at the RHS of a simple `local f = function()` /
/// `Foo.m = function()` / `Foo.m = function() end` assignment. Dotted
/// LHS are preserved verbatim; `obj:m` wrappers produced implicitly
/// (rare for `function_definition`) are kept as-is.
fn infer_anon_caller_name(
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<String> {
    let stmt = enclosing_statement_for_function_expr(node)?;
    match stmt.kind() {
        "local_declaration" => {
            let names = stmt.child_by_field_name("names")?;
            let id = names.named_child(0)?;
            if id.kind() == "identifier" {
                Some(node_text(id, source).to_string())
            } else {
                None
            }
        }
        "assignment_statement" => {
            let left = stmt.child_by_field_name("left")?;
            let first = left.named_child(0)?;
            // Return the full LHS text (supports `Foo.m` / `m[1]` / bare id).
            Some(node_text(first, source).to_string())
        }
        _ => None,
    }
}

struct BuildContext<'a> {
    source: &'a [u8],
    require_bindings: Vec<RequireBinding>,
    global_contributions: Vec<GlobalContribution>,
    function_summaries: HashMap<String, FunctionSummary>,
    type_definitions: Vec<TypeDefinition>,
    local_type_facts: HashMap<String, LocalTypeFact>,
    table_shapes: HashMap<TableShapeId, TableShape>,
    next_shape_id: u32,
    /// `---@type X` annotation pending attachment to the next local declaration.
    pending_type_annotation: Option<EmmyType>,
    /// Class being built across consecutive emmy_comment nodes:
    /// (name, parents, fields, generic_params, name_range of the
    /// `---@class <Name>` identifier token).
    pending_class: Option<(
        String,
        Vec<String>,
        Vec<TypeFieldDef>,
        Vec<String>,
        tower_lsp_server::ls_types::Range,
    )>,
    /// Buffer for `@generic` params that arrive before `@class`.
    pending_generic_params: Vec<String>,
    /// Type of the file-level `return` statement (module export).
    module_return_type: Option<TypeFact>,
    /// Source range of the file-level `return` statement.
    module_return_range: Option<tower_lsp_server::ls_types::Range>,
}

impl<'a> BuildContext<'a> {
    fn alloc_shape_id(&mut self) -> TableShapeId {
        let id = TableShapeId(self.next_shape_id);
        self.next_shape_id += 1;
        id
    }

    fn take_pending_type(&mut self) -> Option<EmmyType> {
        self.pending_type_annotation.take()
    }
}

// ---------------------------------------------------------------------------
// Top-level visitor
// ---------------------------------------------------------------------------

fn visit_top_level(ctx: &mut BuildContext, root: tree_sitter::Node) {
    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let node = cursor.node();
        match node.kind() {
            "local_declaration" => {
                flush_pending_class(ctx, node);
                visit_local_declaration(ctx, node);
            }
            "local_function_declaration" => {
                flush_pending_class(ctx, node);
                ctx.pending_type_annotation = None;
                visit_local_function(ctx, node);
            }
            "function_declaration" => {
                flush_pending_class(ctx, node);
                ctx.pending_type_annotation = None;
                visit_function_declaration(ctx, node);
            }
            "assignment_statement" => {
                flush_pending_class(ctx, node);
                visit_assignment(ctx, node);
            }
            "return_statement" => {
                flush_pending_class(ctx, node);
                ctx.pending_type_annotation = None;
                visit_module_return(ctx, node);
            }
            "emmy_comment" => visit_emmy_comment(ctx, node),
            "if_statement" | "do_statement" | "while_statement" | "repeat_statement"
            | "for_numeric_statement" | "for_generic_statement" => {
                flush_pending_class(ctx, node);
                ctx.pending_type_annotation = None;
                visit_nested_block(ctx, node);
            }
            _ => {
                flush_pending_class(ctx, node);
                ctx.pending_type_annotation = None;
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

fn visit_module_return(ctx: &mut BuildContext, node: tree_sitter::Node) {
    ctx.module_return_range = Some(ts_node_to_range(node, ctx.source));
    if let Some(values) = node.child_by_field_name("values") {
        if let Some(first_expr) = values.named_child(0) {
            let type_fact = infer_expression_type(ctx, first_expr, 0);
            ctx.module_return_type = Some(type_fact);
        }
    }
}

fn visit_nested_block(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "block" | "if_clause" | "elseif_clause" | "else_clause" => {
                visit_nested_block(ctx, child);
            }
            "function_declaration" => {
                visit_function_declaration(ctx, child);
            }
            "assignment_statement" => {
                visit_assignment(ctx, child);
            }
            "local_declaration" => {
                visit_local_declaration(ctx, child);
            }
            "local_function_declaration" => {
                visit_local_function(ctx, child);
            }
            "emmy_comment" => visit_emmy_comment(ctx, child),
            "if_statement" | "do_statement" | "while_statement" | "repeat_statement"
            | "for_numeric_statement" | "for_generic_statement" => {
                visit_nested_block(ctx, child);
            }
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// local declarations: `local x = require("mod")` / `local x = expr`
// ---------------------------------------------------------------------------

fn visit_local_declaration(ctx: &mut BuildContext, node: tree_sitter::Node) {
    // Check for preceding `---@type` annotation (either from pending or inline comment)
    let pending_type = ctx.take_pending_type().or_else(|| {
        extract_preceding_type_annotation(node, ctx.source)
    });
    let prev_kind = node.prev_sibling().map(|s| s.kind().to_string()).unwrap_or_default();
    lsp_log!("[summary] visit_local_declaration: pending_type={:?} prev_sibling_kind='{}'", pending_type, prev_kind);

    let names_node = match node.child_by_field_name("names") {
        Some(n) => n,
        None => return,
    };
    let values_node = node.child_by_field_name("values");

    let name_count = names_node.named_child_count();

    // Multi-return distribution: `local a, b, c = f()` where the RHS
    // has exactly one value expression that is a `function_call`.
    // When we can statically infer the callee's return signature, map
    // `returns[i]` to `names[i]`; for every name beyond the signature's
    // return count we leave `Unknown`.
    let multi_return_types: Option<Vec<TypeFact>> = if name_count > 1 {
        values_node
            .and_then(single_function_call_rhs)
            .and_then(|call| extract_call_return_types(ctx, call))
    } else {
        None
    };

    for i in 0..name_count {
        let name_node = match names_node.named_child(i as u32) {
            Some(n) if n.kind() == "identifier" => n,
            _ => continue,
        };
        let name = node_text(name_node, ctx.source).to_string();
        let range = ts_node_to_range(name_node, ctx.source);

        // If we have an explicit @type annotation, it takes priority
        if i == 0 {
            if let Some(ref type_expr) = pending_type {
                let type_fact = emmy_type_to_fact(type_expr);
                ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
                    name: name.clone(),
                    type_fact,
                    source: TypeFactSource::EmmyAnnotation,
                    range,
                });
                continue;
            }
        }

        // Multi-return path takes priority when in effect: distribute
        // the i-th return type from the single RHS call. Names beyond
        // the signature's return arity stay Unknown.
        if let Some(ref returns) = multi_return_types {
            let type_fact = returns.get(i).cloned().unwrap_or(TypeFact::Unknown);
            ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
                name: name.clone(),
                type_fact,
                source: TypeFactSource::Assignment,
                range,
            });
            continue;
        }

        let value_node = values_node
            .and_then(|v| v.named_child(i as u32));

        if let Some(val) = value_node {
            if let Some(rb) = try_extract_require(ctx, &name, val) {
                ctx.require_bindings.push(rb);
                ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
                    name: name.clone(),
                    type_fact: TypeFact::Stub(SymbolicStub::RequireRef {
                        module_path: ctx.require_bindings.last().unwrap().module_path.clone(),
                    }),
                    source: TypeFactSource::RequireBinding,
                    range,
                });
                continue;
            }

            let type_fact = infer_expression_type(ctx, val, 0);
            ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
                name: name.clone(),
                type_fact,
                source: TypeFactSource::Assignment,
                range,
            });
        }
    }
}

/// True iff `values_node` (the `expression_list` on the RHS of a
/// `local_declaration` / `assignment_statement`) holds exactly one
/// expression and that expression is a `function_call`. Returns the
/// call node when it qualifies.
fn single_function_call_rhs(values: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if values.named_child_count() != 1 {
        return None;
    }
    let only = values.named_child(0)?;
    if only.kind() == "function_call" {
        Some(only)
    } else {
        None
    }
}

/// Try to statically extract the full list of return types for a
/// `function_call` node. Returns `None` when the callee's signature
/// isn't reachable at summary-build time (e.g. cross-file require,
/// method calls, field expressions) — callers should fall back to
/// leaving each name `Unknown` in that case rather than guessing.
///
/// Only the cheap, deterministic paths are handled here; more
/// elaborate multi-return analysis (e.g. following require to another
/// file's `module_return_type` → resolving multiple returns) is
/// intentionally deferred to avoid ballooning the summary pass.
fn extract_call_return_types(
    ctx: &BuildContext,
    call_node: tree_sitter::Node,
) -> Option<Vec<TypeFact>> {
    // Grammar: `function_call` with a `method` field is `obj:m(...)`.
    // The `callee` child is just `obj` in that shape, so relying on
    // the callee kind alone would wrongly treat `obj:m()` like `obj()`
    // when `obj` is also a top-level function registered in
    // `function_summaries`. Mirror `infer_call_return_type`'s method
    // branch and bail out unambiguously.
    if call_node.child_by_field_name("method").is_some() {
        return None;
    }
    let callee = call_node.child_by_field_name("callee")?;
    // Only bare identifier callees are statically resolvable here;
    // dotted calls (`mod.f()`) and subscript calls fall through the
    // resolver as CallReturn stubs without direct access to the full
    // returns list.
    if !matches!(callee.kind(), "variable" | "identifier") {
        return None;
    }
    let callee_text = node_text(callee, ctx.source);

    // Same-file function summary (covers `local function`, `function`,
    // `function Class:m`, etc.).
    if let Some(fs) = ctx.function_summaries.get(callee_text) {
        return Some(fs.signature.returns.clone());
    }
    None
}

/// Extract `---@type X` from a comment node immediately preceding the given node.
fn extract_preceding_type_annotation(node: tree_sitter::Node, source: &[u8]) -> Option<EmmyType> {
    let prev = node.prev_sibling()?;
    match prev.kind() {
        "emmy_comment" => {
            for i in 0..prev.named_child_count() {
                if let Some(line_node) = prev.named_child(i as u32) {
                    if line_node.kind() == "emmy_line" {
                        let text = node_text(line_node, source).trim();
                        if let Some(rest) = text.strip_prefix("---") {
                            let rest = rest.trim();
                            if let Some(rest) = rest.strip_prefix("@type") {
                                let type_text = rest.trim();
                                if !type_text.is_empty() {
                                    return Some(parse_type_from_str(type_text));
                                }
                            }
                        }
                    }
                }
            }
            None
        }
        "comment" => {
            let text = node_text(prev, source).trim();
            if let Some(rest) = text.strip_prefix("---@type") {
                let type_text = rest.trim();
                if !type_text.is_empty() {
                    return Some(parse_type_from_str(type_text));
                }
            }
            None
        }
        _ => None,
    }
}

fn try_extract_require<'a>(
    ctx: &BuildContext<'a>,
    local_name: &str,
    value_node: tree_sitter::Node<'a>,
) -> Option<RequireBinding> {
    if value_node.kind() != "function_call" {
        return None;
    }
    let callee = value_node.child_by_field_name("callee")?;
    if node_text(callee, ctx.source) != "require" {
        return None;
    }
    let args = value_node.child_by_field_name("arguments")?;
    let first_arg = args.named_child(0)?;
    let module_path = extract_string_literal(ctx, first_arg)?;

    Some(RequireBinding {
        local_name: local_name.to_string(),
        module_path,
        range: ts_node_to_range(value_node, ctx.source),
    })
}

fn extract_string_literal(ctx: &BuildContext, node: tree_sitter::Node) -> Option<String> {
    fn find_content(n: tree_sitter::Node, source: &[u8]) -> Option<String> {
        if n.kind().starts_with("short_string_content") {
            return Some(node_text(n, source).to_string());
        }
        for i in 0..n.named_child_count() {
            if let Some(child) = n.named_child(i as u32) {
                if let Some(s) = find_content(child, source) {
                    return Some(s);
                }
            }
        }
        None
    }

    if node.kind() == "expression_list" {
        if let Some(first) = node.named_child(0) {
            return find_content(first, ctx.source);
        }
    }
    find_content(node, ctx.source)
}

// ---------------------------------------------------------------------------
// Function declarations
// ---------------------------------------------------------------------------

fn visit_local_function(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, ctx.source).to_string();
    let body = node.child_by_field_name("body");

    let fs = build_function_summary(ctx, &name, node, body);
    ctx.function_summaries.insert(name, fs);
}

fn visit_function_declaration(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, ctx.source).to_string();
    let body = node.child_by_field_name("body");

    let fs = build_function_summary(ctx, &name, node, body);
    let sig_for_global = fs.signature.clone();
    ctx.function_summaries.insert(name.clone(), fs);

    ctx.global_contributions.push(GlobalContribution {
        name: name.clone(),
        kind: GlobalContributionKind::Function,
        type_fact: TypeFact::Known(KnownType::Function(sig_for_global)),
        range: ts_node_to_range(node, ctx.source),
        selection_range: ts_node_to_range(name_node, ctx.source),
    });
}

fn build_function_summary(
    ctx: &mut BuildContext,
    name: &str,
    decl_node: tree_sitter::Node,
    body: Option<tree_sitter::Node>,
) -> FunctionSummary {
    let emmy_comments = collect_preceding_comments(decl_node, ctx.source);
    let emmy_text = emmy_comments.join("\n");
    let annotations = parse_emmy_comments(&emmy_text);

    let mut params = Vec::new();
    let mut returns = Vec::new();
    let mut emmy_annotated = false;
    let mut overloads = Vec::new();

    for ann in &annotations {
        match ann {
            EmmyAnnotation::Param { name: pname, type_expr, .. } => {
                emmy_annotated = true;
                params.push(ParamInfo {
                    name: pname.clone(),
                    type_fact: emmy_type_to_fact(type_expr),
                });
            }
            EmmyAnnotation::Return { return_types, .. } => {
                emmy_annotated = true;
                for rt in return_types {
                    returns.push(emmy_type_to_fact(rt));
                }
            }
            EmmyAnnotation::Overload { fun_type } => {
                if let TypeFact::Known(KnownType::Function(sig)) = emmy_type_to_fact(fun_type) {
                    overloads.push(sig);
                }
            }
            _ => {}
        }
    }

    // If no Emmy params, extract from AST
    if params.is_empty() {
        if let Some(b) = body {
            if let Some(param_list) = b.child_by_field_name("parameters") {
                extract_ast_params(&mut params, param_list, ctx.source);
            }
        }
    }

    // If no Emmy return, try to infer from return statements
    if returns.is_empty() && !emmy_annotated {
        if let Some(b) = body {
            collect_return_types(ctx, b, &mut returns, 0);
        }
    }

    let sig = FunctionSignature {
        params: params.clone(),
        returns: returns.clone(),
    };
    let fingerprint = hash_function_signature(&sig);

    FunctionSummary {
        name: name.to_string(),
        signature: sig,
        range: ts_node_to_range(decl_node, ctx.source),
        signature_fingerprint: fingerprint,
        emmy_annotated,
        overloads,
    }
}

/// Return the enclosing `local_declaration` / `assignment_statement`
/// iff the `function_definition` at `node` is a **direct** RHS value
/// (i.e. the function expression is the sole content of a
/// value-list slot on an `expression_list` directly under such a
/// statement). Returns `None` for every nested / indirect case — table
/// entries (`{ m = function() end }`), function arguments
/// (`foo(function() end)`), IIFE wrappers
/// (`(function() end)()` — the RHS is the call, not the function
/// expression itself), arithmetic (`x = 1 + function() end()`),
/// nested function bodies, etc. — so we never pick up unrelated
/// outer Emmy annotations.
pub(crate) fn enclosing_statement_for_function_expr(
    node: tree_sitter::Node,
) -> Option<tree_sitter::Node> {
    // The parent of a direct-bound RHS function is always
    // `expression_list`; any other immediate parent means we're
    // inside a call / wrapper / table / nested context.
    let parent = node.parent()?;
    if parent.kind() != "expression_list" {
        return None;
    }
    let grandparent = parent.parent()?;
    match grandparent.kind() {
        "local_declaration" | "assignment_statement" => Some(grandparent),
        _ => None,
    }
}

fn extract_ast_params(params: &mut Vec<ParamInfo>, param_list: tree_sitter::Node, source: &[u8]) {
    // Walk ALL children (named + unnamed) so we can pick up the
    // anonymous `...` token too: the grammar's `_parameter_list_content`
    // is inlined (leading `_`) and does NOT give the ellipsis its own
    // node kind, so `named_child_count` alone would silently drop
    // vararg params. Signal it by pushing a `ParamInfo { name: "...", .. }`.
    for i in 0..param_list.child_count() {
        let Some(child) = param_list.child(i as u32) else { continue };
        match child.kind() {
            "identifier" => {
                params.push(ParamInfo {
                    name: node_text(child, source).to_string(),
                    type_fact: TypeFact::Unknown,
                });
            }
            "name_list" => {
                for j in 0..child.named_child_count() {
                    if let Some(id) = child.named_child(j as u32) {
                        if id.kind() == "identifier" {
                            params.push(ParamInfo {
                                name: node_text(id, source).to_string(),
                                type_fact: TypeFact::Unknown,
                            });
                        }
                    }
                }
            }
            // Legacy explicit node name (if the grammar ever exposes
            // vararg as a named node again) or anonymous `...` token.
            "varargs" => {
                params.push(ParamInfo {
                    name: "...".to_string(),
                    type_fact: TypeFact::Unknown,
                });
            }
            _ => {
                // Anonymous `...` token in `function f(a, ...)`:
                // tree-sitter exposes it as an unnamed child whose
                // literal text is `...`.
                if !child.is_named() && node_text(child, source) == "..." {
                    params.push(ParamInfo {
                        name: "...".to_string(),
                        type_fact: TypeFact::Unknown,
                    });
                }
            }
        }
    }
}

fn collect_return_types(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
    returns: &mut Vec<TypeFact>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "return_statement" => {
                if let Some(values) = child.child_by_field_name("values") {
                    for i in 0..values.named_child_count() {
                        if let Some(expr) = values.named_child(i as u32) {
                            let tf = infer_expression_type(ctx, expr, 0);
                            if returns.len() <= i {
                                returns.push(tf);
                            } else {
                                // Merge with existing (union)
                                let existing = returns[i].clone();
                                returns[i] = merge_types(existing, tf);
                            }
                        }
                    }
                }
            }
            // Don't recurse into nested function bodies
            "function_body" | "function_declaration" | "local_function_declaration" => {}
            _ => {
                collect_return_types(ctx, child, returns, depth + 1);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Assignment statements (globals)
// ---------------------------------------------------------------------------

fn visit_assignment(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let pending_type = ctx.take_pending_type().or_else(|| {
        extract_preceding_type_annotation(node, ctx.source)
    });

    let left = match node.child_by_field_name("left") {
        Some(n) => n,
        None => return,
    };
    let right = node.child_by_field_name("right");

    for i in 0..left.named_child_count() {
        let var_node = match left.named_child(i as u32) {
            Some(n) => n,
            None => continue,
        };

        let value_node = right.and_then(|r| r.named_child(i as u32));

        match var_node.kind() {
            // Simple global: `foo = expr`
            "variable" if var_node.child_count() == 1 => {
                let name = node_text(var_node, ctx.source).to_string();
                let type_fact = if i == 0 {
                    if let Some(ref type_expr) = pending_type {
                        emmy_type_to_fact(type_expr)
                    } else {
                        value_node
                            .map(|v| infer_expression_type(ctx, v, 0))
                            .unwrap_or(TypeFact::Unknown)
                    }
                } else {
                    value_node
                        .map(|v| infer_expression_type(ctx, v, 0))
                        .unwrap_or(TypeFact::Unknown)
                };

                ctx.global_contributions.push(GlobalContribution {
                    name,
                    kind: GlobalContributionKind::Variable,
                    type_fact,
                    range: ts_node_to_range(node, ctx.source),
                    selection_range: ts_node_to_range(var_node, ctx.source),
                });
            }
            // Field assignment: `x.foo = expr` or `a.b.c = expr` etc.
            //
            // Strategy (AST-driven, not string splitn):
            //   1. Walk the nested `variable(object, field)` chain to collect
            //      field names outermost→innermost and reach the innermost
            //      bare identifier.
            //   2. If any intermediate node is not a pure dotted `variable`
            //      (e.g. `function_call`, subscript, parenthesized) → bail:
            //      the LHS targets a transient value that can't be named
            //      again. No shape write, no global contribution.
            //   3. Pure dotted chain:
            //      - Base is a local with Table shape → walk/create nested
            //        shapes for intermediate fields, then `set_field` on the
            //        innermost. Intermediate shape creation is on-demand.
            //      - Otherwise → legacy global TableExtension with the full
            //        dotted path as name.
            "field_expression" | "variable" => {
                let chain = match extract_dotted_chain(var_node, ctx.source) {
                    Some(c) if !c.fields.is_empty() => c,
                    _ => continue,
                };

                let type_fact = if i == 0 {
                    if let Some(ref type_expr) = pending_type {
                        emmy_type_to_fact(type_expr)
                    } else {
                        value_node
                            .map(|v| infer_expression_type(ctx, v, 0))
                            .unwrap_or(TypeFact::Unknown)
                    }
                } else {
                    value_node
                        .map(|v| infer_expression_type(ctx, v, 0))
                        .unwrap_or(TypeFact::Unknown)
                };

                let assign_range = ts_node_to_range(node, ctx.source);
                if register_nested_field_write(
                    ctx,
                    &chain.base_name,
                    &chain.fields,
                    type_fact.clone(),
                    assign_range,
                ) {
                    continue;
                }

                // `register_nested_field_write` returned `false` for one
                // of two reasons:
                //   (α) base isn't a local OR isn't a local with a Table
                //       shape → legacy TableExtension is appropriate
                //       (e.g. `GlobalTable.foo = 1`).
                //   (β) base IS a local Table shape but an intermediate
                //       field carries a non-Table type (e.g.
                //       `local a = {}; a.b = 1; a.b.c = 2`) → bail
                //       silently; writing to a non-Table via `.c` is
                //       a likely user bug and we MUST NOT surface the
                //       junk path through `global_shard` (the local `a`
                //       is not a global).
                if ctx.local_type_facts.contains_key(&chain.base_name) {
                    continue;
                }

                let name = chain.joined();
                ctx.global_contributions.push(GlobalContribution {
                    name,
                    kind: GlobalContributionKind::TableExtension,
                    type_fact,
                    range: ts_node_to_range(node, ctx.source),
                    selection_range: ts_node_to_range(var_node, ctx.source),
                });
            }
            // Bracket index: `t[expr] = value` — mark shape open if key is dynamic
            "subscript_expression" => {
                if let Some(base) = var_node.child_by_field_name("object") {
                    let base_text = node_text(base, ctx.source);
                    if let Some(ltf) = ctx.local_type_facts.get(base_text) {
                        if let TypeFact::Known(KnownType::Table(shape_id)) = &ltf.type_fact {
                            let sid = *shape_id;
                            if let Some(shape) = ctx.table_shapes.get_mut(&sid) {
                                shape.mark_open();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Dotted LHS helpers — AST-driven chain extraction & nested shape registration
// ---------------------------------------------------------------------------

/// Decomposition of a pure dotted LHS like `a.b.c` into the bare base name
/// and an ordered list of field names. Returned by `extract_dotted_chain`
/// only when the entire chain is made of `variable` nodes with an
/// `object + field` pair (outer levels) plus a single bare `identifier`
/// at the root. Any intermediate `function_call` / subscript /
/// `parenthesized_expression` → `None` (caller should bail on shape writes).
struct DottedChain {
    base_name: String,
    /// Field names in write order: for `a.b.c` this is `["b", "c"]`.
    fields: Vec<String>,
}

impl DottedChain {
    /// Full dotted path — `"a.b.c"`.
    fn joined(&self) -> String {
        let mut s = self.base_name.clone();
        for f in &self.fields {
            s.push('.');
            s.push_str(f);
        }
        s
    }
}

fn extract_dotted_chain(node: tree_sitter::Node, source: &[u8]) -> Option<DottedChain> {
    // Walk down the `object` chain, collecting field names. Every
    // intermediate node must itself be a `variable` (or legacy
    // `field_expression`) with an `object` + `field` pair. The innermost
    // `object` must be a `variable` whose only named child is a bare
    // `identifier` — i.e. the chain roots at a plain local/global name.
    //
    // Rejected (return None):
    //   - `foo().c`     → intermediate `function_call`
    //   - `a[1].c`      → `variable` whose `object` has an `index` field
    //                     instead of `field`
    //   - `(x).c`       → `parenthesized_expression` intermediate
    //   - `a:m().c`     → grammar wraps the method call as `function_call`
    let mut fields_rev: Vec<String> = Vec::new();
    let mut current = node;
    loop {
        if !matches!(current.kind(), "variable" | "field_expression") {
            return None;
        }
        let field = match current.child_by_field_name("field") {
            Some(f) => f,
            None => {
                // Innermost: a `variable` with a single `identifier` child.
                if current.named_child_count() == 1 {
                    let child = current.named_child(0)?;
                    if child.kind() == "identifier" {
                        let base_name = node_text(child, source).to_string();
                        fields_rev.reverse();
                        return Some(DottedChain {
                            base_name,
                            fields: fields_rev,
                        });
                    }
                }
                return None;
            }
        };
        let object = current.child_by_field_name("object")?;
        fields_rev.push(node_text(field, source).to_string());
        current = object;
    }
}

/// Register a field write `base.f1.f2...fn = value` against the local's
/// table shape, creating intermediate nested shapes on demand. Returns
/// `true` if the write was registered (the caller should skip legacy
/// global_contribution emission), `false` if the base isn't a local with
/// a Table shape (caller should fall through to legacy TableExtension).
fn register_nested_field_write(
    ctx: &mut BuildContext,
    base_name: &str,
    fields: &[String],
    type_fact: TypeFact,
    assign_range: tower_lsp_server::ls_types::Range,
) -> bool {
    let base_shape_id = match ctx.local_type_facts.get(base_name) {
        Some(ltf) => match &ltf.type_fact {
            TypeFact::Known(KnownType::Table(sid)) => *sid,
            _ => return false,
        },
        None => return false,
    };

    // Walk the intermediate shapes. Three cases per step:
    //   (a) field exists as `Known(Table(sid))` → reuse existing shape.
    //   (b) field missing entirely → alloc a fresh shape + register it as
    //       the field's type on the parent (on-demand nesting).
    //   (c) field exists but is NOT a Table (e.g. `a.b = 1` then
    //       `a.b.c = 2`) → bail. Silently overwriting `a.b`'s number
    //       with a new Table shape would hide a likely user bug and
    //       mislead downstream hover/sig-help.
    //
    // Invariant (no orphan shapes on bail): shapes allocated inside this
    // loop are freshly created and therefore have empty `fields`, so
    // subsequent iterations can only observe `None` and keep allocating.
    // A `Some(non-Table) → return false` bail can therefore only trigger
    // on a pre-existing shape encountered *before* any allocation in
    // this call — guaranteeing we never leave an unreferenced shape
    // behind when we return `false`.
    let mut current_shape = base_shape_id;
    let last_idx = fields.len().saturating_sub(1);
    for (i, field_name) in fields.iter().enumerate() {
        if i == last_idx {
            break;
        }
        let existing_field = ctx.table_shapes.get(&current_shape)
            .and_then(|s| s.fields.get(field_name))
            .map(|fi| fi.type_fact.clone());
        let next_shape = match existing_field {
            Some(TypeFact::Known(KnownType::Table(sid))) => sid,
            Some(_) => return false,
            None => {
                let new_id = ctx.alloc_shape_id();
                ctx.table_shapes.insert(new_id, TableShape::new(new_id));
                if let Some(parent) = ctx.table_shapes.get_mut(&current_shape) {
                    parent.set_field(field_name.clone(), FieldInfo {
                        name: field_name.clone(),
                        type_fact: TypeFact::Known(KnownType::Table(new_id)),
                        def_range: Some(assign_range),
                        assignment_count: 1,
                    });
                }
                new_id
            }
        };
        current_shape = next_shape;
    }

    let final_field = match fields.last() {
        Some(f) => f.clone(),
        None => return false,
    };
    if let Some(shape) = ctx.table_shapes.get_mut(&current_shape) {
        shape.set_field(final_field.clone(), FieldInfo {
            name: final_field,
            type_fact,
            def_range: Some(assign_range),
            assignment_count: 1,
        });
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Emmy comments (type definitions)
// ---------------------------------------------------------------------------

/// Flush any pending @class definition into type_definitions.
/// Called when a non-emmy_comment node is encountered.
fn flush_pending_class(ctx: &mut BuildContext, node: tree_sitter::Node) {
    if let Some((cname, parents, fields, generic_params, name_range)) = ctx.pending_class.take() {
        lsp_log!("[flush_class] '{}' with {} fields: {:?}", cname, fields.len(), fields.iter().map(|f| &f.name).collect::<Vec<_>>());
        ctx.type_definitions.push(TypeDefinition {
            name: cname,
            kind: TypeDefinitionKind::Class,
            parents,
            fields,
            alias_type: None,
            generic_params,
            range: ts_node_to_range(node, ctx.source),
            name_range: Some(name_range),
        });
    }
}

fn emit_pending_class_as_typedef(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
) {
    if let Some((cname, prev_parents, fields, gparams, name_range)) = ctx.pending_class.take() {
        ctx.type_definitions.push(TypeDefinition {
            name: cname,
            kind: TypeDefinitionKind::Class,
            parents: prev_parents,
            fields,
            alias_type: None,
            generic_params: gparams,
            range: ts_node_to_range(node, ctx.source),
            name_range: Some(name_range),
        });
    }
}

fn visit_emmy_comment(ctx: &mut BuildContext, node: tree_sitter::Node) {
    // Walk emmy_line children individually so each annotation can be
    // paired with its originating source range. `parse_emmy_comments`
    // returns at most one annotation per line (it tokenizes after the
    // leading `---`/`--` prefix), so `Vec::first()` suffices per line.
    // A name_range is computed per line by locating the identifier
    // token within the raw line text.
    for i in 0..node.named_child_count() {
        let Some(line_node) = node.named_child(i as u32) else { continue };
        if line_node.kind() != "emmy_line" {
            continue;
        }
        let line_text = node_text(line_node, ctx.source).to_string();
        let anns = parse_emmy_comments(&line_text);
        let Some(ann) = anns.first() else { continue };

        let line_start_byte = line_node.start_byte();
        let line_end_byte = line_node.end_byte();

        match ann {
            EmmyAnnotation::Class { name, parents, .. } => {
                emit_pending_class_as_typedef(ctx, node);
                let initial_gparams = std::mem::take(&mut ctx.pending_generic_params);
                let name_range = find_name_range_in_line(
                    ctx.source,
                    line_start_byte,
                    line_end_byte,
                    &line_text,
                    name,
                    "class",
                );
                ctx.pending_class = Some((
                    name.clone(),
                    parents.clone(),
                    Vec::new(),
                    initial_gparams,
                    name_range,
                ));
            }
            EmmyAnnotation::Generic { params } => {
                if let Some((_, _, _, ref mut gparams, _)) = ctx.pending_class {
                    for gp in params {
                        gparams.push(gp.name.clone());
                    }
                } else {
                    for gp in params {
                        ctx.pending_generic_params.push(gp.name.clone());
                    }
                }
            }
            EmmyAnnotation::Field { name: fname, type_expr, .. } => {
                if let Some((_, _, ref mut fields, _, _)) = ctx.pending_class {
                    let full_range = ts_node_to_range(line_node, ctx.source);
                    let name_range = find_name_range_in_line(
                        ctx.source,
                        line_start_byte,
                        line_end_byte,
                        &line_text,
                        fname,
                        "field",
                    );
                    fields.push(TypeFieldDef {
                        name: fname.clone(),
                        type_fact: emmy_type_to_fact(type_expr),
                        range: full_range,
                        name_range: Some(name_range),
                    });
                }
            }
            EmmyAnnotation::Type { type_expr, .. } => {
                if ctx.pending_class.is_none() {
                    ctx.pending_type_annotation = Some(type_expr.clone());
                }
            }
            EmmyAnnotation::Alias { name, type_expr } => {
                emit_pending_class_as_typedef(ctx, node);
                let name_range = find_name_range_in_line(
                    ctx.source,
                    line_start_byte,
                    line_end_byte,
                    &line_text,
                    name,
                    "alias",
                );
                ctx.type_definitions.push(TypeDefinition {
                    name: name.clone(),
                    kind: TypeDefinitionKind::Alias,
                    parents: Vec::new(),
                    fields: Vec::new(),
                    alias_type: Some(emmy_type_to_fact(type_expr)),
                    generic_params: Vec::new(),
                    range: ts_node_to_range(node, ctx.source),
                    name_range: Some(name_range),
                });
            }
            EmmyAnnotation::Enum { name } => {
                emit_pending_class_as_typedef(ctx, node);
                let name_range = find_name_range_in_line(
                    ctx.source,
                    line_start_byte,
                    line_end_byte,
                    &line_text,
                    name,
                    "enum",
                );
                ctx.type_definitions.push(TypeDefinition {
                    name: name.clone(),
                    kind: TypeDefinitionKind::Enum,
                    parents: Vec::new(),
                    fields: Vec::new(),
                    alias_type: None,
                    generic_params: Vec::new(),
                    range: ts_node_to_range(node, ctx.source),
                    name_range: Some(name_range),
                });
            }
            _ => {}
        }
    }
}

/// Locate the byte range of the `<name>` token following a specific
/// annotation tag (`class`/`alias`/`enum`/`field`) inside an
/// `emmy_line`'s source text. Returns a `Range` anchored at the
/// original source file via `line_start_byte`.
///
/// Falls back to the full emmy_line range when the name cannot be
/// located (defensive; parser already validated the name exists).
///
/// For `@field`, the optional visibility keyword (`public` /
/// `private` / `protected` / `package`) is skipped before locating
/// the field name token so `---@field private name integer` resolves
/// to `name`, not `private`.
fn find_name_range_in_line(
    source: &[u8],
    line_start_byte: usize,
    line_end_byte: usize,
    line_text: &str,
    name: &str,
    tag: &str,
) -> tower_lsp_server::ls_types::Range {
    // Find the `@<tag>` occurrence; scan forward to the name token.
    let tag_marker = format!("@{}", tag);
    let Some(tag_pos) = line_text.find(&tag_marker) else {
        return byte_range_to_range(source, line_start_byte, line_end_byte);
    };
    // Byte cursor past the tag keyword.
    let mut cursor = tag_pos + tag_marker.len();
    let bytes = line_text.as_bytes();

    // Skip whitespace.
    while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
        cursor += 1;
    }

    // For @field: optionally skip a visibility keyword if what follows
    // is not the target `name`. We probe one identifier ahead.
    if tag == "field" {
        if let Some((word, next_cursor)) = read_identifier(bytes, cursor) {
            if word != name
                && matches!(word.as_str(), "public" | "private" | "protected" | "package")
            {
                cursor = next_cursor;
                while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
                    cursor += 1;
                }
            }
        }
    }

    // Now expect the `name` identifier at `cursor`.
    if cursor + name.len() <= bytes.len()
        && &bytes[cursor..cursor + name.len()] == name.as_bytes()
    {
        // Confirm word boundary on both sides to avoid partial matches.
        let before_ok = cursor == 0
            || !bytes[cursor - 1].is_ascii_alphanumeric() && bytes[cursor - 1] != b'_';
        let after_idx = cursor + name.len();
        let after_ok = after_idx >= bytes.len()
            || !(bytes[after_idx].is_ascii_alphanumeric() || bytes[after_idx] == b'_');
        if before_ok && after_ok {
            let start = line_start_byte + cursor;
            let end = start + name.len();
            return byte_range_to_range(source, start, end);
        }
    }
    byte_range_to_range(source, line_start_byte, line_end_byte)
}

/// Read an ASCII identifier starting at `start` within `bytes`.
/// Returns `(word, next_cursor)` past the identifier, or `None` when
/// no identifier is present at the cursor.
fn read_identifier(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut end = start;
    if end >= bytes.len() {
        return None;
    }
    if !(bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
        return None;
    }
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    let word = std::str::from_utf8(&bytes[start..end]).ok()?.to_string();
    Some((word, end))
}

/// Build an LSP `Range` from an absolute byte span, reusing the
/// file-level line/column computation via a tree-sitter Point-like
/// walk. Keeps UTF-16 column encoding consistent with the rest of
/// the crate.
fn byte_range_to_range(
    source: &[u8],
    start_byte: usize,
    end_byte: usize,
) -> tower_lsp_server::ls_types::Range {
    tower_lsp_server::ls_types::Range {
        start: byte_to_position(source, start_byte),
        end: byte_to_position(source, end_byte),
    }
}

fn byte_to_position(source: &[u8], target: usize) -> tower_lsp_server::ls_types::Position {
    let mut line: u32 = 0;
    let mut line_start: usize = 0;
    let mut i: usize = 0;
    while i < target && i < source.len() {
        if source[i] == b'\n' {
            line += 1;
            line_start = i + 1;
        }
        i += 1;
    }
    let col_bytes = target.min(source.len()) - line_start;
    let line_slice = &source[line_start..line_start + col_bytes];
    let character = crate::util::byte_col_to_utf16_col(line_slice, col_bytes);
    tower_lsp_server::ls_types::Position { line, character }
}

// ---------------------------------------------------------------------------
// Expression type inference (single-file)
// ---------------------------------------------------------------------------

fn infer_expression_type(ctx: &mut BuildContext, node: tree_sitter::Node, depth: usize) -> TypeFact {
    if depth > MAX_TABLE_SHAPE_DEPTH {
        return TypeFact::Unknown;
    }
    match node.kind() {
        "number" => TypeFact::Known(KnownType::Number),
        "string" => TypeFact::Known(KnownType::String),
        "true" | "false" => TypeFact::Known(KnownType::Boolean),
        "nil" => TypeFact::Known(KnownType::Nil),

        "table_constructor" => {
            let shape_id = ctx.alloc_shape_id();
            let mut shape = TableShape::new(shape_id);
            extract_table_shape(ctx, node, &mut shape, depth + 1);
            ctx.table_shapes.insert(shape_id, shape);
            TypeFact::Known(KnownType::Table(shape_id))
        }

        "function_definition" => {
            // Extract params from the `parameters` list on the
            // function_body child; fall through to Emmy-annotation
            // enrichment on the enclosing `local f = function(...)`
            // or `f = function(...)` statement so hover/signatureHelp
            // show a meaningful signature (rather than empty `fun()`).
            let mut params = Vec::new();
            let mut returns = Vec::new();
            let mut emmy_annotated = false;

            if let Some(body) = node.child_by_field_name("body") {
                if let Some(param_list) = body.child_by_field_name("parameters") {
                    extract_ast_params(&mut params, param_list, ctx.source);
                }
            }

            if let Some(stmt) = enclosing_statement_for_function_expr(node) {
                let emmy_comments = collect_preceding_comments(stmt, ctx.source);
                let emmy_text = emmy_comments.join("\n");
                for ann in parse_emmy_comments(&emmy_text) {
                    match ann {
                        EmmyAnnotation::Param { name: pname, type_expr, .. } => {
                            emmy_annotated = true;
                            let fact = emmy_type_to_fact(&type_expr);
                            // Only overwrite an AST-declared param of the
                            // same name. A typo'd `@param xyz` for a
                            // function declaring `a` must NOT append a
                            // phantom `xyz` parameter (keeps behavior in
                            // line with `build_function_summary`).
                            if let Some(p) = params.iter_mut().find(|p| p.name == pname) {
                                p.type_fact = fact;
                            }
                        }
                        EmmyAnnotation::Return { return_types, .. } => {
                            emmy_annotated = true;
                            for rt in return_types {
                                returns.push(emmy_type_to_fact(&rt));
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Any Emmy annotation (even a lone `@param`, no `@return`)
            // disables AST-derived return inference — matching
            // `build_function_summary` so users can explicitly opt into
            // "no return value" by writing e.g. `---@param x number`
            // without a `---@return`.
            if returns.is_empty() && !emmy_annotated {
                if let Some(body) = node.child_by_field_name("body") {
                    collect_return_types(ctx, body, &mut returns, 0);
                }
            }

            TypeFact::Known(KnownType::Function(FunctionSignature { params, returns }))
        }

        "function_call" => {
            infer_call_return_type(ctx, node)
        }

        "field_expression" => {
            infer_field_expression_type(ctx, node, depth)
        }

        "variable" | "identifier" => {
            let text = node_text(node, ctx.source);
            // Check if it's a known local
            if let Some(fact) = ctx.local_type_facts.get(text) {
                return fact.type_fact.clone();
            }
            // Otherwise it's a global reference stub
            TypeFact::Stub(SymbolicStub::GlobalRef {
                name: text.to_string(),
            })
        }

        "unary_expression" | "binary_expression" => {
            infer_operator_type(ctx, node, depth)
        }

        "parenthesized_expression" => {
            if let Some(inner) = node.named_child(0) {
                infer_expression_type(ctx, inner, depth)
            } else {
                TypeFact::Unknown
            }
        }

        _ => TypeFact::Unknown,
    }
}

fn infer_call_return_type(ctx: &BuildContext, node: tree_sitter::Node) -> TypeFact {
    let callee = match node.child_by_field_name("callee") {
        Some(c) => c,
        None => return TypeFact::Unknown,
    };

    let callee_text = node_text(callee, ctx.source);

    // `require("mod")` → RequireRef stub
    if callee_text == "require" {
        if let Some(args) = node.child_by_field_name("arguments") {
            if let Some(first_arg) = args.named_child(0) {
                if let Some(module_path) = extract_string_literal(ctx, first_arg) {
                    return TypeFact::Stub(SymbolicStub::RequireRef { module_path });
                }
            }
        }
        return TypeFact::Unknown;
    }

    // `obj:method()` → CallReturn(base_stub, method_name)
    if let Some(method_node) = node.child_by_field_name("method") {
        let method_name = node_text(method_node, ctx.source).to_string();
        let callee_text = node_text(callee, ctx.source);

        let base_stub = if let Some(fact) = ctx.local_type_facts.get(callee_text) {
            match &fact.type_fact {
                TypeFact::Stub(s) => s.clone(),
                TypeFact::Known(KnownType::EmmyType(type_name)) => {
                    SymbolicStub::TypeRef { name: type_name.clone() }
                }
                _ => SymbolicStub::GlobalRef { name: callee_text.to_string() },
            }
        } else {
            SymbolicStub::GlobalRef { name: callee_text.to_string() }
        };

        return TypeFact::Stub(SymbolicStub::CallReturn {
            base: Box::new(base_stub),
            func_name: method_name,
        });
    }

    // `mod.func()` → CallReturn(RequireRef/GlobalRef, func_name)
    // Current grammar wraps dotted access as a `variable` node with
    // `object` + `field` fields; `field_expression` is kept for forward
    // compatibility only.
    if matches!(callee.kind(), "variable" | "field_expression") {
        if let Some(base) = callee.child_by_field_name("object") {
            if let Some(field) = callee.child_by_field_name("field") {
                let base_text = node_text(base, ctx.source);
                let func_name = node_text(field, ctx.source).to_string();

                let base_stub = if let Some(fact) = ctx.local_type_facts.get(base_text) {
                    match &fact.type_fact {
                        TypeFact::Stub(s) => s.clone(),
                        TypeFact::Known(KnownType::EmmyType(type_name)) => {
                            SymbolicStub::TypeRef { name: type_name.clone() }
                        }
                        _ => SymbolicStub::GlobalRef { name: base_text.to_string() },
                    }
                } else {
                    SymbolicStub::GlobalRef { name: base_text.to_string() }
                };

                return TypeFact::Stub(SymbolicStub::CallReturn {
                    base: Box::new(base_stub),
                    func_name,
                });
            }
        }
    }

    // Simple local/global function call — check local function summaries
    if let Some(fs) = ctx.function_summaries.get(callee_text) {
        if let Some(ret) = fs.signature.returns.first() {
            return ret.clone();
        }
    }

    TypeFact::Unknown
}

fn infer_field_expression_type(
    ctx: &BuildContext,
    node: tree_sitter::Node,
    _depth: usize,
) -> TypeFact {
    let base = match node.child_by_field_name("object") {
        Some(b) => b,
        None => return TypeFact::Unknown,
    };
    let field = match node.child_by_field_name("field") {
        Some(f) => f,
        None => return TypeFact::Unknown,
    };

    let base_text = node_text(base, ctx.source);
    let field_name = node_text(field, ctx.source).to_string();

    // If base is a known local with a table shape, look up the field directly
    if let Some(fact) = ctx.local_type_facts.get(base_text) {
        if let TypeFact::Known(KnownType::Table(shape_id)) = &fact.type_fact {
            if let Some(shape) = ctx.table_shapes.get(shape_id) {
                if let Some(fi) = shape.fields.get(&field_name) {
                    return fi.type_fact.clone();
                }
            }
        }
        return TypeFact::Stub(SymbolicStub::FieldOf {
            base: Box::new(fact.type_fact.clone()),
            field: field_name,
        });
    }

    TypeFact::Stub(SymbolicStub::FieldOf {
        base: Box::new(TypeFact::Stub(SymbolicStub::GlobalRef {
            name: base_text.to_string(),
        })),
        field: field_name,
    })
}

fn infer_operator_type(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
    _depth: usize,
) -> TypeFact {
    if let Some(op_node) = node.child_by_field_name("operator") {
        let op = node_text(op_node, ctx.source);
        match op {
            "+" | "-" | "*" | "/" | "//" | "%" | "^" => {
                return TypeFact::Known(KnownType::Number);
            }
            ".." => {
                return TypeFact::Known(KnownType::String);
            }
            "==" | "~=" | "<" | "<=" | ">" | ">=" | "not" => {
                return TypeFact::Known(KnownType::Boolean);
            }
            "and" | "or" => {
                return TypeFact::Unknown;
            }
            _ => {}
        }
    }
    // Unary minus/length
    if node.kind() == "unary_expression" {
        if let Some(op_child) = node.child(0) {
            let op_text = node_text(op_child, ctx.source);
            match op_text {
                "-" => return TypeFact::Known(KnownType::Number),
                "#" => return TypeFact::Known(KnownType::Integer),
                "not" => return TypeFact::Known(KnownType::Boolean),
                _ => {}
            }
        }
    }
    TypeFact::Unknown
}

// ---------------------------------------------------------------------------
// Table shape extraction
// ---------------------------------------------------------------------------

fn extract_table_shape(
    ctx: &mut BuildContext,
    constructor: tree_sitter::Node,
    shape: &mut TableShape,
    depth: usize,
) {
    if depth > MAX_TABLE_SHAPE_DEPTH {
        shape.truncated = true;
        return;
    }

    let mut cursor = constructor.walk();
    if !cursor.goto_first_child() {
        return;
    }

    loop {
        let child = cursor.node();
        match child.kind() {
            "field" => {
                if let Some(key_node) = child.child_by_field_name("name") {
                    let key = node_text(key_node, ctx.source).to_string();
                    if let Some(val_node) = child.child_by_field_name("value") {
                        let type_fact = infer_expression_type(ctx, val_node, depth);
                        shape.set_field(key.clone(), FieldInfo {
                            name: key,
                            type_fact,
                            def_range: Some(ts_node_to_range(child, ctx.source)),
                            assignment_count: 1,
                        });
                    }
                } else if let Some(key_node) = child.child_by_field_name("key") {
                    // Bracket key: `[expr] = value`
                    let key_text = node_text(key_node, ctx.source);
                    let is_static = matches!(key_node.kind(), "string" | "number");
                    if is_static {
                        if let Some(val_node) = child.child_by_field_name("value") {
                            let type_fact = infer_expression_type(ctx, val_node, depth);
                            shape.set_field(key_text.to_string(), FieldInfo {
                                name: key_text.to_string(),
                                type_fact,
                                def_range: Some(ts_node_to_range(child, ctx.source)),
                                assignment_count: 1,
                            });
                        }
                    } else {
                        shape.mark_open();
                        if let Some(val_node) = child.child_by_field_name("value") {
                            let type_fact = infer_expression_type(ctx, val_node, depth);
                            shape.array_element_type = Some(
                                match shape.array_element_type.take() {
                                    Some(existing) => merge_types(existing, type_fact),
                                    None => type_fact,
                                }
                            );
                        }
                    }
                } else if let Some(val_node) = child.child_by_field_name("value") {
                    let type_fact = infer_expression_type(ctx, val_node, depth);
                    shape.array_element_type = Some(
                        match shape.array_element_type.take() {
                            Some(existing) => merge_types(existing, type_fact),
                            None => type_fact,
                        }
                    );
                }
            }
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Type merging
// ---------------------------------------------------------------------------

fn merge_types(a: TypeFact, b: TypeFact) -> TypeFact {
    if a == b {
        return a;
    }
    match (a, b) {
        (TypeFact::Unknown, other) | (other, TypeFact::Unknown) => other,
        (TypeFact::Union(mut items), other) => {
            if !items.contains(&other) {
                items.push(other);
            }
            TypeFact::Union(items)
        }
        (other, TypeFact::Union(mut items)) => {
            if !items.contains(&other) {
                items.insert(0, other);
            }
            TypeFact::Union(items)
        }
        (a, b) => TypeFact::Union(vec![a, b]),
    }
}

// ---------------------------------------------------------------------------
// Hashing / fingerprints
// ---------------------------------------------------------------------------

fn hash_bytes(data: &[u8]) -> u64 {
    crate::util::hash_bytes(data)
}

fn hash_function_signature(sig: &FunctionSignature) -> u64 {
    let mut hasher = DefaultHasher::new();
    for p in &sig.params {
        p.name.hash(&mut hasher);
        format!("{}", p.type_fact).hash(&mut hasher);
    }
    for r in &sig.returns {
        format!("{}", r).hash(&mut hasher);
    }
    hasher.finish()
}

fn compute_signature_fingerprint(ctx: &BuildContext) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Hash require bindings (affect cross-file resolution)
    let mut requires: Vec<_> = ctx.require_bindings.iter()
        .map(|r| (&r.local_name, &r.module_path))
        .collect();
    requires.sort();
    for (name, path) in &requires {
        name.hash(&mut hasher);
        path.hash(&mut hasher);
    }

    // Hash global contributions including their type facts
    let mut globals: Vec<_> = ctx.global_contributions.iter()
        .map(|g| (g.name.as_str(), format!("{}", g.type_fact)))
        .collect();
    globals.sort();
    for (name, type_str) in &globals {
        name.hash(&mut hasher);
        type_str.hash(&mut hasher);
    }

    // Hash function signatures
    let mut func_names: Vec<&str> = ctx.function_summaries.keys().map(|k| k.as_str()).collect();
    func_names.sort();
    for name in &func_names {
        name.hash(&mut hasher);
        if let Some(fs) = ctx.function_summaries.get(*name) {
            fs.signature_fingerprint.hash(&mut hasher);
        }
    }

    // Hash type definitions: kind, parents, alias, fields
    let mut type_defs: Vec<_> = ctx.type_definitions.iter()
        .map(|t| {
            let fields_str: String = t.fields.iter()
                .map(|f| format!("{}:{}", f.name, f.type_fact))
                .collect::<Vec<_>>()
                .join(",");
            let alias_str = t.alias_type.as_ref()
                .map(|a| format!("{}", a))
                .unwrap_or_default();
            let parents_str = t.parents.join(",");
            let kind_str = format!("{:?}", t.kind);
            (t.name.as_str(), kind_str, parents_str, alias_str, fields_str)
        })
        .collect();
    type_defs.sort();
    for (name, kind, parents, alias, fields) in &type_defs {
        name.hash(&mut hasher);
        kind.hash(&mut hasher);
        parents.hash(&mut hasher);
        alias.hash(&mut hasher);
        fields.hash(&mut hasher);
    }

    // Hash module return type
    if let Some(ref ret) = ctx.module_return_type {
        "module_return".hash(&mut hasher);
        format!("{}", ret).hash(&mut hasher);
    }

    hasher.finish()
}
