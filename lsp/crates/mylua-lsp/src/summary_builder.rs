use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use tower_lsp_server::ls_types::Uri;

use crate::emmy::{collect_preceding_comments, parse_emmy_comments, EmmyAnnotation};
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
        uri: uri.clone(),
        source,
        require_bindings: Vec::new(),
        global_contributions: Vec::new(),
        function_summaries: HashMap::new(),
        type_definitions: Vec::new(),
        local_type_facts: HashMap::new(),
        table_shapes: HashMap::new(),
        next_shape_id: 0,
        pending_type_annotation: None,
    };

    let root = tree.root_node();
    visit_top_level(&mut ctx, root);

    let content_hash = hash_bytes(source);
    let signature_fingerprint = compute_signature_fingerprint(&ctx);

    DocumentSummary {
        uri: uri.clone(),
        content_hash,
        require_bindings: ctx.require_bindings,
        global_contributions: ctx.global_contributions,
        function_summaries: ctx.function_summaries,
        type_definitions: ctx.type_definitions,
        local_type_facts: ctx.local_type_facts,
        table_shapes: ctx.table_shapes,
        signature_fingerprint,
    }
}

struct BuildContext<'a> {
    #[allow(dead_code)]
    uri: Uri,
    source: &'a [u8],
    require_bindings: Vec<RequireBinding>,
    global_contributions: Vec<GlobalContribution>,
    function_summaries: HashMap<String, FunctionSummary>,
    type_definitions: Vec<TypeDefinition>,
    local_type_facts: HashMap<String, LocalTypeFact>,
    table_shapes: HashMap<TableShapeId, TableShape>,
    next_shape_id: u32,
    /// `---@type X` annotation pending attachment to the next local declaration.
    pending_type_annotation: Option<String>,
}

impl<'a> BuildContext<'a> {
    fn alloc_shape_id(&mut self) -> TableShapeId {
        let id = TableShapeId(self.next_shape_id);
        self.next_shape_id += 1;
        id
    }

    fn take_pending_type(&mut self) -> Option<String> {
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
            "local_declaration" => visit_local_declaration(ctx, node),
            "local_function_declaration" => {
                ctx.pending_type_annotation = None;
                visit_local_function(ctx, node);
            }
            "function_declaration" => {
                ctx.pending_type_annotation = None;
                visit_function_declaration(ctx, node);
            }
            "assignment_statement" => {
                ctx.pending_type_annotation = None;
                visit_assignment(ctx, node);
            }
            "emmy_comment" => visit_emmy_comment(ctx, node),
            _ => {
                ctx.pending_type_annotation = None;
            }
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
    for i in 0..name_count {
        let name_node = match names_node.named_child(i as u32) {
            Some(n) if n.kind() == "identifier" => n,
            _ => continue,
        };
        let name = node_text(name_node, ctx.source).to_string();
        let range = ts_node_to_range(name_node);

        // If we have an explicit @type annotation, it takes priority
        if i == 0 {
            if let Some(ref type_name) = pending_type {
                let type_fact = emmy_type_text_to_fact(type_name);
                ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
                    name: name.clone(),
                    type_fact,
                    source: TypeFactSource::EmmyAnnotation,
                    range,
                });
                continue;
            }
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

/// Extract `---@type X` from a comment node immediately preceding the given node.
fn extract_preceding_type_annotation(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
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
                                    return Some(type_text.to_string());
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
                    return Some(type_text.to_string());
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
        range: ts_node_to_range(value_node),
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
    let fingerprint = fs.signature_fingerprint;
    ctx.function_summaries.insert(name.clone(), fs);

    ctx.global_contributions.push(GlobalContribution {
        name: name.clone(),
        kind: GlobalContributionKind::Function,
        type_fact: TypeFact::Known(KnownType::Function(FunctionSignature {
            params: Vec::new(),
            returns: Vec::new(),
        })),
        range: ts_node_to_range(node),
        selection_range: ts_node_to_range(name_node),
    });
    let _ = fingerprint;
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

    // Extract params from Emmy annotations
    for ann in &annotations {
        match ann {
            EmmyAnnotation::Param { name: pname, type_text, .. } => {
                emmy_annotated = true;
                params.push(ParamInfo {
                    name: pname.clone(),
                    type_fact: emmy_type_text_to_fact(type_text),
                });
            }
            EmmyAnnotation::Return { type_text, .. } => {
                emmy_annotated = true;
                returns.push(emmy_type_text_to_fact(type_text));
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
        range: ts_node_to_range(decl_node),
        signature_fingerprint: fingerprint,
        returned_shapes: Vec::new(),
        emmy_annotated,
    }
}

fn extract_ast_params(params: &mut Vec<ParamInfo>, param_list: tree_sitter::Node, source: &[u8]) {
    for i in 0..param_list.named_child_count() {
        if let Some(child) = param_list.named_child(i as u32) {
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
                "varargs" => {
                    params.push(ParamInfo {
                        name: "...".to_string(),
                        type_fact: TypeFact::Unknown,
                    });
                }
                _ => {}
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
                let type_fact = value_node
                    .map(|v| infer_expression_type(ctx, v, 0))
                    .unwrap_or(TypeFact::Unknown);

                ctx.global_contributions.push(GlobalContribution {
                    name,
                    kind: GlobalContributionKind::Variable,
                    type_fact,
                    range: ts_node_to_range(node),
                    selection_range: ts_node_to_range(var_node),
                });
            }
            // Field assignment: `x.foo = expr`
            "field_expression" | "variable" => {
                let full_text = node_text(var_node, ctx.source);
                if full_text.contains('.') {
                    let type_fact = value_node
                        .map(|v| infer_expression_type(ctx, v, 0))
                        .unwrap_or(TypeFact::Unknown);

                    // Check if the base is a known local with a table shape
                    let parts: Vec<&str> = full_text.splitn(2, '.').collect();
                    let is_local_field = parts.len() == 2
                        && ctx.local_type_facts.contains_key(parts[0]);

                    if is_local_field {
                        let base_name = parts[0];
                        let field_name = parts[1].to_string();
                        // Try to update the local's table shape
                        if let Some(ltf) = ctx.local_type_facts.get(base_name) {
                            if let TypeFact::Known(KnownType::Table(shape_id)) = &ltf.type_fact {
                                let sid = *shape_id;
                                if let Some(shape) = ctx.table_shapes.get_mut(&sid) {
                                    shape.set_field(field_name.clone(), FieldInfo {
                                        name: field_name,
                                        type_fact,
                                        def_range: Some(ts_node_to_range(node)),
                                        assignment_count: 1,
                                    });
                                    continue;
                                }
                            }
                        }
                    }

                    // Otherwise treat as global table extension
                    let name = full_text.to_string();
                    ctx.global_contributions.push(GlobalContribution {
                        name,
                        kind: GlobalContributionKind::TableExtension,
                        type_fact,
                        range: ts_node_to_range(node),
                        selection_range: ts_node_to_range(var_node),
                    });
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Emmy comments (type definitions)
// ---------------------------------------------------------------------------

fn visit_emmy_comment(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let mut lines = Vec::new();
    for i in 0..node.named_child_count() {
        if let Some(line_node) = node.named_child(i as u32) {
            if line_node.kind() == "emmy_line" {
                lines.push(node_text(line_node, ctx.source).to_string());
            }
        }
    }
    let text = lines.join("\n");
    let annotations = parse_emmy_comments(&text);

    let mut current_class: Option<(String, Vec<TypeFieldDef>)> = None;
    let mut has_class_or_alias = false;

    for ann in &annotations {
        match ann {
            EmmyAnnotation::Class { name, .. } => {
                has_class_or_alias = true;
                if let Some((cname, fields)) = current_class.take() {
                    ctx.type_definitions.push(TypeDefinition {
                        name: cname,
                        kind: TypeDefinitionKind::Class,
                        fields,
                        range: ts_node_to_range(node),
                    });
                }
                current_class = Some((name.clone(), Vec::new()));
            }
            EmmyAnnotation::Field { name: fname, type_text, .. } => {
                if let Some((_, ref mut fields)) = current_class {
                    fields.push(TypeFieldDef {
                        name: fname.clone(),
                        type_fact: emmy_type_text_to_fact(type_text),
                        range: ts_node_to_range(node),
                    });
                }
            }
            EmmyAnnotation::Type { type_text, .. } => {
                if current_class.is_none() {
                    // Standalone @type: set as pending for the next local declaration
                    ctx.pending_type_annotation = Some(type_text.clone());
                }
            }
            EmmyAnnotation::Alias { name, type_text } => {
                has_class_or_alias = true;
                if let Some((cname, fields)) = current_class.take() {
                    ctx.type_definitions.push(TypeDefinition {
                        name: cname,
                        kind: TypeDefinitionKind::Class,
                        fields,
                        range: ts_node_to_range(node),
                    });
                }
                ctx.type_definitions.push(TypeDefinition {
                    name: name.clone(),
                    kind: TypeDefinitionKind::Alias,
                    fields: Vec::new(),
                    range: ts_node_to_range(node),
                });
                let _ = type_text;
            }
            _ => {}
        }
    }

    if let Some((cname, fields)) = current_class.take() {
        ctx.type_definitions.push(TypeDefinition {
            name: cname,
            kind: TypeDefinitionKind::Class,
            fields,
            range: ts_node_to_range(node),
        });
    }

    // Don't clear pending_type if we only had @type (no @class/@alias)
    // pending_type was already set above and will be consumed by visit_local_declaration
    let _ = has_class_or_alias;
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
            TypeFact::Known(KnownType::Function(FunctionSignature {
                params: Vec::new(),
                returns: Vec::new(),
            }))
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

    // `mod.func()` → CallReturn(RequireRef/GlobalRef, func_name)
    if callee.kind() == "field_expression" {
        if let Some(base) = callee.child_by_field_name("object") {
            if let Some(field) = callee.child_by_field_name("field") {
                let base_text = node_text(base, ctx.source);
                let func_name = node_text(field, ctx.source).to_string();

                let base_stub = if let Some(fact) = ctx.local_type_facts.get(base_text) {
                    match &fact.type_fact {
                        TypeFact::Stub(s) => s.clone(),
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
                            def_range: Some(ts_node_to_range(child)),
                            assignment_count: 1,
                        });
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
// Emmy type text → TypeFact
// ---------------------------------------------------------------------------

fn emmy_type_text_to_fact(type_text: &str) -> TypeFact {
    let text = type_text.trim();
    if text.is_empty() {
        return TypeFact::Unknown;
    }

    // Union types: "string|number"
    if text.contains('|') {
        let parts: Vec<TypeFact> = text
            .split('|')
            .map(|s| emmy_type_text_to_fact(s.trim()))
            .collect();
        if parts.len() == 1 {
            return parts.into_iter().next().unwrap();
        }
        return TypeFact::Union(parts);
    }

    // Optional: "string?"
    if let Some(base) = text.strip_suffix('?') {
        return TypeFact::Union(vec![
            emmy_type_text_to_fact(base),
            TypeFact::Known(KnownType::Nil),
        ]);
    }

    match text {
        "nil" => TypeFact::Known(KnownType::Nil),
        "boolean" => TypeFact::Known(KnownType::Boolean),
        "number" => TypeFact::Known(KnownType::Number),
        "integer" => TypeFact::Known(KnownType::Integer),
        "string" => TypeFact::Known(KnownType::String),
        "table" => TypeFact::Known(KnownType::Table(TableShapeId(u32::MAX))),
        "function" => TypeFact::Known(KnownType::Function(FunctionSignature {
            params: Vec::new(),
            returns: Vec::new(),
        })),
        "any" => TypeFact::Unknown,
        _ => TypeFact::Known(KnownType::EmmyType(text.to_string())),
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
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
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

    // Hash global contributions
    let mut globals: Vec<&str> = ctx.global_contributions.iter().map(|g| g.name.as_str()).collect();
    globals.sort();
    for name in &globals {
        name.hash(&mut hasher);
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

    // Hash type definitions
    let mut type_names: Vec<&str> = ctx.type_definitions.iter().map(|t| t.name.as_str()).collect();
    type_names.sort();
    for name in &type_names {
        name.hash(&mut hasher);
    }

    hasher.finish()
}
