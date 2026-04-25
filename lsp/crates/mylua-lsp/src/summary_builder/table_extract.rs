use crate::table_shape::{FieldInfo, TableShape, MAX_TABLE_SHAPE_DEPTH};
use crate::type_system::*;
use crate::util::{node_text, extract_string_literal};

use super::BuildContext;
use super::type_infer::infer_expression_type;
use super::fingerprint::merge_types;

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

    // ── Fast path: bracket-key-only tables ──────────────────────────
    // Tables where ALL fields use bracket-key syntax (`[exp] = value`)
    // are typically data-mapping tables (e.g. asset path remapping).
    // For these tables we skip per-field extraction and only record
    // the key/value types — this avoids O(N) HashMap inserts and
    // string allocations for tables with thousands of entries.
    if crate::util::is_bracket_key_only_table(constructor) {
        extract_bracket_key_table_types(ctx, constructor, shape, depth);
        return;
    }

    for i in 0..constructor.named_child_count() {
        let Some(field_list) = constructor.named_child(i as u32) else { continue };
        if field_list.kind() != "field_list" {
            continue;
        }
        for j in 0..field_list.named_child_count() {
            let Some(field_node) = field_list.named_child(j as u32) else { continue };
            if field_node.kind() != "field" {
                continue;
            }
            extract_single_field(ctx, field_node, shape, depth);
        }
    }
}

/// Fast-path extraction for bracket-key-only tables: sample a few
/// fields to determine key and value types, then set them on the
/// shape without storing individual field entries.
fn extract_bracket_key_table_types(
    ctx: &mut BuildContext,
    constructor: tree_sitter::Node,
    shape: &mut TableShape,
    depth: usize,
) {
    shape.mark_open();

    // Sample up to SAMPLE_COUNT fields to infer key/value types.
    const SAMPLE_COUNT: usize = 4;
    let mut sampled = 0usize;
    let mut key_type: Option<TypeFact> = None;
    let mut value_type: Option<TypeFact> = None;

    'outer: for i in 0..constructor.named_child_count() {
        let Some(field_list) = constructor.named_child(i as u32) else { continue };
        if field_list.kind() != "field_list" {
            continue;
        }
        for j in 0..field_list.named_child_count() {
            if sampled >= SAMPLE_COUNT {
                break 'outer;
            }
            let Some(field_node) = field_list.named_child(j as u32) else { continue };
            if field_node.kind() != "field" {
                continue;
            }
            if let Some(k) = field_node.child_by_field_name("key") {
                let kt = infer_expression_type(ctx, k, depth);
                key_type = Some(match key_type.take() {
                    Some(existing) => merge_types(existing, kt),
                    None => kt,
                });
            }
            if let Some(v) = field_node.child_by_field_name("value") {
                let vt = infer_expression_type(ctx, v, depth);
                value_type = Some(match value_type.take() {
                    Some(existing) => merge_types(existing, vt),
                    None => vt,
                });
            }
            sampled += 1;
        }
    }

    shape.key_type = key_type;
    shape.array_element_type = value_type;
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
                let type_fact = infer_expression_type(ctx, val, depth);
                shape.set_field(key.clone(), FieldInfo {
                    name: key,
                    type_fact,
                    def_range: Some(ctx.line_index.ts_node_to_byte_range(field_node, ctx.source)),
                    assignment_count: 1,
                });
            }
        }
        // `[literal] = value` — static bracket key (string / number).
        // Normalize the raw lexeme:
        //   - strings: strip surrounding quotes so `["foo"] = 1` is
        //     indexed under `foo` (matching `t.foo` / `t["foo"]`
        //     lookups in the diagnostics/hover paths).
        //   - numbers: keep the source text as-is; callers that want
        //     to compare `t[1]` look up by the decimal spelling.
        // Non-string/number literals are rejected by the outer match.
        Some(k) if matches!(k.kind(), "string" | "number") => {
            let key_text = if k.kind() == "string" {
                // Fall back to raw lexeme when the scanner produced an
                // empty/exotic string that `extract_string_literal`
                // can't reach.
                extract_string_literal(k, ctx.source)
                    .unwrap_or_else(|| node_text(k, ctx.source).to_string())
            } else {
                node_text(k, ctx.source).to_string()
            };
            if let Some(val) = value_node {
                let type_fact = infer_expression_type(ctx, val, depth);
                shape.set_field(key_text.clone(), FieldInfo {
                    name: key_text,
                    type_fact,
                    def_range: Some(ctx.line_index.ts_node_to_byte_range(field_node, ctx.source)),
                    assignment_count: 1,
                });
            }
        }
        // `[expr] = value` with a non-literal key — dynamic bracket
        // write. Mark the shape as open and roll the value type into
        // `array_element_type` so `t[i]` subscript reads still pick up
        // a hint.
        Some(_) => {
            shape.mark_open();
            if let Some(val) = value_node {
                let type_fact = infer_expression_type(ctx, val, depth);
                shape.array_element_type = Some(
                    match shape.array_element_type.take() {
                        Some(existing) => merge_types(existing, type_fact),
                        None => type_fact,
                    }
                );
            }
        }
        // No key at all — array-style entry (`{ 1, 2, 3 }`).
        None => {
            if let Some(val) = value_node {
                let type_fact = infer_expression_type(ctx, val, depth);
                shape.array_element_type = Some(
                    match shape.array_element_type.take() {
                        Some(existing) => merge_types(existing, type_fact),
                        None => type_fact,
                    }
                );
            }
        }
    }
}

/// Thin wrapper: unwrap `expression_list` then delegate to the shared
/// `util::extract_string_literal`.
pub(super) fn extract_string_from_node(ctx: &BuildContext, node: tree_sitter::Node) -> Option<String> {
    let inner = if node.kind() == "expression_list" {
        node.named_child(0)?
    } else {
        node
    };
    extract_string_literal(inner, ctx.source)
}
