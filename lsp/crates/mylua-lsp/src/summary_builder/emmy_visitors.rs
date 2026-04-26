use crate::emmy::{parse_emmy_comments, emmy_type_to_fact, EmmyAnnotation, EmmyTableFieldKey, EmmyType};
use crate::summary::*;
use crate::util::{node_text, LineIndex};

use super::BuildContext;

// ---------------------------------------------------------------------------
// Emmy comments (type definitions)
// ---------------------------------------------------------------------------

/// Flush any pending @class definition into type_definitions.
/// Called when a non-emmy_comment node is encountered.
pub(super) fn flush_pending_class(ctx: &mut BuildContext, node: tree_sitter::Node) {
    if let Some((cname, parents, fields, generic_params, name_range)) = ctx.pending_class.take() {
        ctx.type_definitions.push(TypeDefinition {
            name: cname,
            kind: TypeDefinitionKind::Class,
            parents,
            fields,
            alias_type: None,
            generic_params,
            range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
            name_range: Some(name_range),
            anchor_shape_id: None,
        });
    }
}

fn emit_pending_class_as_typedef(
    ctx: &mut BuildContext,
    node: tree_sitter::Node,
) {
    if let Some((cname, prev_parents, fields, gparams, name_range)) = ctx.pending_class.take() {
        ctx.type_definitions.push(TypeDefinition {
            name: cname,
            kind: TypeDefinitionKind::Class,
            parents: prev_parents,
            fields,
            alias_type: None,
            generic_params: gparams,
            range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
            name_range: Some(name_range),
            anchor_shape_id: None,
        });
    }
}

pub(super) fn visit_emmy_comment(ctx: &mut BuildContext, node: tree_sitter::Node) {
    // Walk emmy_line children individually so each annotation can be
    // paired with its originating source range. `parse_emmy_comments`
    // returns at most one annotation per line (it tokenizes after the
    // leading `---`/`--` prefix), so `Vec::first()` suffices per line.
    // A name_range is computed per line by locating the identifier
    // token within the raw line text.
    for i in 0..node.named_child_count() {
        let Some(line_node) = node.named_child(i as u32) else { continue };
        if line_node.kind() != "emmy_line" {
            continue;
        }
        let line_text = node_text(line_node, ctx.source).to_string();
        let anns = parse_emmy_comments(&line_text);
        let Some(ann) = anns.first() else { continue };

        let line_start_byte = line_node.start_byte();
        let line_end_byte = line_node.end_byte();

        match ann {
            EmmyAnnotation::Class { name, parents, .. } => {
                emit_pending_class_as_typedef(ctx, node);
                let initial_gparams = std::mem::take(&mut ctx.pending_generic_params);
                let name_range = find_name_range_in_line(
                    ctx.line_index,
                    ctx.source,
                    line_start_byte,
                    line_end_byte,
                    &line_text,
                    name,
                    "class",
                );
                ctx.pending_class = Some((
                    name.clone(),
                    parents.clone(),
                    Vec::new(),
                    initial_gparams,
                    name_range,
                ));
            }
            EmmyAnnotation::Generic { params } => {
                if let Some((_, _, _, ref mut gparams, _)) = ctx.pending_class {
                    for gp in params {
                        gparams.push(gp.name.clone());
                    }
                } else {
                    for gp in params {
                        ctx.pending_generic_params.push(gp.name.clone());
                    }
                }
            }
            EmmyAnnotation::Field { name: fname, type_expr, .. } => {
                if let Some((_, _, ref mut fields, _, _)) = ctx.pending_class {
                    let full_range = ctx.line_index.ts_node_to_byte_range(line_node, ctx.source);
                    let name_range = find_name_range_in_line(
                        ctx.line_index,
                        ctx.source,
                        line_start_byte,
                        line_end_byte,
                        &line_text,
                        fname,
                        "field",
                    );
                    fields.push(TypeFieldDef {
                        name: fname.clone(),
                        type_fact: emmy_type_to_fact(type_expr),
                        range: full_range,
                        name_range: Some(name_range),
                    });
                }
            }
            EmmyAnnotation::Type { type_expr, .. } => {
                if ctx.pending_class.is_none() {
                    ctx.pending_type_annotation = Some(type_expr.clone());
                }
            }
            EmmyAnnotation::Alias { name, type_expr } => {
                emit_pending_class_as_typedef(ctx, node);
                let name_range = find_name_range_in_line(
                    ctx.line_index,
                    ctx.source,
                    line_start_byte,
                    line_end_byte,
                    &line_text,
                    name,
                    "alias",
                );
                // When the alias targets an inline table literal
                // (`---@alias Foo { x: number, y: number }`), flatten
                // its named fields into `TypeDefinition.fields` so that
                // `Foo` behaves like a class for field resolution,
                // hover, diagnostics and completion. `emmy_type_to_fact`
                // on `EmmyType::Table` otherwise collapses to an opaque
                // `Table(MAX)` with no recorded shape, which breaks the
                // `p.x` lookup on `---@type Foo local p = ...`.
                // `IndexType` keys (`[string]: number`) are not
                // nameable via dot/colon access, so they're skipped.
                let aliased_fields = if let EmmyType::Table(tfs) = type_expr {
                    let line_range = ctx.line_index.ts_node_to_byte_range(line_node, ctx.source);
                    tfs.iter()
                        .filter_map(|tf| match &tf.key {
                            EmmyTableFieldKey::Name(n) => Some(TypeFieldDef {
                                name: n.clone(),
                                type_fact: emmy_type_to_fact(&tf.value),
                                range: line_range,
                                name_range: None,
                            }),
                            EmmyTableFieldKey::IndexType(_) => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                ctx.type_definitions.push(TypeDefinition {
                    name: name.clone(),
                    kind: TypeDefinitionKind::Alias,
                    parents: Vec::new(),
                    fields: aliased_fields,
                    alias_type: Some(emmy_type_to_fact(type_expr)),
                    generic_params: Vec::new(),
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    name_range: Some(name_range),
                    anchor_shape_id: None,
                });
            }
            EmmyAnnotation::Enum { name } => {
                emit_pending_class_as_typedef(ctx, node);
                let name_range = find_name_range_in_line(
                    ctx.line_index,
                    ctx.source,
                    line_start_byte,
                    line_end_byte,
                    &line_text,
                    name,
                    "enum",
                );
                ctx.type_definitions.push(TypeDefinition {
                    name: name.clone(),
                    kind: TypeDefinitionKind::Enum,
                    parents: Vec::new(),
                    fields: Vec::new(),
                    alias_type: None,
                    generic_params: Vec::new(),
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    name_range: Some(name_range),
                    anchor_shape_id: None,
                });
            }
            _ => {}
        }
    }
}

/// Locate the byte range of the `<name>` token following a specific
/// annotation tag (`class`/`alias`/`enum`/`field`) inside an
/// `emmy_line`'s source text. Returns a `Range` anchored at the
/// original source file via `line_start_byte`.
///
/// Falls back to the full emmy_line range when the name cannot be
/// located (defensive; parser already validated the name exists).
///
/// For `@field`, the optional visibility keyword (`public` /
/// `private` / `protected` / `package`) is skipped before locating
/// the field name token so `---@field private name integer` resolves
/// to `name`, not `private`.
fn find_name_range_in_line(
    line_index: &LineIndex,
    source: &[u8],
    line_start_byte: usize,
    line_end_byte: usize,
    line_text: &str,
    name: &str,
    tag: &str,
) -> crate::util::ByteRange {
    // Find the `@<tag>` occurrence; scan forward to the name token.
    let tag_marker = format!("@{}", tag);
    let Some(tag_pos) = line_text.find(&tag_marker) else {
        return byte_span_to_byte_range(line_index, source, line_start_byte, line_end_byte);
    };
    // Byte cursor past the tag keyword.
    let mut cursor = tag_pos + tag_marker.len();
    let bytes = line_text.as_bytes();

    // Skip whitespace.
    while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
        cursor += 1;
    }

    // For @field: optionally skip a visibility keyword if what follows
    // is not the target `name`. We probe one identifier ahead.
    if tag == "field" {
        if let Some((word, next_cursor)) = read_identifier(bytes, cursor) {
            if word != name
                && matches!(word.as_str(), "public" | "private" | "protected" | "package")
            {
                cursor = next_cursor;
                while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
                    cursor += 1;
                }
            }
        }
    }

    // Now expect the `name` identifier at `cursor`.
    if cursor + name.len() <= bytes.len()
        && &bytes[cursor..cursor + name.len()] == name.as_bytes()
    {
        // Confirm word boundary on both sides to avoid partial matches.
        let before_ok = cursor == 0
            || !bytes[cursor - 1].is_ascii_alphanumeric() && bytes[cursor - 1] != b'_';
        let after_idx = cursor + name.len();
        let after_ok = after_idx >= bytes.len()
            || !(bytes[after_idx].is_ascii_alphanumeric() || bytes[after_idx] == b'_');
        if before_ok && after_ok {
            let start = line_start_byte + cursor;
            let end = start + name.len();
            return byte_span_to_byte_range(line_index, source, start, end);
        }
    }
    byte_span_to_byte_range(line_index, source, line_start_byte, line_end_byte)
}

/// Read an ASCII identifier starting at `start` within `bytes`.
/// Returns `(word, next_cursor)` past the identifier, or `None` when
/// no identifier is present at the cursor.
fn read_identifier(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    let mut end = start;
    if end >= bytes.len() {
        return None;
    }
    if !(bytes[end].is_ascii_alphabetic() || bytes[end] == b'_') {
        return None;
    }
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    let word = std::str::from_utf8(&bytes[start..end]).ok()?.to_string();
    Some((word, end))
}

/// Build a `ByteRange` from an absolute byte span, using the
/// file-level `LineIndex` to compute row / byte-column.
fn byte_span_to_byte_range(
    line_index: &LineIndex,
    _source: &[u8],
    start_byte: usize,
    end_byte: usize,
) -> crate::util::ByteRange {
    let start_row = match line_index.line_starts().binary_search(&start_byte) {
        Ok(exact) => exact,
        Err(ins) => ins.saturating_sub(1),
    };
    let start_line_start = line_index.byte_offset_of_line(start_row).unwrap_or(0);
    let start_col = start_byte - start_line_start;

    let end_row = match line_index.line_starts().binary_search(&end_byte) {
        Ok(exact) => exact,
        Err(ins) => ins.saturating_sub(1),
    };
    let end_line_start = line_index.byte_offset_of_line(end_row).unwrap_or(0);
    let end_col = end_byte - end_line_start;

    crate::util::ByteRange {
        start_byte,
        end_byte,
        start_row: start_row as u32,
        start_col: start_col as u32,
        end_row: end_row as u32,
        end_col: end_col as u32,
    }
}
