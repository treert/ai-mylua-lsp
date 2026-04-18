use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use tower_lsp_server::ls_types::*;

pub fn hash_bytes(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Convert a tree-sitter `Point` (row + BYTE-column into that row) into an
/// LSP `Position` where `character` is a UTF-16 code-unit offset — the
/// default LSP position encoding.
pub fn ts_point_to_position(point: tree_sitter::Point, source: &[u8]) -> Position {
    let row = point.row;
    let byte_col = point.column;
    let line_bytes = line_bytes_for_row(source, row);
    let utf16_col = byte_col_to_utf16_col(line_bytes, byte_col);
    Position {
        line: row as u32,
        character: utf16_col,
    }
}

/// Convert a tree-sitter `Node`'s span to an LSP `Range` with UTF-16-unit
/// columns. Callers always have `source` on hand; pass it through so the
/// conversion happens once at the tree-sitter boundary and every stored
/// `Range` is "LSP-ready".
pub fn ts_node_to_range(node: tree_sitter::Node, source: &[u8]) -> Range {
    Range {
        start: ts_point_to_position(node.start_position(), source),
        end: ts_point_to_position(node.end_position(), source),
    }
}

pub fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("<error>")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.replace('\n', "\\n")
    } else {
        format!("{}...", &s[..max].replace('\n', "\\n"))
    }
}

/// Convert an LSP `Position` (`character` = UTF-16 code units into its line)
/// into a byte offset into `source`.
pub fn position_to_byte_offset(source: &str, pos: Position) -> Option<usize> {
    let bytes = source.as_bytes();
    let line_start = byte_offset_of_line(bytes, pos.line as usize)?;
    let line_bytes = line_bytes_after(bytes, line_start);
    let col_byte = utf16_col_to_byte_col(line_bytes, pos.character);
    let abs = line_start + col_byte;
    if abs <= source.len() {
        Some(abs)
    } else {
        None
    }
}

/// Returns the byte offset of the start of `row` (0-indexed) in `source`,
/// or `None` if `row` is beyond the last newline.
fn byte_offset_of_line(source: &[u8], row: usize) -> Option<usize> {
    if row == 0 {
        return Some(0);
    }
    let mut line = 0usize;
    for (i, &b) in source.iter().enumerate() {
        if b == b'\n' {
            line += 1;
            if line == row {
                return Some(i + 1);
            }
        }
    }
    if line == row {
        Some(source.len())
    } else {
        None
    }
}

/// Return the slice of `source` from `start` up to (but not including) the
/// next `\n` or the end of input.
fn line_bytes_after(source: &[u8], start: usize) -> &[u8] {
    let mut end = start;
    while end < source.len() && source[end] != b'\n' {
        end += 1;
    }
    &source[start..end]
}

/// Return the bytes of the `row`-th line in `source` (without the trailing
/// newline). If `row` is past the last line, returns an empty slice.
fn line_bytes_for_row(source: &[u8], row: usize) -> &[u8] {
    match byte_offset_of_line(source, row) {
        Some(start) => line_bytes_after(source, start),
        None => &[],
    }
}

/// Convert a byte column within `line_bytes` (UTF-8) to a UTF-16 code-unit
/// column. `byte_col` past the end is clamped to the line's UTF-16 length.
pub fn byte_col_to_utf16_col(line_bytes: &[u8], byte_col: usize) -> u32 {
    let clamped = byte_col.min(line_bytes.len());
    // Safety: tree-sitter byte columns align with character boundaries for
    // well-formed UTF-8; invalid sequences fall back to replacement chars.
    let prefix = std::str::from_utf8(&line_bytes[..clamped])
        .unwrap_or_else(|e| std::str::from_utf8(&line_bytes[..e.valid_up_to()]).unwrap_or(""));
    let mut units = 0u32;
    for ch in prefix.chars() {
        units += ch.len_utf16() as u32;
    }
    units
}

/// Convert a UTF-16 code-unit column within `line_bytes` (UTF-8) back to a
/// byte column. Values past end-of-line are clamped to the line length.
pub fn utf16_col_to_byte_col(line_bytes: &[u8], utf16_col: u32) -> usize {
    let line_str = match std::str::from_utf8(line_bytes) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&line_bytes[..e.valid_up_to()]).unwrap_or(""),
    };
    let mut remaining = utf16_col;
    let mut bytes = 0usize;
    for ch in line_str.chars() {
        let unit = ch.len_utf16() as u32;
        if remaining < unit {
            break;
        }
        remaining -= unit;
        bytes += ch.len_utf8();
    }
    bytes
}

pub fn find_node_at_position<'a>(
    root: tree_sitter::Node<'a>,
    byte_offset: usize,
) -> Option<tree_sitter::Node<'a>> {
    let mut node = root.descendant_for_byte_range(byte_offset, byte_offset)?;
    while node.kind() != "identifier" {
        node = node.parent()?;
    }
    Some(node)
}

/// Apply an LSP `TextDocumentContentChangeEvent`-style incremental edit to
/// `text`, returning the `InputEdit` needed to tell tree-sitter about the
/// edit. `range` is in LSP coordinates (UTF-16).
///
/// If either endpoint of `range` cannot be resolved (e.g. the client sent a
/// position past end-of-file), we clamp **both** endpoints to `text.len()`
/// and emit the edit as an append at EOF. This is safer than silently
/// inserting at byte 0, which would corrupt the document. A warning is
/// logged so the mismatch is visible in the log file.
pub fn apply_text_edit(
    text: &mut String,
    range: Range,
    new_text: &str,
) -> tree_sitter::InputEdit {
    let raw_start = position_to_byte_offset(text, range.start);
    let raw_end = position_to_byte_offset(text, range.end);

    // Happy path: trust the client's row numbers (the byte offsets above
    // were computed from them anyway). Only fall back to a full-text
    // scan of `byte_offset_to_ts_point` when we clamp to EOF, so editing
    // near the end of a large file doesn't force an O(file_size) rescan
    // for every keystroke.
    let (start_byte, start_point, old_end_byte, old_end_point) =
        match (raw_start, raw_end) {
            (Some(s), Some(e)) if s <= e => {
                let bytes = text.as_bytes();
                let s_line_start =
                    byte_offset_of_line(bytes, range.start.line as usize).unwrap_or(0);
                let e_line_start =
                    byte_offset_of_line(bytes, range.end.line as usize).unwrap_or(s_line_start);
                let s_point = tree_sitter::Point {
                    row: range.start.line as usize,
                    column: s.saturating_sub(s_line_start),
                };
                let e_point = tree_sitter::Point {
                    row: range.end.line as usize,
                    column: e.saturating_sub(e_line_start),
                };
                (s, s_point, e, e_point)
            }
            _ => {
                crate::lsp_log!(
                    "[apply_text_edit] out-of-range Range {:?}/{:?} (text len={}); \
                     clamping to EOF",
                    range.start,
                    range.end,
                    text.len(),
                );
                let p = byte_offset_to_ts_point(text.as_bytes(), text.len());
                (text.len(), p, text.len(), p)
            }
        };

    text.replace_range(start_byte..old_end_byte, new_text);

    let new_end_byte = start_byte + new_text.len();
    let new_end_point = if let Some(last_nl) = new_text.rfind('\n') {
        let newlines = new_text.bytes().filter(|&b| b == b'\n').count();
        let tail_len = new_text.len() - (last_nl + 1);
        tree_sitter::Point {
            row: start_point.row + newlines,
            column: tail_len,
        }
    } else {
        tree_sitter::Point {
            row: start_point.row,
            column: start_point.column + new_text.len(),
        }
    };

    tree_sitter::InputEdit {
        start_byte,
        old_end_byte,
        new_end_byte,
        start_position: start_point,
        old_end_position: old_end_point,
        new_end_position: new_end_point,
    }
}

/// Convert a byte offset in `source` into a tree-sitter `Point` with
/// BYTE column. Used by `apply_text_edit` so the edit's start / old_end
/// positions are always consistent with the text we actually modify, even
/// when the caller's `Position` was out of range and got clamped.
fn byte_offset_to_ts_point(source: &[u8], byte_offset: usize) -> tree_sitter::Point {
    let clamped = byte_offset.min(source.len());
    let mut row = 0usize;
    let mut line_start = 0usize;
    for (i, &b) in source.iter().take(clamped).enumerate() {
        if b == b'\n' {
            row += 1;
            line_start = i + 1;
        }
    }
    tree_sitter::Point {
        row,
        column: clamped - line_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_utf16_ascii() {
        assert_eq!(byte_col_to_utf16_col(b"hello", 0), 0);
        assert_eq!(byte_col_to_utf16_col(b"hello", 3), 3);
        assert_eq!(byte_col_to_utf16_col(b"hello", 5), 5);
    }

    #[test]
    fn byte_to_utf16_bmp_chinese() {
        // 中 is U+4E2D (BMP, 1 UTF-16 unit, 3 UTF-8 bytes)
        let line = "中abc".as_bytes();
        assert_eq!(byte_col_to_utf16_col(line, 0), 0);
        assert_eq!(byte_col_to_utf16_col(line, 3), 1); // past 中
        assert_eq!(byte_col_to_utf16_col(line, 4), 2);
        assert_eq!(byte_col_to_utf16_col(line, 6), 4); // end
    }

    #[test]
    fn byte_to_utf16_astral_emoji() {
        // 👋 is U+1F44B (astral, 2 UTF-16 units, 4 UTF-8 bytes)
        let line = "👋x".as_bytes();
        assert_eq!(byte_col_to_utf16_col(line, 0), 0);
        assert_eq!(byte_col_to_utf16_col(line, 4), 2);
        assert_eq!(byte_col_to_utf16_col(line, 5), 3);
    }

    #[test]
    fn utf16_to_byte_roundtrip() {
        for line in &["abc", "中abc", "👋x", "ASCIIonly", "混合hello👋"] {
            let bytes = line.as_bytes();
            let mut byte_col = 0;
            for ch in line.chars() {
                let u16_col = byte_col_to_utf16_col(bytes, byte_col);
                let back = utf16_col_to_byte_col(bytes, u16_col);
                assert_eq!(back, byte_col, "roundtrip at {:?} byte {}", line, byte_col);
                byte_col += ch.len_utf8();
            }
        }
    }

    #[test]
    fn apply_text_edit_single_line_insert() {
        let mut text = String::from("hello world");
        // Insert "!" before "world" (utf-16 col 6).
        let edit = apply_text_edit(
            &mut text,
            Range {
                start: Position { line: 0, character: 6 },
                end: Position { line: 0, character: 6 },
            },
            "!",
        );
        assert_eq!(text, "hello !world");
        assert_eq!(edit.start_byte, 6);
        assert_eq!(edit.old_end_byte, 6);
        assert_eq!(edit.new_end_byte, 7);
        assert_eq!(edit.new_end_position.row, 0);
        assert_eq!(edit.new_end_position.column, 7);
    }

    #[test]
    fn apply_text_edit_out_of_range_appends_at_eof() {
        // A range starting past end-of-file must NOT silently insert at
        // byte 0 (which would corrupt the document). Instead, clamp both
        // endpoints to EOF and emit the edit as an append.
        let mut text = String::from("hello");
        let original = text.clone();
        let edit = apply_text_edit(
            &mut text,
            Range {
                start: Position { line: 99, character: 99 },
                end: Position { line: 99, character: 99 },
            },
            "!",
        );
        assert_eq!(text, format!("{}{}", original, "!"), "must append, not prepend");
        assert_eq!(edit.start_byte, original.len());
        assert_eq!(edit.old_end_byte, original.len());
        assert_eq!(edit.new_end_byte, original.len() + 1);
    }

    #[test]
    fn apply_text_edit_multi_line_replace() {
        let mut text = String::from("a\nb\nc");
        // Replace `b` (line 1 col 0..1) with "XX\nYY"
        let edit = apply_text_edit(
            &mut text,
            Range {
                start: Position { line: 1, character: 0 },
                end: Position { line: 1, character: 1 },
            },
            "XX\nYY",
        );
        assert_eq!(text, "a\nXX\nYY\nc");
        // new_end_position should be on the line containing "YY" at column 2.
        assert_eq!(edit.new_end_position.row, 2);
        assert_eq!(edit.new_end_position.column, 2);
    }

    #[test]
    fn position_to_byte_offset_with_chinese_line() {
        // LSP sends column as UTF-16 units. For line "中abc", column 3 (past
        // 中ab) should map to byte 5 (3 for 中 + 2 for ab).
        let src = "---\n中abc";
        let pos = Position { line: 1, character: 3 };
        let byte = position_to_byte_offset(src, pos).unwrap();
        assert_eq!(&src.as_bytes()[byte..byte + 1], b"c");
    }
}
