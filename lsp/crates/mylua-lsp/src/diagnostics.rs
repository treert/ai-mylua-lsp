use std::collections::HashSet;
use tower_lsp_server::ls_types::*;
use crate::config::DiagnosticsConfig;
use crate::resolver;
use crate::scope::ScopeTree;
use crate::type_system::{TypeFact, KnownType, SymbolicStub};
use crate::util::{ts_node_to_range, node_text, truncate};
use crate::aggregation::WorkspaceAggregation;

// Built-in identifier set is now version-dependent and lives in
// `lua_builtins::builtins_for(version)`. Diagnostic paths pull the
// set through `collect_semantic_diagnostics`'s config parameter.

pub fn collect_diagnostics(root: tree_sitter::Node, source: &[u8]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut cursor = root.walk();
    collect_errors_recursive(&mut cursor, source, &mut diagnostics);
    diagnostics
}

pub fn collect_semantic_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diag_config: &DiagnosticsConfig,
) -> Vec<Diagnostic> {
    collect_semantic_diagnostics_with_version(
        root, source, uri, index, scope_tree, diag_config, "5.3",
    )
}

/// Version-aware variant — `runtime_version` (e.g. `"5.3"` / `"5.4"`
/// / `"luajit"`) selects which built-in identifiers are considered
/// defined so that `undefinedGlobal` and related checks stay
/// accurate per runtime.
pub fn collect_semantic_diagnostics_with_version(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diag_config: &DiagnosticsConfig,
    runtime_version: &str,
) -> Vec<Diagnostic> {
    if !diag_config.enable {
        return Vec::new();
    }

    let mut diagnostics = Vec::new();
    let builtins: HashSet<&str> = crate::lua_builtins::builtins_for(runtime_version)
        .into_iter()
        .collect();

    let mut cursor = root.walk();
    // `---@meta` files declare stubs for runtime-provided APIs, so
    // many of the identifiers they reference are intentionally not
    // declared in the workspace. Skip `undefinedGlobal` there to
    // avoid a wall of noise on a legitimate stub file.
    let is_meta = index.summaries.get(uri).map(|s| s.is_meta).unwrap_or(false);
    if let Some(severity) = diag_config.undefined_global.to_lsp_severity() {
        if !is_meta {
            check_undefined_globals(&mut cursor, source, &builtins, index, scope_tree, &mut diagnostics, severity);
        }
    }
    let emmy_severity = diag_config.emmy_unknown_field.to_lsp_severity();
    let lua_error_severity = diag_config.lua_field_error.to_lsp_severity();
    let lua_warn_severity = diag_config.lua_field_warning.to_lsp_severity();
    if emmy_severity.is_some() || lua_error_severity.is_some() || lua_warn_severity.is_some() {
        check_field_access_diagnostics(
            root, source, uri, index, &mut diagnostics,
            emmy_severity, lua_error_severity, lua_warn_severity,
        );
    }
    if let Some(severity) = diag_config.emmy_type_mismatch.to_lsp_severity() {
        check_type_mismatch_diagnostics(
            root, source, uri, index, scope_tree, &mut diagnostics, severity,
        );
    }
    if let Some(severity) = diag_config.duplicate_table_key.to_lsp_severity() {
        check_duplicate_table_keys(root, source, &mut diagnostics, severity);
    }
    if let Some(severity) = diag_config.unused_local.to_lsp_severity() {
        check_unused_locals(root, source, scope_tree, &mut diagnostics, severity);
    }
    let count_sev = diag_config.argument_count_mismatch.to_lsp_severity();
    let type_sev = diag_config.argument_type_mismatch.to_lsp_severity();
    if count_sev.is_some() || type_sev.is_some() {
        check_call_argument_diagnostics(
            root, source, uri, index, &mut diagnostics, count_sev, type_sev,
        );
    }
    if let Some(severity) = diag_config.return_mismatch.to_lsp_severity() {
        check_return_mismatch_diagnostics(root, source, &mut diagnostics, severity);
    }
    diagnostics
}

fn check_undefined_globals(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    builtins: &HashSet<&str>,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let node = cursor.node();

    if node.kind() == "identifier" {
        if let Some(parent) = node.parent() {
            let is_bare_var = parent.kind() == "variable" && parent.child_count() == 1;
            let is_definition = matches!(
                parent.kind(),
                "function_name" | "attribute_name_list" | "name_list" | "label_statement"
            );
            if is_bare_var && !is_definition {
                let name = node_text(node, source);
                let byte_offset = node.start_byte();
                let is_local = scope_tree.resolve_decl(byte_offset, name).is_some();
                if !is_local
                    && !builtins.contains(name)
                    && !index.global_shard.contains_key(name)
                {
                    diagnostics.push(Diagnostic {
                        range: ts_node_to_range(node, source),
                        severity: Some(severity),
                        source: Some("mylua".to_string()),
                        message: format!("Undefined global '{}'", name),
                        ..Default::default()
                    });
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            check_undefined_globals(cursor, source, builtins, index, scope_tree, diagnostics, severity);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn collect_errors_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let node = cursor.node();
    if node.is_error() {
        diagnostics.push(Diagnostic {
            range: ts_node_to_range(node, source),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Syntax error near '{}'", truncate(node_text(node, source), 40)),
            ..Default::default()
        });
    } else if node.is_missing() {
        diagnostics.push(Diagnostic {
            range: ts_node_to_range(node, source),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("mylua".to_string()),
            message: format!("Missing '{}'", node.kind()),
            ..Default::default()
        });
    }

    if node.has_error() && cursor.goto_first_child() {
        loop {
            collect_errors_recursive(cursor, source, diagnostics);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn check_field_access_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    diagnostics: &mut Vec<Diagnostic>,
    emmy_severity: Option<DiagnosticSeverity>,
    lua_error_severity: Option<DiagnosticSeverity>,
    lua_warn_severity: Option<DiagnosticSeverity>,
) {
    let mut cursor = root.walk();
    collect_field_diagnostics(
        &mut cursor, source, uri, index, diagnostics,
        emmy_severity, lua_error_severity, lua_warn_severity,
    );
}

/// Returns true if `node` is (or is any descendant of) the left-hand side
/// of an assignment statement. Walks ancestors so that chained LHS like
/// `a.b.c = 1` — where the outer node is `variable` and the inner
/// `field_expression` node (`a.b`) is not a direct child of `variable_list` —
/// is still recognized as an assignment target and skipped by field
/// diagnostics.
fn is_assignment_target(node: tree_sitter::Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "assignment_statement" {
            // `current` is always an ancestor of (or equal to) `node`,
            // so `is_ancestor_or_equal(left, node)` already covers the
            // `left == current` case.
            return parent
                .child_by_field_name("left")
                .map_or(false, |left| is_ancestor_or_equal(left, node));
        }
        current = parent;
    }
    false
}

/// Returns true when `ancestor` is, or contains, `descendant`.
fn is_ancestor_or_equal(ancestor: tree_sitter::Node, descendant: tree_sitter::Node) -> bool {
    let mut n = descendant;
    loop {
        if n.id() == ancestor.id() {
            return true;
        }
        match n.parent() {
            Some(p) => n = p,
            None => return false,
        }
    }
}

fn collect_field_diagnostics(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    diagnostics: &mut Vec<Diagnostic>,
    emmy_severity: Option<DiagnosticSeverity>,
    lua_error_severity: Option<DiagnosticSeverity>,
    lua_warn_severity: Option<DiagnosticSeverity>,
) {
    let node = cursor.node();

    let is_dotted = matches!(node.kind(), "field_expression" | "variable")
        && node.child_by_field_name("object").is_some()
        && node.child_by_field_name("field").is_some();

    if is_dotted && !is_assignment_target(node) {
        if let (Some(object), Some(field)) = (
            node.child_by_field_name("object"),
            node.child_by_field_name("field"),
        ) {
            let base_fact = crate::hover::infer_node_type(object, source, uri, index);
            let field_name = node_text(field, source).to_string();

            let resolved_base = resolver::resolve_type(&base_fact, index);
            match &resolved_base.type_fact {
                TypeFact::Known(KnownType::EmmyType(type_name)) => {
                    if let Some(severity) = emmy_severity {
                        let field_resolved = resolver::resolve_field_chain(
                            &resolved_base.type_fact,
                            &[field_name.clone()],
                            index,
                        );
                        if field_resolved.type_fact == TypeFact::Unknown && field_resolved.def_uri.is_none() {
                            let qualified = format!("{}.{}", type_name, field_name);
                            if index.global_shard.get(&qualified).is_none() {
                                diagnostics.push(Diagnostic {
                                    range: ts_node_to_range(field, source),
                                    severity: Some(severity),
                                    source: Some("mylua".to_string()),
                                    message: format!(
                                        "Unknown field '{}' on type '{}'",
                                        field_name, type_name
                                    ),
                                    ..Default::default()
                                });
                            }
                        }
                    }
                }
                TypeFact::Known(KnownType::Table(shape_id)) => {
                    let table_uri = resolved_base.def_uri.as_ref().unwrap_or(uri);
                    if let Some(summary) = index.summaries.get(table_uri) {
                        if let Some(shape) = summary.table_shapes.get(shape_id) {
                            if !shape.fields.contains_key(&field_name) {
                                let severity = if shape.is_closed {
                                    lua_error_severity
                                } else {
                                    lua_warn_severity
                                };
                                if let Some(sev) = severity {
                                    diagnostics.push(Diagnostic {
                                        range: ts_node_to_range(field, source),
                                        severity: Some(sev),
                                        source: Some("mylua".to_string()),
                                        message: format!(
                                            "Unknown field '{}' on table",
                                            field_name
                                        ),
                                        ..Default::default()
                                    });
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_field_diagnostics(
                cursor, source, uri, index, diagnostics,
                emmy_severity, lua_error_severity, lua_warn_severity,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn check_type_mismatch_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let Some(summary) = index.summaries.get(uri).cloned() else { return };

    // Pass 1 — original behaviour: check the initial `local x = <rhs>`
    // assignment against `---@type` declared on the same line.
    for ltf in summary.local_type_facts.values() {
        if ltf.source != crate::summary::TypeFactSource::EmmyAnnotation {
            continue;
        }
        let declared = &ltf.type_fact;
        let actual = find_actual_type_for_local(&ltf.name, &ltf.range, root, source, &summary);
        if actual == TypeFact::Unknown {
            continue;
        }
        if !is_type_compatible(declared, &actual) {
            diagnostics.push(Diagnostic {
                range: ltf.range,
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!(
                    "Type mismatch: declared '{}', got '{}'",
                    declared, actual
                ),
                ..Default::default()
            });
        }
    }

    // Pass 2 — follow-up `x = <rhs>` assignments to locals previously
    // declared with `---@type T`. Walks every `assignment_statement`,
    // resolves the LHS identifier via `scope_tree` back to its
    // declaration site, and (if the decl site carries an Emmy type
    // fact) compares RHS literal type against the declared type.
    // Shadowing is handled correctly by `resolve_decl` — a new `local
    // x` inside an inner scope produces a different `decl_byte`, so
    // assignments inside that scope won't be checked against the
    // outer declaration's type.
    check_assignment_type_mismatches(
        root, source, &summary, scope_tree, diagnostics, severity,
    );
}

/// Walk every `assignment_statement` and, for each simple LHS
/// identifier that resolves to a local whose declaration carries an
/// Emmy type annotation, report mismatches between the declared type
/// and the RHS literal type.
fn check_assignment_type_mismatches(
    root: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let mut cursor = root.walk();
    walk_assignment_nodes(&mut cursor, source, summary, scope_tree, diagnostics, severity);
}

fn walk_assignment_nodes(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let node = cursor.node();
    if node.kind() == "assignment_statement" {
        inspect_assignment_for_mismatch(node, source, summary, scope_tree, diagnostics, severity);
    }
    if cursor.goto_first_child() {
        loop {
            walk_assignment_nodes(cursor, source, summary, scope_tree, diagnostics, severity);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn inspect_assignment_for_mismatch(
    node: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let Some(left) = node.child_by_field_name("left") else { return };
    let Some(right) = node.child_by_field_name("right") else { return };

    // Iterate LHS/RHS pairs. Only single-variable, bare-identifier
    // LHS entries are checked — dotted / subscripted LHS like
    // `t.x = "str"` is a field write, not an assignment to a local
    // with an `@type` annotation, and belongs to field-access
    // diagnostics instead.
    for i in 0..left.named_child_count() {
        let Some(lhs) = left.named_child(i as u32) else { continue };
        // Single-identifier LHS: either a bare `identifier` or a
        // `variable` wrapping exactly one identifier. Skip dotted /
        // subscripted forms (they have an `object` or `index` field).
        let ident_node = if lhs.kind() == "identifier" {
            Some(lhs)
        } else if lhs.kind() == "variable"
            && lhs.child_by_field_name("object").is_none()
            && lhs.child_by_field_name("index").is_none()
        {
            // `variable` with a single identifier child.
            lhs.named_child(0).filter(|c| c.kind() == "identifier")
        } else {
            None
        };
        let Some(ident) = ident_node else { continue };

        let name = node_text(ident, source);
        // Resolve to the declaration site; skip names that don't
        // resolve (globals without an @type fact reach this path).
        let Some(decl) = scope_tree.resolve_decl(ident.start_byte(), name) else {
            continue;
        };

        // The local's declaration site must carry an EmmyAnnotation-
        // sourced type fact — otherwise there's no declared type to
        // compare against. `local_type_facts` is keyed by name; guard
        // against shadowing by matching the decl_byte against the
        // ltf's range start.
        let Some(ltf) = summary.local_type_facts.get(name) else { continue };
        if ltf.source != crate::summary::TypeFactSource::EmmyAnnotation {
            continue;
        }
        // Confirm the ltf corresponds to the resolved declaration: the
        // ltf's range line should match the decl line. tree-sitter
        // byte -> line lookup isn't free; fall back to line comparison
        // via the AST node at decl_byte.
        if !ltf_matches_decl(decl.decl_byte, ltf, source) {
            continue;
        }

        let Some(value_expr) = right.named_child(i as u32) else { continue };
        let actual = infer_literal_type(value_expr, source, summary);
        if actual == TypeFact::Unknown {
            continue;
        }
        if !is_type_compatible(&ltf.type_fact, &actual) {
            diagnostics.push(Diagnostic {
                range: ts_node_to_range(ident, source),
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!(
                    "Type mismatch on assignment to '{}': declared '{}', got '{}'",
                    name, ltf.type_fact, actual
                ),
                ..Default::default()
            });
        }
    }
}

/// True if the local-type-fact range starts on the same line as
/// `decl_byte`. Used to guard against a same-named later declaration
/// leaking its ltf onto an outer-scope assignment.
fn ltf_matches_decl(
    decl_byte: usize,
    ltf: &crate::summary::LocalTypeFact,
    source: &[u8],
) -> bool {
    // Count line breaks up to decl_byte.
    let mut line: u32 = 0;
    for &b in source.iter().take(decl_byte.min(source.len())) {
        if b == b'\n' {
            line += 1;
        }
    }
    ltf.range.start.line == line
}

fn find_actual_type_for_local(
    name: &str,
    decl_range: &Range,
    root: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
) -> TypeFact {
    let target_line = decl_range.start.line;
    find_local_rhs_type(root, name, target_line, summary, source)
}

/// Walk the subtree under `node` looking for a `local_declaration` that
/// declares `name` on line `target_line` and return the inferred literal
/// type of its matching RHS value. Pure recursion (no tree-cursor state)
/// keeps the traversal robust against early returns.
fn find_local_rhs_type(
    node: tree_sitter::Node,
    name: &str,
    target_line: u32,
    summary: &crate::summary::DocumentSummary,
    source: &[u8],
) -> TypeFact {
    if node.kind() == "local_declaration" {
        if let Some(names) = node.child_by_field_name("names") {
            for i in 0..names.named_child_count() {
                if let Some(n) = names.named_child(i as u32) {
                    if n.kind() == "identifier" && node_text(n, source) == name {
                        let node_line = n.start_position().row as u32;
                        if node_line == target_line {
                            if let Some(values) = node.child_by_field_name("values") {
                                if let Some(val) = values.named_child(i as u32) {
                                    return infer_literal_type(val, source, summary);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            let result = find_local_rhs_type(child, name, target_line, summary, source);
            if result != TypeFact::Unknown {
                return result;
            }
        }
    }
    TypeFact::Unknown
}

fn infer_literal_type(
    node: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
) -> TypeFact {
    match node.kind() {
        "number" => TypeFact::Known(KnownType::Number),
        "string" => TypeFact::Known(KnownType::String),
        "true" | "false" => TypeFact::Known(KnownType::Boolean),
        "nil" => TypeFact::Known(KnownType::Nil),
        "table_constructor" => TypeFact::Known(KnownType::Table(crate::table_shape::TableShapeId(u32::MAX))),
        "function_definition" => TypeFact::Known(KnownType::Function(crate::type_system::FunctionSignature {
            params: vec![], returns: vec![],
        })),
        "variable" | "identifier" => {
            let text = node_text(node, source);
            if let Some(ltf) = summary.local_type_facts.get(text) {
                if ltf.source != crate::summary::TypeFactSource::EmmyAnnotation {
                    return ltf.type_fact.clone();
                }
            }
            TypeFact::Unknown
        }
        _ => TypeFact::Unknown,
    }
}

fn is_type_compatible(declared: &TypeFact, actual: &TypeFact) -> bool {
    match (declared, actual) {
        (TypeFact::Unknown, _) | (_, TypeFact::Unknown) => true,
        (TypeFact::Known(d), TypeFact::Known(a)) => known_types_compatible(d, a),
        (TypeFact::Union(types), actual) => {
            types.iter().any(|t| is_type_compatible(t, actual))
        }
        (declared, TypeFact::Union(types)) => {
            types.iter().all(|t| is_type_compatible(declared, t))
        }
        (TypeFact::Stub(SymbolicStub::TypeRef { name }), TypeFact::Known(a)) => {
            is_named_type_compatible(name, a)
        }
        _ => true,
    }
}

fn is_named_type_compatible(name: &str, actual: &KnownType) -> bool {
    match (name, actual) {
        ("string", KnownType::String) => true,
        ("number" | "integer", KnownType::Number | KnownType::Integer) => true,
        ("boolean", KnownType::Boolean) => true,
        ("nil", KnownType::Nil) => true,
        ("table", KnownType::Table(_)) => true,
        ("function", KnownType::Function(_)) => true,
        ("any", _) => true,
        (_, KnownType::Nil) => true,
        ("string", KnownType::Number | KnownType::Boolean) => false,
        ("number" | "integer", KnownType::String | KnownType::Boolean) => false,
        ("boolean", KnownType::String | KnownType::Number) => false,
        _ => true,
    }
}

fn known_types_compatible(declared: &KnownType, actual: &KnownType) -> bool {
    match (declared, actual) {
        (KnownType::Nil, _) | (_, KnownType::Nil) => true,
        (KnownType::Number, KnownType::Number | KnownType::Integer) => true,
        (KnownType::Integer, KnownType::Number | KnownType::Integer) => true,
        (KnownType::String, KnownType::String) => true,
        (KnownType::Boolean, KnownType::Boolean) => true,
        (KnownType::Table(_), KnownType::Table(_)) => true,
        (KnownType::Function(_), KnownType::Function(_)) => true,
        (KnownType::EmmyType(name), actual) | (KnownType::EmmyGeneric(name, _), actual) => {
            is_named_type_compatible(name, actual)
        }
        (KnownType::String, KnownType::Number | KnownType::Integer | KnownType::Boolean) => false,
        (KnownType::Number | KnownType::Integer, KnownType::String | KnownType::Boolean) => false,
        (KnownType::Boolean, KnownType::String | KnownType::Number | KnownType::Integer) => false,
        (KnownType::Table(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Function(_)) => false,
        (KnownType::Function(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Table(_)) => false,
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// P2-3 — duplicate table keys and unused locals
// ---------------------------------------------------------------------------

/// Walk every `table_constructor` and report keys that appear more
/// than once. Only named keys (`{ a = 1, a = 2 }`) and static
/// bracket-key literals (`{ [1] = 'x', [1] = 'y' }`) can be reliably
/// compared at summary-build time; dynamic `[expr]` keys are skipped.
fn check_duplicate_table_keys(
    root: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let mut cursor = root.walk();
    check_duplicate_keys_recursive(&mut cursor, source, diagnostics, severity);
}

fn check_duplicate_keys_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let node = cursor.node();
    if node.kind() == "table_constructor" {
        let mut seen: std::collections::HashMap<String, Range> = std::collections::HashMap::new();
        for i in 0..node.named_child_count() {
            let Some(field_list) = node.named_child(i as u32) else { continue };
            let fields = if field_list.kind() == "field_list" {
                field_list
            } else {
                continue;
            };
            for j in 0..fields.named_child_count() {
                let Some(field) = fields.named_child(j as u32) else { continue };
                if field.kind() != "field" {
                    continue;
                }
                let Some(key_text) = extract_field_key(field, source) else { continue };
                if let Some(first_range) = seen.get(&key_text) {
                    let range = ts_node_to_range(field, source);
                    diagnostics.push(Diagnostic {
                        range,
                        severity: Some(severity),
                        source: Some("mylua".to_string()),
                        message: format!(
                            "Duplicate table key '{}' (first defined at line {})",
                            key_text,
                            first_range.start.line + 1,
                        ),
                        ..Default::default()
                    });
                } else {
                    seen.insert(key_text, ts_node_to_range(field, source));
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            check_duplicate_keys_recursive(cursor, source, diagnostics, severity);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn extract_field_key(field: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Identifier key: `a = 1`
    if let Some(key) = field.child_by_field_name("key") {
        match key.kind() {
            "identifier" => {
                return Some(node_text(key, source).to_string());
            }
            "string" => {
                // Bracket string key: `["a"] = 1` — normalize by text
                // content excluding quotes so that `["a"]` vs `['a']`
                // dedup.
                let t = node_text(key, source);
                return Some(t.trim_matches(|c| c == '"' || c == '\'' || c == '[' || c == ']').to_string());
            }
            "number" => {
                return Some(format!("num:{}", node_text(key, source)));
            }
            _ => return None,
        }
    }
    None
}

/// Warn on locals that are declared but never referenced. Uses the
/// `ScopeTree` to find every declaration, then scans the file for
/// matching identifier usages (excluding the declaration site
/// itself). `_` / `_*` names are conventionally "intentionally
/// discarded" and don't trigger the warning.
fn check_unused_locals(
    root: tree_sitter::Node,
    source: &[u8],
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    // Count references per (name, decl_byte) by walking the tree
    // and resolving each identifier through the scope tree.
    let mut ref_count: std::collections::HashMap<(String, usize), usize> =
        std::collections::HashMap::new();
    let mut cursor = root.walk();
    count_identifier_references(&mut cursor, source, scope_tree, &mut ref_count);

    // Any declaration whose ref_count is zero is unused.
    for decl in scope_tree.all_declarations() {
        // Convention: `_` or `_something` indicate intentional discard.
        if decl.name == "_" || decl.name.starts_with('_') {
            continue;
        }
        let key = (decl.name.clone(), decl.decl_byte);
        if ref_count.get(&key).copied().unwrap_or(0) == 0 {
            diagnostics.push(Diagnostic {
                range: decl.selection_range,
                severity: Some(severity),
                source: Some("mylua".to_string()),
                message: format!("Unused local '{}'", decl.name),
                ..Default::default()
            });
        }
    }
}

fn count_identifier_references(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    scope_tree: &ScopeTree,
    ref_count: &mut std::collections::HashMap<(String, usize), usize>,
) {
    let node = cursor.node();
    if node.kind() == "identifier" {
        let name = node_text(node, source);
        let byte = node.start_byte();
        if let Some(decl) = scope_tree.resolve_decl(byte, name) {
            // Skip if this identifier IS the declaration itself —
            // decl.decl_byte's occurrence is the binding, not a use.
            if byte != decl.decl_byte {
                *ref_count.entry((name.to_string(), decl.decl_byte)).or_insert(0) += 1;
            }
        }
    }
    if cursor.goto_first_child() {
        loop {
            count_identifier_references(cursor, source, scope_tree, ref_count);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

// ---------------------------------------------------------------------------
// P2-3 — function-call argument count/type mismatch
// ---------------------------------------------------------------------------

/// Walk every `function_call` in the tree and compare actual argument
/// count (and, when types are knowable, types) against the resolved
/// callee's `FunctionSignature`s. `@overload` annotations produce
/// alternative signatures; if any one matches the call, no diagnostic
/// is emitted.
///
/// - `self` is implicit for `obj:method(...)` calls and is therefore
///   filtered out of the parameter list before counting.
/// - A vararg trailing param (`...`) absorbs any number of extra
///   arguments; only the required-arg minimum is enforced.
/// - Unknown-typed args (literal expression whose `infer_literal_type`
///   returns `Unknown`) suppress the type mismatch but do not suppress
///   the count check.
fn check_call_argument_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    uri: &Uri,
    index: &mut WorkspaceAggregation,
    diagnostics: &mut Vec<Diagnostic>,
    count_severity: Option<DiagnosticSeverity>,
    type_severity: Option<DiagnosticSeverity>,
) {
    // Depth-first collection of call nodes; we have to collect up front
    // because `resolve_call_signatures` borrows `index` mutably and we
    // can't nest that inside a tree-sitter cursor walk that also owns
    // `root`.
    let mut calls: Vec<tree_sitter::Node> = Vec::new();
    collect_function_calls(root, &mut calls);

    for call in calls {
        let Some((sigs, is_method, display)) =
            crate::signature_help::resolve_call_signatures(call, source, uri, index)
        else {
            continue;
        };
        // After `resolve_call_signatures` returns, the `&mut index`
        // borrow ends (the returned values are owned). We can now
        // take an immutable reference to `index.summaries[uri]` for
        // the type-check path without cloning a full DocumentSummary
        // on every call.
        if sigs.is_empty() {
            continue;
        }
        let Some(args_node) = call.child_by_field_name("arguments") else { continue };
        let (actual_count, arg_exprs) = collect_call_arguments(args_node, source);

        // Count match: any overload compatible with the actual count
        // clears the diagnostic.
        if let Some(severity) = count_severity {
            let any_count_ok = sigs.iter().any(|sig| signature_accepts_count(sig, actual_count, is_method));
            if !any_count_ok {
                // Use the smallest/largest expected count range across
                // overloads for the human-readable message.
                let (min_expected, max_expected) = expected_count_range(&sigs, is_method);
                let range = ts_node_to_range(args_node, source);
                let expected_desc = if min_expected == max_expected {
                    format!("{}", min_expected)
                } else if max_expected == u32::MAX {
                    format!("at least {}", min_expected)
                } else {
                    format!("{} to {}", min_expected, max_expected)
                };
                diagnostics.push(Diagnostic {
                    range,
                    severity: Some(severity),
                    source: Some("mylua".to_string()),
                    message: format!(
                        "Call to '{}' passes {} argument(s), expected {}",
                        display, actual_count, expected_desc,
                    ),
                    ..Default::default()
                });
                // Skip per-arg type checks when count is already wrong —
                // the positional pairing is ambiguous.
                continue;
            }
        }

        // Type match: only when a suitable summary is available (local
        // file) to evaluate argument literal types. For each positional
        // arg i, check against the best matching overload's param i.
        // A single "any overload matches" check keeps behavior
        // consistent with the count pass.
        if let Some(severity) = type_severity {
            let Some(summary) = index.summaries.get(uri) else { continue };
            // Find the first overload whose count is compatible; use
            // its param slots for typing. If multiple overloads match,
            // prefer the one whose param types align most with the
            // provided literal types (best-effort, non-critical).
            let Some(best_sig) = pick_best_typing_overload(&sigs, &arg_exprs, is_method, source, summary) else { continue };
            let visible_params = visible_params_for(&best_sig, is_method);
            for (i, arg_expr) in arg_exprs.iter().enumerate() {
                // Vararg param absorbs everything past its position.
                let param_idx = i;
                let param = match visible_params.get(param_idx) {
                    Some(p) => p,
                    None => break,
                };
                if param.name == "..." {
                    break;
                }
                let actual = infer_argument_type(*arg_expr, source, summary);
                if actual == TypeFact::Unknown {
                    continue;
                }
                if !is_type_compatible(&param.type_fact, &actual) {
                    diagnostics.push(Diagnostic {
                        range: ts_node_to_range(*arg_expr, source),
                        severity: Some(severity),
                        source: Some("mylua".to_string()),
                        message: format!(
                            "Argument {} of '{}': declared '{}', got '{}'",
                            i + 1, display, param.type_fact, actual,
                        ),
                        ..Default::default()
                    });
                }
            }
        }
    }
}

/// Extended version of `infer_literal_type` that also allows
/// `EmmyAnnotation`-sourced locals to contribute their declared
/// type. This is what call-site argument checking wants — if
/// `local s ---@type string = f()` appears, then passing `s` to a
/// `@param n number` slot should be diagnosable. The original
/// `infer_literal_type` deliberately refuses Emmy-sourced locals
/// because the initial `local` declaration's mismatch check would
/// otherwise be circular.
fn infer_argument_type(
    node: tree_sitter::Node,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
) -> TypeFact {
    if matches!(node.kind(), "variable" | "identifier") {
        let text = node_text(node, source);
        if let Some(ltf) = summary.local_type_facts.get(text) {
            return ltf.type_fact.clone();
        }
    }
    infer_literal_type(node, source, summary)
}

fn collect_function_calls<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if node.kind() == "function_call" {
        out.push(node);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_function_calls(child, out);
        }
    }
}

/// Count actual arguments at a `function_call`'s `arguments` node and
/// return the individual argument-expression nodes.
///
/// Three grammar forms:
/// - `( expression_list )` — multi-arg; count the expression_list's
///   named children.
/// - `table_constructor` (`foo{...}`) — 1 arg, the table itself.
/// - `string` (`foo "x"`) — 1 arg, the string literal.
fn collect_call_arguments<'tree>(
    args: tree_sitter::Node<'tree>,
    source: &[u8],
) -> (u32, Vec<tree_sitter::Node<'tree>>) {
    // Paren form only starts with '('; otherwise it's a single-arg
    // form (table / string).
    if source.get(args.start_byte()).copied() != Some(b'(') {
        return (1, vec![args]);
    }
    // Find the `expression_list` named child (optional); if absent the
    // call has zero args.
    let mut exprs = Vec::new();
    for i in 0..args.named_child_count() {
        if let Some(child) = args.named_child(i as u32) {
            if child.kind() == "expression_list" {
                for j in 0..child.named_child_count() {
                    if let Some(e) = child.named_child(j as u32) {
                        exprs.push(e);
                    }
                }
            } else {
                // Some grammars expose args directly without an
                // `expression_list` wrapper; still count each named
                // child as an arg.
                exprs.push(child);
            }
        }
    }
    (exprs.len() as u32, exprs)
}

fn visible_params_for(sig: &crate::type_system::FunctionSignature, is_method: bool) -> Vec<crate::type_system::ParamInfo> {
    sig.params
        .iter()
        .filter(|p| !(is_method && p.name == "self"))
        .cloned()
        .collect()
}

fn signature_accepts_count(sig: &crate::type_system::FunctionSignature, actual: u32, is_method: bool) -> bool {
    let visible = visible_params_for(sig, is_method);
    let has_vararg = visible.last().map_or(false, |p| p.name == "...");
    let declared = visible.len() as u32;
    if has_vararg {
        // `declared - 1` is the count of non-vararg params; vararg
        // absorbs zero or more extras.
        actual >= declared.saturating_sub(1)
    } else {
        actual == declared
    }
}

/// Return the `(min, max)` acceptable argument counts across all
/// overloads, where `max == u32::MAX` indicates at least one overload
/// has a vararg trailing parameter.
fn expected_count_range(sigs: &[crate::type_system::FunctionSignature], is_method: bool) -> (u32, u32) {
    let mut min_acc = u32::MAX;
    let mut max_acc = 0u32;
    let mut any_vararg = false;
    for sig in sigs {
        let visible = visible_params_for(sig, is_method);
        let has_vararg = visible.last().map_or(false, |p| p.name == "...");
        let declared = visible.len() as u32;
        let (lo, hi) = if has_vararg {
            any_vararg = true;
            (declared.saturating_sub(1), u32::MAX)
        } else {
            (declared, declared)
        };
        if lo < min_acc { min_acc = lo; }
        if hi > max_acc { max_acc = hi; }
    }
    if any_vararg {
        (min_acc, u32::MAX)
    } else {
        (min_acc, max_acc)
    }
}

/// Heuristic: among overloads that accept the actual count, pick the
/// one whose first N param types are compatible with the supplied
/// argument literal types. Returns `None` when no overload is a count
/// match — the caller already diagnosed that case.
fn pick_best_typing_overload(
    sigs: &[crate::type_system::FunctionSignature],
    arg_exprs: &[tree_sitter::Node],
    is_method: bool,
    source: &[u8],
    summary: &crate::summary::DocumentSummary,
) -> Option<crate::type_system::FunctionSignature> {
    let actual_count = arg_exprs.len() as u32;
    let candidates: Vec<&crate::type_system::FunctionSignature> = sigs
        .iter()
        .filter(|s| signature_accepts_count(s, actual_count, is_method))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let mut best: Option<(&crate::type_system::FunctionSignature, usize)> = None;
    for sig in candidates {
        let visible = visible_params_for(sig, is_method);
        let mut score = 0usize;
        for (i, arg) in arg_exprs.iter().enumerate() {
            let Some(param) = visible.get(i) else { break };
            if param.name == "..." {
                break;
            }
            let actual = infer_argument_type(*arg, source, summary);
            if actual == TypeFact::Unknown {
                continue;
            }
            if is_type_compatible(&param.type_fact, &actual) {
                score += 1;
            }
        }
        if best.map_or(true, |(_, s)| score > s) {
            best = Some((sig, score));
        }
    }
    best.map(|(s, _)| s.clone())
}

// ---------------------------------------------------------------------------
// P2-3 — @return vs actual return statement mismatch
// ---------------------------------------------------------------------------

/// Walk every function declaration / definition; when preceded by
/// `---@return` annotations, compare against every `return_statement`
/// reachable from the body (including nested `if`/`do`/`while`/`for`
/// / `repeat` blocks). Both count and literal types are checked when
/// statically resolvable.
fn check_return_mismatch_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    let mut functions: Vec<tree_sitter::Node> = Vec::new();
    collect_function_like_nodes(root, &mut functions);
    for fun in functions {
        inspect_function_returns(fun, source, diagnostics, severity);
    }
}

fn collect_function_like_nodes<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if matches!(
        node.kind(),
        "function_declaration" | "local_function_declaration" | "function_definition"
    ) {
        out.push(node);
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_function_like_nodes(child, out);
        }
    }
}

fn inspect_function_returns(
    fun: tree_sitter::Node,
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    // For `function_definition` (anonymous `local f = function() end`
    // or `Class.m = function() end`), the anchor statement (used to
    // locate preceding `---@return` comments) is the enclosing
    // `local_declaration` / `assignment_statement`. For the named
    // forms, the declaration node itself carries the comments.
    let anchor = match fun.kind() {
        "function_definition" => crate::summary_builder::enclosing_statement_for_function_expr(fun)
            .unwrap_or(fun),
        _ => fun,
    };

    let emmy_text = crate::emmy::collect_preceding_comments(anchor, source).join("\n");
    let anns = crate::emmy::parse_emmy_comments(&emmy_text);
    let mut declared_types: Vec<TypeFact> = Vec::new();
    for ann in &anns {
        if let crate::emmy::EmmyAnnotation::Return { return_types, .. } = ann {
            for rt in return_types {
                declared_types.push(crate::emmy::emmy_type_to_fact(rt));
            }
            // All `@return` lines accumulate; each contributes one or
            // more declared types (per EmmyLua convention). A function
            // with `---@return number, string` followed by
            // `---@return Err` declares 3 total return positions.
        }
    }
    if declared_types.is_empty() {
        return;
    }

    let body = match fun.kind() {
        "function_definition" => fun.child_by_field_name("body"),
        _ => fun.child_by_field_name("body"),
    };
    let Some(body) = body else { return };

    let mut returns: Vec<tree_sitter::Node> = Vec::new();
    collect_return_statements(body, &mut returns);

    // A function with `@return` but no `return` anywhere in its body is
    // suspicious but often intentional (stub). Skip unless at least
    // one return is present — better to report concrete mismatches
    // than nag about stubs.
    if returns.is_empty() {
        return;
    }

    for ret in returns {
        inspect_single_return(ret, &declared_types, source, diagnostics, severity);
    }
}

fn collect_return_statements<'tree>(
    node: tree_sitter::Node<'tree>,
    out: &mut Vec<tree_sitter::Node<'tree>>,
) {
    if node.kind() == "return_statement" {
        out.push(node);
        return;
    }
    // Do NOT descend into nested functions — their own `return`
    // statements belong to them, not the outer function.
    if matches!(
        node.kind(),
        "function_declaration" | "local_function_declaration" | "function_definition"
    ) {
        return;
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_return_statements(child, out);
        }
    }
}

fn inspect_single_return(
    ret: tree_sitter::Node,
    declared_types: &[TypeFact],
    source: &[u8],
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    // `return_statement` in our grammar is `'return' optional(expression_list)
    // optional(';')` — no `values` field. Find the `expression_list`
    // child directly; absence means a bare `return` (0 values).
    let values = (0..ret.named_child_count())
        .filter_map(|i| ret.named_child(i as u32))
        .find(|c| c.kind() == "expression_list");
    let actual_count = values.map(|v| v.named_child_count() as u32).unwrap_or(0);
    let declared_count = declared_types.len() as u32;

    // Lua multi-value expansion: a trailing `function_call` or
    // `vararg_expression` at the last return-value position expands
    // into N values at call time. Static count comparison isn't
    // meaningful then — skip count *and* type checks for those
    // returns to avoid flooding opt-in users with false positives
    // from idiomatic `return foo()` / `return ...`.
    if let Some(values) = values {
        if let Some(last) = values
            .named_child(values.named_child_count().saturating_sub(1) as u32)
        {
            if matches!(last.kind(), "function_call" | "vararg_expression") {
                return;
            }
        }
    }

    if actual_count != declared_count {
        diagnostics.push(Diagnostic {
            range: ts_node_to_range(ret, source),
            severity: Some(severity),
            source: Some("mylua".to_string()),
            message: format!(
                "Return statement yields {} value(s), expected {}",
                actual_count, declared_count,
            ),
            ..Default::default()
        });
        return;
    }
    // Count matches — check literal types when resolvable.
    if let Some(values) = values {
        // Use a lightweight inference that mirrors
        // `infer_literal_type` but without summary access; we don't
        // have the per-file summary plumbed here and the walk is
        // already heuristic. Literal nodes cover the common cases.
        for (i, declared) in declared_types.iter().enumerate() {
            let Some(val) = values.named_child(i as u32) else { break };
            let actual = infer_return_literal_type(val);
            if actual == TypeFact::Unknown {
                continue;
            }
            if !is_type_compatible(declared, &actual) {
                diagnostics.push(Diagnostic {
                    range: ts_node_to_range(val, source),
                    severity: Some(severity),
                    source: Some("mylua".to_string()),
                    message: format!(
                        "Return value {}: declared '{}', got '{}'",
                        i + 1, declared, actual,
                    ),
                    ..Default::default()
                });
            }
        }
    }
}

/// Literal-only type inference for return values. Avoids the summary
/// dependency of `infer_literal_type` so this walk can run without a
/// summary in scope (keeps the diagnostic self-contained).
fn infer_return_literal_type(node: tree_sitter::Node) -> TypeFact {
    match node.kind() {
        "number" => TypeFact::Known(KnownType::Number),
        "string" => TypeFact::Known(KnownType::String),
        "true" | "false" => TypeFact::Known(KnownType::Boolean),
        "nil" => TypeFact::Known(KnownType::Nil),
        "table_constructor" => {
            TypeFact::Known(KnownType::Table(crate::table_shape::TableShapeId(u32::MAX)))
        }
        _ => TypeFact::Unknown,
    }
}

// ---------------------------------------------------------------------------
// ---@diagnostic suppression directives
// ---------------------------------------------------------------------------
//
// Lua-LS convention, supported forms (case-sensitive on tag):
//
//   ---@diagnostic disable-next-line
//   ---@diagnostic disable-next-line: undefined-global
//   ---@diagnostic disable-next-line: undefined-global, unused-local
//   ---@diagnostic disable-line
//   ---@diagnostic disable-line: <codes>
//   ---@diagnostic disable                    -- from this line until re-enabled
//   ---@diagnostic disable: <codes>
//   ---@diagnostic enable
//   ---@diagnostic enable: <codes>
//
// `disable` / `enable` pair up file-scoped regions. `disable-next-line` and
// `disable-line` are one-shot. Codes are a comma-separated list of stable
// slugs matching what `classify_diagnostic_code` produces; the special
// slug `*` (or an omitted `:` list) means "all codes".

/// Classify a diagnostic message into a stable rule code slug used by
/// `---@diagnostic disable: <code>` directives. Returned slugs follow
/// the Lua-LS convention so user muscle memory transfers.
pub fn classify_diagnostic_code(message: &str) -> &'static str {
    if message.starts_with("Syntax error") || message.starts_with("Missing '") {
        "syntax"
    } else if message.starts_with("Undefined global") {
        "undefined-global"
    } else if message.starts_with("Unused local") {
        "unused-local"
    } else if message.starts_with("Unknown field") {
        "unknown-field"
    } else if message.starts_with("Type mismatch") {
        "type-mismatch"
    } else if message.starts_with("Duplicate table key") {
        "duplicate-table-key"
    } else if message.starts_with("Call to") && message.contains("argument(s)") {
        "argument-count"
    } else if message.starts_with("Argument ") {
        "argument-type"
    } else if message.starts_with("Return ") {
        "return-mismatch"
    } else {
        "general"
    }
}

/// Apply `---@diagnostic` suppression directives to an already-assembled
/// diagnostic list and stamp each surviving diagnostic's `code` field
/// with its stable slug (handy for client display).
///
/// This is a post-process, safe to call on any mixture of syntax +
/// semantic diagnostics.
pub fn apply_diagnostic_suppressions(
    root: tree_sitter::Node,
    source: &[u8],
    diagnostics: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    let directives = collect_suppression_directives(root, source);
    diagnostics
        .into_iter()
        .filter_map(|mut d| {
            let code = classify_diagnostic_code(&d.message);
            let line = d.range.start.line;
            if is_suppressed(line, code, &directives) {
                return None;
            }
            d.code = Some(NumberOrString::String(code.to_string()));
            Some(d)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectiveKind {
    DisableNextLine,
    DisableLine,
    /// From this line until matching `enable`, for the listed codes.
    Disable,
    /// Re-enable the listed codes.
    Enable,
}

#[derive(Debug, Clone)]
struct Directive {
    kind: DirectiveKind,
    line: u32,
    /// `None` means "all codes" (i.e. no `:` list, or a `*` token).
    codes: Option<Vec<String>>,
}

fn collect_suppression_directives(root: tree_sitter::Node, source: &[u8]) -> Vec<Directive> {
    let mut out = Vec::new();
    collect_directives_recursive(root, source, &mut out);
    // Stable line ordering makes the enable/disable scoping pass
    // deterministic even across tree-sitter's unspecified emmy_line
    // sibling order (it's usually source order already, but be safe).
    out.sort_by_key(|d| d.line);
    out
}

fn collect_directives_recursive(
    node: tree_sitter::Node,
    source: &[u8],
    out: &mut Vec<Directive>,
) {
    if node.kind() == "emmy_line" {
        if let Some(d) = parse_directive_from_emmy_line(node, source) {
            out.push(d);
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i as u32) {
            collect_directives_recursive(child, source, out);
        }
    }
}

fn parse_directive_from_emmy_line(
    line_node: tree_sitter::Node,
    source: &[u8],
) -> Option<Directive> {
    let raw = node_text(line_node, source);
    // Trim leading `---` / `--`, tabs / spaces.
    let trimmed = raw.trim_start_matches('-').trim();
    // Must be an `@diagnostic` annotation.
    let rest = trimmed.strip_prefix("@diagnostic")?.trim_start();
    // Split on first `:` (optional) to get `(tag, codes_list)`.
    let (tag_raw, codes_raw) = match rest.find(':') {
        Some(i) => (rest[..i].trim(), Some(rest[i + 1..].trim())),
        None => (rest.trim(), None),
    };
    let kind = match tag_raw {
        "disable-next-line" => DirectiveKind::DisableNextLine,
        "disable-line" => DirectiveKind::DisableLine,
        "disable" => DirectiveKind::Disable,
        "enable" => DirectiveKind::Enable,
        _ => return None, // unknown tag — ignore silently
    };
    let codes = codes_raw.and_then(|s| {
        if s.is_empty() {
            return None;
        }
        let list: Vec<String> = s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if list.is_empty() {
            None
        } else if list.iter().any(|t| t == "*") {
            None // `*` means all — same as no list
        } else {
            Some(list)
        }
    });
    Some(Directive {
        kind,
        line: line_node.start_position().row as u32,
        codes,
    })
}

/// Decide whether a diagnostic at `(line, code)` is suppressed by
/// the set of directives. Rules:
/// - `disable-next-line` at line L suppresses diagnostics on L+1.
/// - `disable-line` at line L suppresses diagnostics on L.
/// - `disable` at line L suppresses from L onward until a matching
///   `enable` directive. Matching: the disable applies only to codes
///   listed (or all codes if no list); a subsequent `enable` with an
///   overlapping code list clears those codes from that line onward.
fn is_suppressed(target_line: u32, code: &str, directives: &[Directive]) -> bool {
    // Walk directives in line order, maintaining a per-code "disabled
    // since this line" map. `*` disable disables everything.
    use std::collections::HashMap;

    let mut disabled_since: HashMap<String, u32> = HashMap::new();
    let mut all_disabled_since: Option<u32> = None;
    let all_key = "*"; // sentinel only used internally
    let _ = all_key;

    for d in directives {
        if d.line > target_line {
            // Directives after the target can only be of interest if
            // they're `disable-next-line` or `disable-line` — handled
            // separately below. For file-scoped disable/enable walk
            // we stop here.
            break;
        }
        match d.kind {
            DirectiveKind::Disable => {
                match &d.codes {
                    None => all_disabled_since = Some(d.line),
                    Some(list) => {
                        for c in list {
                            disabled_since.insert(c.clone(), d.line);
                        }
                    }
                }
            }
            DirectiveKind::Enable => {
                match &d.codes {
                    None => {
                        all_disabled_since = None;
                        disabled_since.clear();
                    }
                    Some(list) => {
                        for c in list {
                            disabled_since.remove(c);
                        }
                        // A per-code `enable` also pierces an `all_disabled`
                        // region for that specific code: stash a
                        // `enabled_since` marker by removing from the
                        // `disabled_since` map and leaving all_disabled_since
                        // alone — then below we check code-specific first.
                    }
                }
            }
            _ => {}
        }
    }

    // File-scoped check: does some active disable cover this code?
    if let Some(line) = disabled_since.get(code) {
        if *line <= target_line {
            return true;
        }
    }
    if let Some(line) = all_disabled_since {
        if line <= target_line {
            // Honor a later per-code `enable` by re-scanning: if a
            // pre-target `Enable` directive lists this code and sits
            // after the global disable, treat as enabled.
            let mut enabled_after_disable = false;
            for d in directives {
                if d.line > target_line {
                    break;
                }
                if d.line < line {
                    continue;
                }
                if d.kind == DirectiveKind::Enable {
                    match &d.codes {
                        None => {
                            enabled_after_disable = true;
                        }
                        Some(list) if list.iter().any(|c| c == code) => {
                            enabled_after_disable = true;
                        }
                        _ => {}
                    }
                }
            }
            if !enabled_after_disable {
                return true;
            }
        }
    }

    // One-shot line directives: scan every directive for a
    // disable-line/disable-next-line matching `target_line`.
    for d in directives {
        let covers_target = match d.kind {
            DirectiveKind::DisableNextLine => d.line + 1 == target_line,
            DirectiveKind::DisableLine => d.line == target_line,
            _ => false,
        };
        if !covers_target {
            continue;
        }
        match &d.codes {
            None => return true, // all codes
            Some(list) if list.iter().any(|c| c == code) => return true,
            _ => {}
        }
    }

    false
}

#[cfg(test)]
mod directive_tests {
    use super::*;

    #[test]
    fn classify_covers_major_rules() {
        assert_eq!(classify_diagnostic_code("Undefined global 'x'"), "undefined-global");
        assert_eq!(classify_diagnostic_code("Unused local 'y'"), "unused-local");
        assert_eq!(classify_diagnostic_code("Unknown field 'foo' on type 'Bar'"), "unknown-field");
        assert_eq!(classify_diagnostic_code("Type mismatch: declared 'X', got 'Y'"), "type-mismatch");
        assert_eq!(classify_diagnostic_code("Duplicate table key 'a' (first defined at line 2)"), "duplicate-table-key");
        assert_eq!(classify_diagnostic_code("Call to 'foo' passes 3 argument(s), expected 2"), "argument-count");
        assert_eq!(classify_diagnostic_code("Argument 1 of 'foo': declared 'X', got 'Y'"), "argument-type");
        assert_eq!(classify_diagnostic_code("Return statement yields 1 value(s), expected 2"), "return-mismatch");
        assert_eq!(classify_diagnostic_code("Syntax error near 'foo'"), "syntax");
    }
}
