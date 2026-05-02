use crate::aggregation::WorkspaceAggregation;
use crate::scope::ScopeTree;
use crate::util::{node_text, LineIndex};
use std::collections::HashSet;
use tower_lsp_server::ls_types::*;

/// True if `function_name` contains any `.` or `:` separator — i.e.
/// the form is `foo.bar(...)` / `foo:m(...)` rather than the bare
/// `foo(...)`. In those cases the first identifier is a read of an
/// existing table, not a global definition.
fn function_name_has_path_separator(function_name: tree_sitter::Node) -> bool {
    for i in 0..function_name.child_count() {
        if let Some(child) = function_name.child(i as u32) {
            if !child.is_named() {
                let kind = child.kind();
                if kind == "." || kind == ":" {
                    return true;
                }
            }
        }
    }
    false
}

/// True if `ident` is the first (leftmost) identifier child of a
/// `function_name` node — i.e. the base table name, not a field or
/// method name.
fn is_function_name_base(function_name: tree_sitter::Node, ident: tree_sitter::Node) -> bool {
    for i in 0..function_name.child_count() {
        if let Some(child) = function_name.child(i as u32) {
            if child.kind() == "identifier" {
                return child.id() == ident.id();
            }
        }
    }
    false
}

pub(super) fn check_undefined_globals(
    cursor: &mut tree_sitter::TreeCursor,
    source: &[u8],
    builtins: &HashSet<&str>,
    index: &WorkspaceAggregation,
    scope_tree: &ScopeTree,
    diagnostics: &mut Vec<Diagnostic>,
    severity: DiagnosticSeverity,
    line_index: &LineIndex,
) {
    let node = cursor.node();

    if node.kind() == "identifier" {
        if let Some(parent) = node.parent() {
            let is_bare_var = parent.kind() == "variable" && parent.child_count() == 1;
            let is_definition = matches!(
                parent.kind(),
                "attribute_name_list" | "name_list" | "label_statement"
            );
            // `function_name` covers three forms with very different
            // semantics w.r.t. the *base* identifier:
            //   `function foo()`      → defines global `foo`
            //   `function foo.bar()`  → assigns `foo.bar`, reads `foo`
            //   `function foo:m()`    → assigns `foo.m`,    reads `foo`
            // Only the bare form is a definition; the dotted / method
            // forms require `foo` to already exist at runtime, so the
            // base identifier must participate in the undefined-global
            // check. Later identifiers (`bar`, `m`) are field writes —
            // skip them.
            let is_function_name_child = parent.kind() == "function_name";
            let should_check_as_ref = is_bare_var
                || (is_function_name_child
                    && is_function_name_base(parent, node)
                    && function_name_has_path_separator(parent));
            if should_check_as_ref && !is_definition {
                let name = node_text(node, source);
                let byte_offset = node.start_byte();
                let is_local = scope_tree.resolve_decl(byte_offset, name).is_some();
                if !is_local && !builtins.contains(name) && !index.global_shard.contains_key(name) {
                    diagnostics.push(Diagnostic {
                        range: line_index.ts_node_to_range(node, source),
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
            // Skip bracket-key-only table constructors — they contain
            // no identifiers, only literal key-value pairs.
            let child = cursor.node();
            if child.kind() == "table_constructor" && crate::util::is_bracket_key_only_table(child)
            {
                if !cursor.goto_next_sibling() {
                    break;
                }
                continue;
            }
            check_undefined_globals(
                cursor,
                source,
                builtins,
                index,
                scope_tree,
                diagnostics,
                severity,
                line_index,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
