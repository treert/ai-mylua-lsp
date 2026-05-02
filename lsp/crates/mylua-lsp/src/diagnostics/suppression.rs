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

use crate::util::node_text;
use tower_lsp_server::ls_types::*;

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
    } else if message.starts_with("@param ") {
        "param-annotation"
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

fn collect_directives_recursive(node: tree_sitter::Node, source: &[u8], out: &mut Vec<Directive>) {
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
        if list.is_empty() || list.iter().any(|t| t == "*") {
            None
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
            DirectiveKind::Disable => match &d.codes {
                None => all_disabled_since = Some(d.line),
                Some(list) => {
                    for c in list {
                        disabled_since.insert(c.clone(), d.line);
                    }
                }
            },
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
        assert_eq!(
            classify_diagnostic_code("Undefined global 'x'"),
            "undefined-global"
        );
        assert_eq!(classify_diagnostic_code("Unused local 'y'"), "unused-local");
        assert_eq!(
            classify_diagnostic_code("Unknown field 'foo' on type 'Bar'"),
            "unknown-field"
        );
        assert_eq!(
            classify_diagnostic_code("Type mismatch: declared 'X', got 'Y'"),
            "type-mismatch"
        );
        assert_eq!(
            classify_diagnostic_code("Duplicate table key 'a' (first defined at line 2)"),
            "duplicate-table-key"
        );
        assert_eq!(
            classify_diagnostic_code("Call to 'foo' passes 3 argument(s), expected 2"),
            "argument-count"
        );
        assert_eq!(
            classify_diagnostic_code("Argument 1 of 'foo': declared 'X', got 'Y'"),
            "argument-type"
        );
        assert_eq!(
            classify_diagnostic_code("Return statement yields 1 value(s), expected 2"),
            "return-mismatch"
        );
        assert_eq!(
            classify_diagnostic_code("@param 'x' does not match any Lua parameter"),
            "param-annotation"
        );
        assert_eq!(
            classify_diagnostic_code("Syntax error near 'foo'"),
            "syntax"
        );
    }
}
