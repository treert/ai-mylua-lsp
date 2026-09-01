//! Suppress "this name/field does not exist" diagnostics at reads the
//! author already guarded with an existence check.
//!
//! Motivating case: a host application (typically C++) registers symbols
//! into the Lua global table at run time. The workspace has no definition
//! for them, so scripts probe before use:
//!
//! ```lua
//! if gg_cpp_registered then
//!     print(gg_cpp_registered)     -- guarded: do not report
//! end
//! ```
//!
//! Reporting inside the guarded branch is pure noise — the check *is* the
//! author saying "this may not exist". A wall of such warnings gets the
//! whole diagnostic category switched off, which is far worse than the
//! occasional missed report.
//!
//! This is deliberately **not** type narrowing. Nothing is written back
//! into the type system: the name keeps whatever type inference gave it
//! (usually `Unknown`), and only the diagnostic is dropped. Writing a
//! complete annotation (`---@class` / a `---@meta` stub) stays the one
//! correct way to actually get types — this only removes noise for code
//! that has not got there yet.
//!
//! # Scope
//!
//! Guards are found by walking **ancestors of the read**, which is
//! syntax-directed rather than a control-flow analysis. Recognized:
//!
//! | Form | Guarded region | Polarity |
//! |---|---|---|
//! | `if C then …` | the `then` body | positive |
//! | `elseif C then …` | that `elseif` body | positive |
//! | `else …` | the `else` body | negated (all preceding conditions) |
//! | `while C do …` | the loop body | positive |
//! | `A and B` | `B` | positive (for paths in `A`) |
//! | the condition itself | — | always suppressed |
//!
//! A condition contributes a guard when it tests a path for existence:
//! `P`, `P ~= nil`, `nil ~= P`, `not P`, `P == nil`, `nil == P`. `not` and
//! `== nil` flip the polarity, so `if not P then … else <here> end` is
//! guarded just like `if P then <here> end`.
//!
//! # Where this stops, and why
//!
//! Requiring the read to be lexically nested inside the guarded region —
//! reachable by walking ancestors — is the deliberate stopping point, not a
//! staging post. Several common idioms establish existence for their
//! *following siblings* instead, and are knowingly left unsupported:
//!
//! ```lua
//! if not P then return end     -- early return
//! assert(P)                    -- assert
//! if not P then P = {} end     -- lazy init
//! ```
//!
//! All three would need statement-order data flow: a fact set accumulated
//! across sibling statements, a "does this branch definitely terminate"
//! analysis (`return` / `break` / `error()` / all-branches-terminate), and a
//! rule for how far a fact escapes its block. They share most of that
//! machinery, so supporting one sensibly means supporting all.
//!
//! They are declined on product grounds rather than difficulty: the more
//! existence idioms are suppressed, the less reason anyone has to write the
//! annotation that would actually give them types. Annotations (`---@class`,
//! `---@meta`) are exact and predictable; suppression is inherently
//! heuristic, and Lua admits unboundedly many ways to say "this might not
//! exist", so every extra form recognized here is also an extra way to hide
//! a real bug. Clearing the obvious noise is worth it; making skipped
//! annotations free is not.
//!
//! Also unsupported, for their own reasons: `or` right-hand sides (`a or b`
//! evaluates `b` precisely when `a` was falsy, so `a` guarantees nothing
//! there), `repeat … until C` (the condition runs *after* the body, so it
//! guards nothing — permanent, not a limitation), guards stored in an
//! intermediate variable, and `a[b]` subscript paths (a non-constant
//! subscript has no stable key).
//!
//! # Cost
//!
//! Runs as a post-process over the assembled diagnostic list, so a file
//! with no candidate diagnostics pays nothing at all. Per surviving
//! diagnostic the cost is one ancestor walk (depth is small in practice).
//! Nothing is pre-collected.

use crate::syntax_kind::{field, kind, NodeKindExt};
use crate::util::{extract_field_chain, node_text, LineIndex};
use tower_lsp_server::ls_types::*;

/// Diagnostic codes this pass can suppress. Both mean "the thing you are
/// reading has no definition we can see", which is exactly the claim an
/// existence check refutes. Other codes (type mismatches, arity errors, …)
/// stay untouched: a guard says nothing about them.
const GUARDABLE_CODES: [&str; 2] = ["undefined-global", "unknown-field"];

/// Drop diagnostics whose subject is guarded by an enclosing existence
/// check. `diagnostics` may hold any mix of syntax and semantic entries;
/// only the guardable codes are considered.
pub fn filter_guarded_diagnostics(
    root: tree_sitter::Node,
    source: &[u8],
    line_index: &LineIndex,
    diagnostics: Vec<Diagnostic>,
) -> Vec<Diagnostic> {
    diagnostics
        .into_iter()
        .filter(|d| !is_guarded_diagnostic(root, source, line_index, d))
        .collect()
}

fn is_guarded_diagnostic(
    root: tree_sitter::Node,
    source: &[u8],
    line_index: &LineIndex,
    diagnostic: &Diagnostic,
) -> bool {
    let code = super::suppression::classify_diagnostic_code(&diagnostic.message);
    if !GUARDABLE_CODES.contains(&code) {
        return false;
    }

    // Recover the offending node from the reported range. Diagnostics
    // point at the identifier (`undefined-global`) or at the field token
    // (`unknown-field`), so the range start always lands on a name.
    let Some(offset) = line_index.position_to_byte_offset(source, diagnostic.range.start) else {
        return false;
    };
    let Some(node) = root.descendant_for_byte_range(offset, offset) else {
        return false;
    };

    let Some(path) = guarded_path_at(node, source) else {
        return false;
    };
    path_is_guarded(node, &path, source)
}

/// The dotted access path the diagnostic is about, outermost-first
/// (`x.m_some` → `["x", "m_some"]`).
///
/// For `unknown-field` the reported node is the field token, whose parent
/// `variable` carries the whole chain; for `undefined-global` it is a bare
/// identifier and the path is just its text. Subscripts are rejected:
/// `extract_field_chain` only follows `object`/`field` pairs, and a base
/// that is not a plain name yields no usable key.
fn guarded_path_at(node: tree_sitter::Node, source: &[u8]) -> Option<Vec<String>> {
    // Walk up to the widest `variable` that still ends at this node, so
    // `a.b.c` reports on `c` but is keyed by the full chain — while
    // `a.b.c` reporting on `b` is keyed by `a.b`, not `a.b.c`.
    let mut chain_node = node;
    while let Some(parent) = chain_node.parent() {
        if !parent.is_kind(kind::VARIABLE) || parent.end_byte() != chain_node.end_byte() {
            break;
        }
        chain_node = parent;
    }

    if chain_node.is_kind(kind::VARIABLE) && chain_node.child_by_field(field::FIELD).is_some() {
        let (base, fields) = extract_field_chain(chain_node, source)?;
        if !matches!(base.syntax_kind(), kind::IDENTIFIER | kind::VARIABLE) {
            return None;
        }
        // A subscripted or called base (`t[i].f`, `f().g`) has no stable
        // textual key.
        if base.child_by_field(field::INDEX).is_some() {
            return None;
        }
        let base_text = node_text(base, source);
        if !is_plain_name(base_text) {
            return None;
        }
        let mut path = Vec::with_capacity(fields.len() + 1);
        path.push(base_text.to_string());
        path.extend(fields.iter().cloned());
        return Some(path);
    }

    if node.is_kind(kind::IDENTIFIER) {
        return Some(vec![node_text(node, source).to_string()]);
    }
    None
}

fn is_plain_name(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(|c: char| c.is_ascii_digit())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `path`, read at `node`, sits in a region guarded by a check on
/// that same path.
fn path_is_guarded(node: tree_sitter::Node, path: &[String], source: &[u8]) -> bool {
    let mut child = node;
    while let Some(parent) = child.parent() {
        // A read *inside* a condition is the check itself, so polarity is
        // irrelevant there: writing `if X == nil then` is just as much an
        // admission that `X` may be missing as `if X then` is. Testing this
        // before the polarity-sensitive cases keeps `if not X then` from
        // reporting on its own condition.
        if is_condition_of(parent, child) {
            if condition_tests_path(parent.child_by_field(field::CONDITION), path, source) {
                return true;
            }
            child = parent;
            continue;
        }

        let guarded = match parent.syntax_kind() {
            kind::IF_STATEMENT => if_statement_guards(parent, child, path, source),
            kind::ELSEIF_CLAUSE => {
                // Reaching an `elseif` from inside means the read is in its
                // body, so its own condition guards it. The accumulated
                // negation of preceding conditions is handled when this
                // clause's parent `if_statement` is visited on a later
                // iteration.
                condition_guards_path(parent.child_by_field(field::CONDITION), path, source, true)
            }
            kind::WHILE_STATEMENT => {
                condition_guards_path(parent.child_by_field(field::CONDITION), path, source, true)
            }
            kind::BINARY_EXPRESSION => binary_guards(parent, child, path, source),
            _ => false,
        };
        if guarded {
            return true;
        }
        child = parent;
    }
    false
}

/// Whether `child` is the `condition` of `parent`. Covers every construct
/// with a `condition` field — `if` / `elseif` / `while` / `repeat … until`.
///
/// `until` guards nothing (it runs after the body), but a read in the
/// `until` expression is still a check on that path, so it is silenced
/// here like any other condition.
fn is_condition_of(parent: tree_sitter::Node, child: tree_sitter::Node) -> bool {
    matches!(
        parent.syntax_kind(),
        kind::IF_STATEMENT | kind::ELSEIF_CLAUSE | kind::WHILE_STATEMENT | kind::REPEAT_STATEMENT
    ) && parent
        .child_by_field(field::CONDITION)
        .is_some_and(|cond| cond.id() == child.id())
}

/// `if C then …` / `else …`: the leading condition guards the `then` body
/// and (negated) the `else` body; preceding `elseif` conditions also guard
/// the `else` body once negated.
fn if_statement_guards(
    if_node: tree_sitter::Node,
    child: tree_sitter::Node,
    path: &[String],
    source: &[u8],
) -> bool {
    let in_else = child.is_kind(kind::ELSE_CLAUSE);

    // The leading `if` condition: positive for the `then` body and for the
    // condition itself, negated for the `else` body.
    if condition_guards_path(
        if_node.child_by_field(field::CONDITION),
        path,
        source,
        !in_else,
    ) {
        return true;
    }

    // `else` is only reached once every `elseif` condition also failed, so
    // each of those contributes a negated guard too.
    if in_else {
        let mut scan = if_node.walk();
        if scan.goto_first_child() {
            loop {
                let clause = scan.node();
                if clause.is_kind(kind::ELSEIF_CLAUSE)
                    && condition_guards_path(
                        clause.child_by_field(field::CONDITION),
                        path,
                        source,
                        false,
                    )
                {
                    return true;
                }
                if !scan.goto_next_sibling() {
                    break;
                }
            }
        }
    }
    false
}

/// `A and B` guards `B` with `A`, and `A` itself is the check.
fn binary_guards(
    bin: tree_sitter::Node,
    child: tree_sitter::Node,
    path: &[String],
    source: &[u8],
) -> bool {
    let Some(op) = bin.child_by_field(field::OPERATOR) else {
        return false;
    };
    if node_text(op, source) != "and" {
        return false;
    }
    let Some(left) = bin.child_by_field(field::LEFT) else {
        return false;
    };

    // The read is in `A`: that *is* the existence check, so polarity does
    // not apply — same reasoning as a read inside an `if` condition.
    if left.id() == child.id() {
        return condition_tests_path(Some(left), path, source);
    }

    // The read is in `B`, reached only when `A` held.
    let Some(right) = bin.child_by_field(field::RIGHT) else {
        return false;
    };
    if right.id() != child.id() {
        return false;
    }
    condition_guards_path(Some(left), path, source, true)
}

/// Whether `condition` probes `path` for existence at all, ignoring which
/// way the test points. Used where the read sits inside the check itself.
fn condition_tests_path(
    condition: Option<tree_sitter::Node>,
    path: &[String],
    source: &[u8],
) -> bool {
    let Some(condition) = condition else {
        return false;
    };
    match classify_condition(condition, source) {
        Some((tested, _)) => same_path(&tested, path),
        None => false,
    }
}

/// Whether `condition` tests `path` for existence with the requested
/// polarity. `want_truthy` is true for regions entered when the condition
/// held, false for regions entered when it failed.
fn condition_guards_path(
    condition: Option<tree_sitter::Node>,
    path: &[String],
    source: &[u8],
    want_truthy: bool,
) -> bool {
    let Some(condition) = condition else {
        return false;
    };
    match classify_condition(condition, source) {
        Some((tested, truthy_means_exists)) => {
            same_path(&tested, path) && truthy_means_exists == want_truthy
        }
        None => false,
    }
}

/// Decompose an existence test into the path it probes and whether a
/// *truthy* condition means "the path exists".
///
/// `P` / `P ~= nil` → `(P, true)`; `not P` / `P == nil` → `(P, false)`.
fn classify_condition(node: tree_sitter::Node, source: &[u8]) -> Option<(Vec<String>, bool)> {
    match node.syntax_kind() {
        kind::PARENTHESIZED_EXPRESSION => {
            classify_condition(node.named_child(0)?, source)
        }
        kind::UNARY_EXPRESSION => {
            // `operator` / `operand` are both named children, so the
            // operand has to be reached through its field rather than by
            // child index.
            let op = node.child_by_field(field::OPERATOR)?;
            if node_text(op, source) != "not" {
                return None;
            }
            let (path, truthy) =
                classify_condition(node.child_by_field(field::OPERAND)?, source)?;
            Some((path, !truthy))
        }
        kind::BINARY_EXPRESSION => {
            let op = node.child_by_field(field::OPERATOR)?;
            let op_text = node_text(op, source);
            let truthy_means_exists = match op_text {
                "~=" => true,
                "==" => false,
                // `A and B` as a whole is not itself an existence test for
                // a single path; each operand is examined separately by
                // `binary_guards` / the ancestor walk.
                _ => return None,
            };
            let left = node.child_by_field(field::LEFT)?;
            let right = node.child_by_field(field::RIGHT)?;
            // Exactly one side must be the literal `nil`.
            let left_nil = left.is_kind(kind::NIL);
            let right_nil = right.is_kind(kind::NIL);
            let subject = match (left_nil, right_nil) {
                (true, false) => right,
                (false, true) => left,
                _ => return None,
            };
            Some((path_of_expression(subject, source)?, truthy_means_exists))
        }
        _ => path_of_expression(node, source).map(|path| (path, true)),
    }
}

/// The dotted path an expression names, or `None` when it is not a plain
/// name / dotted chain (calls, subscripts, literals, …).
fn path_of_expression(node: tree_sitter::Node, source: &[u8]) -> Option<Vec<String>> {
    if node.is_kind(kind::PARENTHESIZED_EXPRESSION) {
        return path_of_expression(node.named_child(0)?, source);
    }
    if node.is_kind(kind::IDENTIFIER) {
        let text = node_text(node, source);
        return is_plain_name(text).then(|| vec![text.to_string()]);
    }
    if node.is_kind(kind::VARIABLE) {
        if node.child_by_field(field::FIELD).is_none() {
            // A bare `variable` wrapping a lone identifier.
            let inner = node.named_child(0)?;
            if inner.is_kind(kind::IDENTIFIER) {
                let text = node_text(inner, source);
                return is_plain_name(text).then(|| vec![text.to_string()]);
            }
            return None;
        }
        if node.child_by_field(field::INDEX).is_some() {
            return None;
        }
        let (base, fields) = extract_field_chain(node, source)?;
        if base.child_by_field(field::INDEX).is_some() {
            return None;
        }
        let base_text = node_text(base, source);
        if !is_plain_name(base_text) {
            return None;
        }
        let mut path = Vec::with_capacity(fields.len() + 1);
        path.push(base_text.to_string());
        path.extend(fields.iter().cloned());
        return Some(path);
    }
    None
}

/// Whether a guard on `tested` covers a read of `read`.
///
/// A guard covers the path it tested and anything beneath it: checking
/// `x.cfg` vouches for `x.cfg` and for `x.cfg.opt`, but a check on
/// `x.cfg.opt` says nothing about a bare `x.cfg` read.
fn same_path(tested: &[String], read: &[String]) -> bool {
    tested.len() <= read.len() && tested.iter().zip(read).all(|(a, b)| a == b)
}
