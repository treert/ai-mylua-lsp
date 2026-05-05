use crate::emmy::{collect_preceding_comments, parse_emmy_comments, emmy_type_to_fact, EmmyAnnotation};
use crate::lua_symbol::get_lua_symbol;
use crate::table_shape::{TableShape, MAX_TABLE_SHAPE_DEPTH};
use crate::type_system::*;
use crate::util::node_text;

use super::BuildContext;
use super::table_extract::{extract_table_shape, extract_string_from_node};
use super::visitors::{enclosing_statement_for_function_expr, extract_ast_params, collect_return_types};

// ---------------------------------------------------------------------------
// Expression type inference (single-file)
// ---------------------------------------------------------------------------

pub(super) fn infer_expression_type(ctx: &mut BuildContext, node: tree_sitter::Node, depth: usize) -> TypeFact {
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
                        EmmyAnnotation::Param { name: pname, optional, type_expr, .. } => {
                            emmy_annotated = true;
                            let fact = emmy_type_to_fact(&type_expr);
                            // Only overwrite an AST-declared param of the
                            // same name. A typo'd `@param xyz` for a
                            // function declaring `a` must NOT append a
                            // phantom `xyz` parameter (keeps behavior in
                            // line with `build_function_summary`).
                            if let Some(p) = params.iter_mut().find(|p| p.name.as_str() == pname) {
                                p.type_fact = fact;
                                p.optional = optional;
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
            infer_call_return_type(ctx, node, depth)
        }

        "variable" | "field_expression"
            if node.child_by_field_name("object").is_some()
                && node.child_by_field_name("field").is_some() =>
        {
            infer_field_expression_type(ctx, node, depth)
        }

        "variable" | "identifier" => {
            let text = node_text(node, ctx.source);
            // Check if it's a known local
            if let Some(decl) = ctx.resolve_visible_in_build_scopes(text, node.start_byte()) {
                if let Some(ref tf) = decl.type_fact {
                    return tf.clone();
                }
            }
            // Otherwise it's a global reference stub
            TypeFact::Stub(SymbolicStub::GlobalRef {
                name: text.into(),
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

/// Collect the inferred types of actual arguments at a function call site.
/// Used by function-level generic inference to unify `@generic T` params.
///
/// Uses a lightweight inference that handles literals and local variable
/// lookups without requiring `&mut BuildContext`.
fn collect_call_arg_types(ctx: &BuildContext, call_node: tree_sitter::Node) -> Vec<TypeFact> {
    let Some(args) = call_node.child_by_field_name("arguments") else {
        return Vec::new();
    };
    crate::util::extract_call_arg_nodes(args, ctx.source)
        .into_iter()
        .map(|e| infer_arg_type_lightweight(ctx, e))
        .collect()
}

fn function_return_with_call_args(
    fs: &crate::summary::FunctionSummary,
    call_arg_types: &[TypeFact],
) -> Option<TypeFact> {
    if !fs.generic_params.is_empty() && !call_arg_types.is_empty() {
        if let Some(substituted_returns) = crate::resolver::unify_function_generics(
            &fs.generic_params,
            &fs.signature.params,
            call_arg_types,
            &fs.signature.returns,
        ) {
            if let Some(ret) = substituted_returns.first() {
                return Some(ret.clone());
            }
        }
    }
    fs.signature.returns.first().cloned()
}

/// Lightweight type inference for call arguments — only handles literals
/// and local variable lookups. Sufficient for function-level generic
/// unification (e.g. `identity("abc")` → `T = string`).
fn infer_arg_type_lightweight(ctx: &BuildContext, node: tree_sitter::Node) -> TypeFact {
    match node.kind() {
        "number" => TypeFact::Known(KnownType::Number),
        "string" => TypeFact::Known(KnownType::String),
        "true" | "false" => TypeFact::Known(KnownType::Boolean),
        "nil" => TypeFact::Known(KnownType::Nil),
        "variable" | "field_expression"
            if node.child_by_field_name("object").is_some()
                && node.child_by_field_name("field").is_some() =>
        {
            infer_field_expression_type(ctx, node, 0)
        }
        "variable" | "identifier" => {
            let text = node_text(node, ctx.source);
            if let Some(decl) = ctx.resolve_visible_in_build_scopes(text, node.start_byte()) {
                if let Some(ref tf) = decl.type_fact {
                    return tf.clone();
                }
            }
            TypeFact::Unknown
        }
        "table_constructor" => {
            // For array-like table literals `{ 1, 2, "a" }`, infer the
            // element type so generic unification can bind `T` in `T[]`.
            infer_table_array_element_type_lightweight(ctx, node)
        }
        _ => TypeFact::Unknown,
    }
}

/// Infer the array element type of a table constructor for generic unification.
/// Returns `__array<elem_type>` if the table has only positional (array) entries,
/// otherwise returns `Unknown`.
fn infer_table_array_element_type_lightweight(ctx: &BuildContext, constructor: tree_sitter::Node) -> TypeFact {
    let mut elem_type: Option<TypeFact> = None;
    for i in 0..constructor.named_child_count() {
        let Some(field_list) = constructor.named_child(i as u32) else { continue };
        if field_list.kind() != "field_list" { continue; }
        for j in 0..field_list.named_child_count() {
            let Some(field_node) = field_list.named_child(j as u32) else { continue };
            if field_node.kind() != "field" { continue; }
            // Only handle positional entries (no key)
            if field_node.child_by_field_name("key").is_some() {
                return TypeFact::Unknown; // has named keys, not a pure array
            }
            if let Some(val) = field_node.child_by_field_name("value") {
                let val_type = infer_arg_type_lightweight(ctx, val);
                if val_type == TypeFact::Unknown {
                    continue;
                }
                elem_type = Some(match elem_type {
                    Some(existing) if existing == val_type => existing,
                    Some(_) => return TypeFact::Unknown, // mixed types
                    None => val_type,
                });
            }
        }
    }
    match elem_type {
        Some(t) => TypeFact::Known(KnownType::EmmyGeneric("__array".into(), vec![t])),
        None => TypeFact::Unknown,
    }
}

fn call_base_generic_args(
    fact: &TypeFact,
) -> Vec<TypeFact> {
    match fact {
        TypeFact::Known(KnownType::EmmyGeneric(_, params)) => params.clone(),
        _ => vec![],
    }
}

fn infer_call_return_type(ctx: &mut BuildContext, node: tree_sitter::Node, depth: usize) -> TypeFact {
    let callee = match node.child_by_field_name("callee") {
        Some(c) => c,
        None => return TypeFact::Unknown,
    };

    let callee_text = node_text(callee, ctx.source);

    // `require("mod")` → RequireRef stub
    if callee_text == "require" {
        if let Some(args) = node.child_by_field_name("arguments") {
            if let Some(first_arg) = args.named_child(0) {
                if let Some(module_path) = extract_string_from_node(ctx, first_arg) {
                    return TypeFact::Stub(SymbolicStub::RequireRef { module_path: module_path.into() });
                }
            }
        }
        return TypeFact::Unknown;
    }

    // `obj:method()` → CallReturn(base_stub, method_name)
    if let Some(method_node) = node.child_by_field_name("method") {
        let method_name = node_text(method_node, ctx.source).to_string();
        let explicit_arg_types = collect_call_arg_types(ctx, node);
        let base_fact = infer_expression_type(ctx, callee, depth + 1);

        if let TypeFact::Known(KnownType::Table(shape_id)) = &base_fact {
            let mut call_arg_types = Vec::with_capacity(explicit_arg_types.len() + 1);
            call_arg_types.push(TypeFact::Known(KnownType::Table(*shape_id)));
            call_arg_types.extend(explicit_arg_types.clone());
            if let Some(shape) = ctx.table_shapes.get(shape_id) {
                if let Some(fi) = shape.get_field(&method_name) {
                    match &fi.type_fact {
                        TypeFact::Known(KnownType::Function(ref sig)) => {
                            if let Some(ret) = sig.returns.first() {
                                return ret.clone();
                            }
                        }
                        TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                            if let Some(fs) = ctx.function_summaries.get(fid) {
                                if let Some(ret) = function_return_with_call_args(fs, &call_arg_types) {
                                    return ret.clone();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Fallback: qualified name in function_summaries for named local tables.
            let callee_text = node_text(callee, ctx.source);
            for sep in [":", "."] {
                let qualified = format!("{}{}{}", callee_text, sep, method_name);
                if let Some(&func_id) = ctx.function_name_to_id.get(&qualified) {
                    if let Some(fs) = ctx.function_summaries.get(&func_id) {
                        if let Some(ret) = function_return_with_call_args(fs, &call_arg_types) {
                            return ret.clone();
                        }
                    }
                }
            }
        }

        let generic_args = call_base_generic_args(&base_fact);
        let mut call_arg_types = Vec::with_capacity(1);
        call_arg_types.push(base_fact.clone());
        call_arg_types.extend(explicit_arg_types);
        return TypeFact::Stub(SymbolicStub::CallReturn {
            base: Box::new(base_fact),
            func_name: method_name.into(),
            is_method_call: true,
            call_arg_types,
            generic_args,
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
                let explicit_arg_types = collect_call_arg_types(ctx, node);
                let base_fact = infer_expression_type(ctx, base, depth + 1);

                if let TypeFact::Known(KnownType::Table(shape_id)) = &base_fact {
                    if let Some(shape) = ctx.table_shapes.get(shape_id) {
                        if let Some(fi) = shape.get_field(&func_name) {
                            match &fi.type_fact {
                                TypeFact::Known(KnownType::Function(ref sig)) => {
                                    if let Some(ret) = sig.returns.first() {
                                        return ret.clone();
                                    }
                                }
                                TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                                    if let Some(fs) = ctx.function_summaries.get(fid) {
                                        if let Some(ret) = function_return_with_call_args(fs, &explicit_arg_types) {
                                            return ret.clone();
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    // Fallback: qualified name in function_summaries for named local tables.
                    let qualified = format!("{}.{}", base_text, func_name);
                    if let Some(&func_id) = ctx.function_name_to_id.get(&qualified) {
                        if let Some(fs) = ctx.function_summaries.get(&func_id) {
                            if let Some(ret) = function_return_with_call_args(fs, &explicit_arg_types) {
                                return ret.clone();
                            }
                        }
                    }
                }

                let generic_args = call_base_generic_args(&base_fact);
                return TypeFact::Stub(SymbolicStub::CallReturn {
                    base: Box::new(base_fact),
                    func_name: func_name.into(),
                    is_method_call: false,
                    call_arg_types: explicit_arg_types,
                    generic_args,
                });
            }
        }
    }

    // Simple local/global function call. Prefer the scoped binding so
    // block-local function expressions do not leak through the file-wide map.
    if let Some(decl) = ctx.resolve_visible_in_build_scopes(callee_text, callee.start_byte()) {
        match decl.type_fact.as_ref() {
            Some(TypeFact::Known(KnownType::FunctionRef(func_id))) => {
                if let Some(fs) = ctx.function_summaries.get(func_id) {
                    let actual_arg_types = collect_call_arg_types(ctx, node);
                    if let Some(ret) = function_return_with_call_args(fs, &actual_arg_types) {
                        return ret;
                    }
                }
                return TypeFact::Unknown;
            }
            Some(TypeFact::Known(KnownType::Function(sig))) => {
                if let Some(ret) = sig.returns.first() {
                    return ret.clone();
                }
                return TypeFact::Unknown;
            }
            _ => return TypeFact::Unknown,
        }
    }

    // Global function fallback. Local functions are intentionally excluded here
    // because their visibility is already handled by the scoped lookup above.
    if let Some(callee_symbol) = get_lua_symbol(callee_text) {
        if let Some(&func_id) = ctx.function_name_index.get(&callee_symbol) {
            if let Some(fs) = ctx.function_summaries.get(&func_id) {
                // Function-level generic inference: if the callee has @generic params,
                // try to unify them from the actual argument types at the call site.
                if !fs.generic_params.is_empty() {
                    let actual_arg_types = collect_call_arg_types(ctx, node);
                    if let Some(substituted_returns) = crate::resolver::unify_function_generics(
                        &fs.generic_params,
                        &fs.signature.params,
                        &actual_arg_types,
                        &fs.signature.returns,
                    ) {
                        if let Some(ret) = substituted_returns.first() {
                            return ret.clone();
                        }
                    }
                }
                if let Some(ret) = fs.signature.returns.first() {
                    return ret.clone();
                }
            }
        }
    }

    TypeFact::Stub(SymbolicStub::FunctionCallReturn {
        func_name: callee_text.into(),
        call_arg_types: collect_call_arg_types(ctx, node),
    })
}

fn infer_field_expression_type(
    ctx: &BuildContext,
    node: tree_sitter::Node,
    depth: usize,
) -> TypeFact {
    let base = match node.child_by_field_name("object") {
        Some(b) => b,
        None => return TypeFact::Unknown,
    };
    let field = match node.child_by_field_name("field") {
        Some(f) => f,
        None => return TypeFact::Unknown,
    };

    let field_name = node_text(field, ctx.source).to_string();
    let base_fact = infer_field_base_type(ctx, base, depth + 1);

    if let TypeFact::Known(KnownType::Table(shape_id)) = &base_fact {
        if let Some(shape) = ctx.table_shapes.get(shape_id) {
            if let Some(fi) = shape.get_field(&field_name) {
                return fi.type_fact.clone();
            }
        }
    }

    TypeFact::Stub(SymbolicStub::FieldOf {
        base: Box::new(base_fact),
        field: field_name.into(),
    })
}

fn infer_field_base_type(
    ctx: &BuildContext,
    base: tree_sitter::Node,
    depth: usize,
) -> TypeFact {
    if depth > MAX_TABLE_SHAPE_DEPTH {
        return TypeFact::Unknown;
    }

    if matches!(base.kind(), "variable" | "field_expression")
        && base.child_by_field_name("object").is_some()
        && base.child_by_field_name("field").is_some()
    {
        return infer_field_expression_type(ctx, base, depth);
    }

    let base_text = node_text(base, ctx.source);
    if matches!(base.kind(), "variable" | "identifier") {
        if let Some(decl) = ctx.resolve_visible_in_build_scopes(base_text, base.start_byte()) {
            if let Some(ref tf) = decl.type_fact {
                return tf.clone();
            }
        }
    }

    TypeFact::Stub(SymbolicStub::GlobalRef {
        name: base_text.into(),
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
