use crate::emmy::{emmy_type_to_fact, parse_emmy_comments, EmmyAnnotation};
use crate::table_shape::{FieldInfo, TableShape, MAX_TABLE_SHAPE_DEPTH};
use crate::type_system::*;
use crate::util::{extract_string_literal, node_text};

use super::fingerprint::merge_types;
use super::type_infer::infer_expression_type;
use super::BuildContext;

// ---------------------------------------------------------------------------
// Table shape extraction
// ---------------------------------------------------------------------------

pub(super) fn extract_table_shape(
    ctx: &mut BuildContext,
    constructor: tree_sitter::Node,
    shape: &mut TableShape,
    depth: usize,
) {
    if depth > MAX_TABLE_SHAPE_DEPTH {
        shape.truncated = true;
        return;
    }

    // Grammar (see grammar/grammar.js):
    //   table_constructor → `{` field_list? `}`
    //   field_list        → field (sep field)* sep?
    //   field             → `[` key=_expression `]` `=` value=_expression
    //                     | key=identifier `=` value=_expression
    //                     | value=_expression
    //
    // So the entries live under a `field_list` wrapper, not as direct
    // children of `table_constructor`. Previously this function looked
    // for `field` nodes directly under the constructor, which meant
    // EVERY literal ended up with an empty `fields` map — turning every
    // `t.x` read into a false-positive "Unknown field" diagnostic.
    //
    // We also read the CST field `key` (not the historical name `name`)
    // as defined by the grammar's `field` rule.

    for i in 0..constructor.named_child_count() {
        let Some(field_list) = constructor.named_child(i as u32) else {
            continue;
        };
        if field_list.kind() != "field_list" {
            continue;
        }
        for j in 0..field_list.named_child_count() {
            let Some(field_node) = field_list.named_child(j as u32) else {
                continue;
            };
            if field_node.kind() != "field" {
                continue;
            }
            extract_single_field(ctx, field_node, shape, depth);
        }
    }
}

fn extract_single_field(
    ctx: &mut BuildContext,
    field_node: tree_sitter::Node,
    shape: &mut TableShape,
    depth: usize,
) {
    let value_node = field_node.child_by_field_name("value");
    let key_node = field_node.child_by_field_name("key");

    match key_node {
        // `name = value` — key is an identifier → named field.
        Some(k) if k.kind() == "identifier" => {
            let key = node_text(k, ctx.source).to_string();
            if let Some(val) = value_node {
                let type_fact = infer_field_value_type(ctx, field_node, val, depth);
                shape.set_field(
                    &key,
                    FieldInfo {
                        name: key.as_str().into(),
                        type_fact,
                        def_range: Some(
                            ctx.line_index.ts_node_to_byte_range(field_node, ctx.source),
                        ),
                        assignment_count: 1,
                    },
                );
            }
        }
        // `["name"] = value` — static string key that can be read via
        // dot syntax (`t.name`). Non-identifier strings remain map-like
        // entries rather than polluting the named-field table.
        Some(k) if k.kind() == "string" => {
            let key_text = extract_string_literal(k, ctx.source)
                .unwrap_or_else(|| node_text(k, ctx.source).to_string());
            if is_lua_identifier_key(&key_text) {
                if let Some(val) = value_node {
                    let type_fact = infer_field_value_type(ctx, field_node, val, depth);
                    shape.set_field(
                        &key_text,
                        FieldInfo {
                            name: key_text.as_str().into(),
                            type_fact,
                            def_range: Some(
                                ctx.line_index.ts_node_to_byte_range(field_node, ctx.source),
                            ),
                            assignment_count: 1,
                        },
                    );
                }
            }
        }
        // `[number] = value` is a static subscript key, but not a named
        // field for dot access.
        Some(k) if k.kind() == "number" => {
            let _ = k;
        }
        // `[expr] = value` with a non-literal key — dynamic bracket
        // write. Mark the shape as open and roll the value type into
        // `array_element_type` so `t[i]` subscript reads still pick up
        // a hint.
        Some(_) => {
            shape.mark_open();
            if let Some(val) = value_node {
                let type_fact = infer_field_value_type(ctx, field_node, val, depth);
                shape.array_element_type = Some(match shape.array_element_type.take() {
                    Some(existing) => merge_types(existing, type_fact),
                    None => type_fact,
                });
            }
        }
        // No key at all — array-style entry (`{ 1, 2, 3 }`).
        None => {
            if let Some(val) = value_node {
                let type_fact = infer_field_value_type(ctx, field_node, val, depth);
                shape.array_element_type = Some(match shape.array_element_type.take() {
                    Some(existing) => merge_types(existing, type_fact),
                    None => type_fact,
                });
            }
        }
    }
}

fn is_lua_identifier_key(text: &str) -> bool {
    let mut bytes = text.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first == b'_' || first.is_ascii_alphabetic()) {
        return false;
    }
    bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric())
}

fn infer_field_value_type(
    ctx: &mut BuildContext,
    field_node: tree_sitter::Node,
    value_node: tree_sitter::Node,
    depth: usize,
) -> TypeFact {
    extract_preceding_field_type_annotation(field_node, ctx.source)
        .unwrap_or_else(|| infer_expression_type(ctx, value_node, depth))
}

fn extract_preceding_field_type_annotation(
    field_node: tree_sitter::Node,
    source: &[u8],
) -> Option<TypeFact> {
    if let Some(type_fact) = extract_trailing_field_type_annotation(field_node, source) {
        return Some(type_fact);
    }

    let start = field_node.start_byte();
    let current_line_start = source[..start]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    if !source[current_line_start..start]
        .iter()
        .all(|b| b.is_ascii_whitespace())
    {
        return None;
    }

    let previous_line_end = current_line_start.checked_sub(1)?;
    let previous_line_start = source[..previous_line_end]
        .iter()
        .rposition(|&b| b == b'\n')
        .map(|idx| idx + 1)
        .unwrap_or(0);
    let previous_line = std::str::from_utf8(&source[previous_line_start..previous_line_end]).ok()?;
    if previous_line.trim().is_empty() {
        return None;
    }
    parse_field_type_annotation(previous_line)
}

fn extract_trailing_field_type_annotation(
    field_node: tree_sitter::Node,
    source: &[u8],
) -> Option<TypeFact> {
    let trailing = trailing_field_segment(field_node, source)?;
    let text = strip_leading_field_separator(trailing);
    text.starts_with("---@type")
        .then(|| parse_field_type_annotation(text))
        .flatten()
}

fn trailing_field_segment<'a>(field_node: tree_sitter::Node, source: &'a [u8]) -> Option<&'a str> {
    let field_row = field_node.end_position().row;
    let line_end = source[field_node.end_byte()..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|offset| field_node.end_byte() + offset)
        .unwrap_or(source.len());
    let mut segment_end = line_end;
    let mut next = field_node.next_sibling();
    while let Some(node) = next {
        if node.start_position().row != field_row {
            break;
        }
        if node.kind() == "field" {
            segment_end = segment_end.min(node.start_byte());
            break;
        }
        next = node.next_sibling();
    }
    std::str::from_utf8(&source[field_node.end_byte()..segment_end]).ok()
}

fn strip_leading_field_separator(text: &str) -> &str {
    let text = text.trim_start();
    let text = text
        .strip_prefix(',')
        .or_else(|| text.strip_prefix(';'))
        .unwrap_or(text);
    text.trim_start()
}

fn parse_field_type_annotation(comment_text: &str) -> Option<TypeFact> {
    parse_emmy_comments(comment_text)
        .into_iter()
        .rev()
        .find_map(|ann| match ann {
            EmmyAnnotation::Type { type_expr, .. } => Some(emmy_type_to_fact(&type_expr)),
            _ => None,
        })
}

/// Thin wrapper: unwrap `expression_list` then delegate to the shared
/// `util::extract_string_literal`.
pub(super) fn extract_string_from_node(
    ctx: &BuildContext,
    node: tree_sitter::Node,
) -> Option<String> {
    let inner = if node.kind() == "expression_list" {
        node.named_child(0)?
    } else {
        node
    };
    extract_string_literal(inner, ctx.source)
}
