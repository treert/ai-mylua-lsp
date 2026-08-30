//! Shared node-shape predicates for free-name diagnostics.
//!
//! A free name `x` is `_ENV.x` by definition (Lua 5.2+), so the two checks
//! that reason about free names — `undefinedGlobal` (environment is the global
//! one) and `envUnknownField` (environment redirected elsewhere) — must agree
//! on *exactly* which `identifier` nodes count. The fiddly part is
//! `function_name`, whose three forms differ in whether the base identifier is
//! a definition or a read; keeping the predicate in one place stops the two
//! checks from drifting apart on it.

use crate::syntax_kind::{field, kind, NodeKindExt};
use crate::util::is_ancestor_or_equal;

/// True if `function_name` contains any `.` or `:` separator — i.e.
/// the form is `foo.bar(...)` / `foo:m(...)` rather than the bare
/// `foo(...)`. In those cases the first identifier is a read of an
/// existing table, not a global definition.
fn function_name_has_path_separator(function_name: tree_sitter::Node) -> bool {
    for i in 0..function_name.child_count() {
        if let Some(child) = function_name.child(i as u32) {
            if !child.is_named() && (child.is_kind(kind::DOT) || child.is_kind(kind::COLON)) {
                return true;
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
            if child.is_kind(kind::IDENTIFIER) {
                return child.id() == ident.id();
            }
        }
    }
    false
}

/// True if `node` is an `identifier` occupying a *free-name reference*
/// position — a bare (unqualified) name that the environment has to supply.
///
/// Covers two shapes:
/// - a bare `variable` (single identifier child), excluding declaration sites
///   (`local` name lists, `goto` labels);
/// - the base identifier of a dotted / method `function_name`
///   (`function foo.bar()` reads `foo`). The bare form `function foo()` is a
///   *definition* of `foo`, not a reference, so it is excluded.
///
/// Note this includes assignment targets (`x = 1`): they are references as far
/// as `undefinedGlobal` is concerned, because the assignment itself is what
/// defines the name. Callers that need reads only must additionally reject
/// [`is_assignment_target`].
pub(super) fn is_free_name_reference(node: tree_sitter::Node) -> bool {
    if !node.is_kind(kind::IDENTIFIER) {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    let is_bare_var = parent.is_kind(kind::VARIABLE) && parent.child_count() == 1;
    let is_definition = matches!(
        parent.syntax_kind(),
        kind::ATTRIBUTE_NAME_LIST | kind::NAME_LIST | kind::LABEL_STATEMENT
    );
    // `function_name` covers three forms with very different
    // semantics w.r.t. the *base* identifier:
    //   `function foo()`      → defines global `foo`
    //   `function foo.bar()`  → assigns `foo.bar`, reads `foo`
    //   `function foo:m()`    → assigns `foo.m`,    reads `foo`
    // Only the bare form is a definition; the dotted / method
    // forms require `foo` to already exist at runtime, so the
    // base identifier must participate. Later identifiers
    // (`bar`, `m`) are field writes — skip them.
    let is_function_name_child = parent.is_kind(kind::FUNCTION_NAME);
    let is_reference = is_bare_var
        || (is_function_name_child
            && is_function_name_base(parent, node)
            && function_name_has_path_separator(parent));
    is_reference && !is_definition
}

/// Returns true if `node` is (or is any descendant of) the left-hand side
/// of an assignment statement. Walks ancestors so that chained LHS like
/// `a.b.c = 1` — where the outer node is `variable` and an inner
/// dotted `variable` node is not a direct child of `variable_list` —
/// is still recognized as an assignment target.
pub(super) fn is_assignment_target(node: tree_sitter::Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.is_kind(kind::ASSIGNMENT_STATEMENT) {
            // `current` is always an ancestor of (or equal to) `node`,
            // so `is_ancestor_or_equal(left, node)` already covers the
            // `left == current` case.
            return parent
                .child_by_field(field::LEFT)
                .is_some_and(|left| is_ancestor_or_equal(left, node));
        }
        current = parent;
    }
    false
}
