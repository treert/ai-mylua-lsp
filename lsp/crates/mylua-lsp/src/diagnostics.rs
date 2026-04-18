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
    if let Some(severity) = diag_config.undefined_global.to_lsp_severity() {
        check_undefined_globals(&mut cursor, source, &builtins, index, scope_tree, &mut diagnostics, severity);
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
        check_type_mismatch_diagnostics(root, source, uri, index, &mut diagnostics, severity);
    }
    if let Some(severity) = diag_config.duplicate_table_key.to_lsp_severity() {
        check_duplicate_table_keys(root, source, &mut diagnostics, severity);
    }
    if let Some(severity) = diag_config.unused_local.to_lsp_severity() {
        check_unused_locals(root, source, scope_tree, &mut diagnostics, severity);
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
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
) {
    if let Some(summary) = index.summaries.get(uri).cloned() {
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
    }
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
