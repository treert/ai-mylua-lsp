use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use serde::{Serialize, Deserialize};
use tower_lsp_server::ls_types::*;

pub fn hash_bytes(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Percent-decode a URI path. Accumulates decoded bytes and interprets the
/// final buffer as UTF-8, so multi-byte encodings (e.g. `%E4%B8%AD` → 中)
/// are decoded correctly. Falls back to lossy decoding if the result is
/// not valid UTF-8.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    String::from_utf8(out)
        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned())
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Pre-computed index of `\n` byte offsets for a source buffer.
///
/// Stored alongside the source text (in `Document`) so that every
/// position-conversion function can do O(1) or O(log N) lookups
/// instead of scanning from byte 0 each time.
///
/// `line_starts` always begins with `[0, ...]` and pushes one entry
/// per `\n` (the byte offset AFTER the newline). So:
///   - For `"a\nb\nc"` (no trailing `\n`) → `[0, 2, 4]` → rows 0, 1, 2.
///   - For `"a\nb\n"`  (trailing `\n`)    → `[0, 2, 4]` → rows 0, 1, 2,
///     where row 2 is the virtual empty line at EOF.
pub struct LineIndex {
    line_starts: Vec<usize>,
}

impl LineIndex {
    /// Build a line index for `source`. One O(N) scan.
    pub fn new(source: &[u8]) -> Self {
        let mut line_starts = Vec::with_capacity(source.len() / 40 + 16);
        line_starts.push(0);
        for (i, &b) in source.iter().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self { line_starts }
    }

    /// Byte offset of the start of `row` (0-indexed), or `None` if
    /// `row` is past the last line.
    pub fn byte_offset_of_line(&self, row: usize) -> Option<usize> {
        self.line_starts.get(row).copied()
    }

    /// Access the raw line-start table for binary search.
    pub fn line_starts(&self) -> &[usize] {
        &self.line_starts
    }

    /// Convert a byte offset to an LSP `Position` using O(log N)
    /// binary search on the line-start table.
    pub fn byte_offset_to_position(&self, source: &[u8], byte_offset: usize) -> Option<Position> {
        if byte_offset > source.len() {
            return None;
        }
        let row = match self.line_starts.binary_search(&byte_offset) {
            Ok(exact) => exact,
            Err(ins) => ins.saturating_sub(1),
        };
        let line_start = self.line_starts[row];
        let col_bytes = byte_offset - line_start;
        let line_slice = &source[line_start..line_start + col_bytes];
        let character = byte_col_to_utf16_col(line_slice, col_bytes);
        Some(Position { line: row as u32, character })
    }

    /// Convert an LSP `Position` to a byte offset. O(1) line lookup +
    /// O(line_length) UTF-16 → byte column conversion.
    pub fn position_to_byte_offset(&self, source: &[u8], pos: Position) -> Option<usize> {
        let line_start = self.byte_offset_of_line(pos.line as usize)?;
        let line_bytes = line_bytes_after(source, line_start);
        let col_byte = utf16_col_to_byte_col(line_bytes, pos.character);
        let abs = line_start + col_byte;
        if abs <= source.len() {
            Some(abs)
        } else {
            None
        }
    }

    /// Return the bytes of the `row`-th line (without trailing newline).
    pub fn line_bytes_for_row<'a>(&self, source: &'a [u8], row: usize) -> &'a [u8] {
        match self.byte_offset_of_line(row) {
            Some(start) => line_bytes_after(source, start),
            None => &[],
        }
    }

    /// Convert a tree-sitter `Point` to an LSP `Position`.
    pub fn ts_point_to_position(&self, point: tree_sitter::Point, source: &[u8]) -> Position {
        let row = point.row;
        let byte_col = point.column;
        let line_bytes = self.line_bytes_for_row(source, row);
        let utf16_col = byte_col_to_utf16_col(line_bytes, byte_col);
        Position {
            line: row as u32,
            character: utf16_col,
        }
    }

    /// Convert a tree-sitter `Node` span to an LSP `Range`.
    pub fn ts_node_to_range(&self, node: tree_sitter::Node, source: &[u8]) -> Range {
        Range {
            start: self.ts_point_to_position(node.start_position(), source),
            end: self.ts_point_to_position(node.end_position(), source),
        }
    }

    /// Convert a tree-sitter `Node` span to an internal `ByteRange`.
    ///
    /// This extracts byte offsets and row/byte-column directly from the
    /// tree-sitter node **without** any UTF-16 conversion, making it
    /// significantly cheaper than `ts_node_to_range` for hot paths like
    /// `build_file_analysis`.
    pub fn ts_node_to_byte_range(&self, node: tree_sitter::Node, _source: &[u8]) -> ByteRange {
        let sp = node.start_position();
        let ep = node.end_position();
        ByteRange {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: sp.row as u32,
            start_col: sp.column as u32,
            end_row: ep.row as u32,
            end_col: ep.column as u32,
        }
    }

    /// Convert an internal `ByteRange` to an LSP `Range` (UTF-16).
    ///
    /// This is the **outbound** conversion used at the LSP protocol
    /// boundary when sending responses/notifications to VS Code.
    pub fn byte_range_to_lsp_range(&self, br: ByteRange, source: &[u8]) -> Range {
        let start_line_bytes = self.line_bytes_for_row(source, br.start_row as usize);
        let end_line_bytes = self.line_bytes_for_row(source, br.end_row as usize);
        Range {
            start: Position {
                line: br.start_row,
                character: byte_col_to_utf16_col(start_line_bytes, br.start_col as usize),
            },
            end: Position {
                line: br.end_row,
                character: byte_col_to_utf16_col(end_line_bytes, br.end_col as usize),
            },
        }
    }

    /// Convert an LSP `Range` (UTF-16) to an internal `ByteRange`.
    ///
    /// This is the **inbound** conversion used when VS Code sends a
    /// range that needs to be compared with internal `ByteRange` values.
    pub fn lsp_range_to_byte_range(&self, range: Range, source: &[u8]) -> ByteRange {
        let start_line_start = self.byte_offset_of_line(range.start.line as usize).unwrap_or(0);
        let start_line_bytes = line_bytes_after(source, start_line_start);
        let start_col_byte = utf16_col_to_byte_col(start_line_bytes, range.start.character);
        let start_byte = start_line_start + start_col_byte;

        let end_line_start = self.byte_offset_of_line(range.end.line as usize).unwrap_or(0);
        let end_line_bytes = line_bytes_after(source, end_line_start);
        let end_col_byte = utf16_col_to_byte_col(end_line_bytes, range.end.character);
        let end_byte = end_line_start + end_col_byte;

        ByteRange {
            start_byte,
            end_byte,
            start_row: range.start.line,
            start_col: start_col_byte as u32,
            end_row: range.end.line,
            end_col: end_col_byte as u32,
        }
    }
}

/// Bundles source text with its pre-computed [`LineIndex`].
///
/// Every time the text changes a new `LuaSource` is built, so the
/// line-start table is always in sync with the content. This replaces
/// the old pattern of storing `text: String` and `line_index: LineIndex`
/// as separate fields and manually keeping them consistent.
pub struct LuaSource {
    text: String,
    line_index: LineIndex,
}

impl LuaSource {
    /// Build a new `LuaSource` from owned text. The `LineIndex` is
    /// computed once during construction.
    pub fn new(text: String) -> Self {
        let line_index = LineIndex::new(text.as_bytes());
        Self { text, line_index }
    }

    /// Raw source bytes (`&[u8]`).
    #[inline]
    pub fn source(&self) -> &[u8] {
        self.text.as_bytes()
    }

    /// Source text as `&str`.
    #[inline]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The pre-computed line index.
    #[inline]
    pub fn line_index(&self) -> &LineIndex {
        &self.line_index
    }

    /// Consume and return the inner `String` (e.g. for snapshot copies).
    pub fn into_text(self) -> String {
        self.text
    }
}

/// Internal byte-oriented range type used throughout the LSP server.
///
/// Stores tree-sitter's native byte offsets and row/byte-column pairs,
/// avoiding the cost of UTF-16 conversion during `build_file_analysis`. Conversion to/from LSP `Range` (UTF-16) happens
/// only at the protocol boundary via `LineIndex::byte_range_to_lsp_range`
/// and `LineIndex::lsp_range_to_byte_range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct ByteRange {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_row: u32,
    pub start_col: u32,
    pub end_row: u32,
    pub end_col: u32,
}

impl ByteRange {
    /// Returns `true` if the given byte offset falls within this range
    /// (inclusive start, exclusive end).
    #[inline]
    pub fn contains_byte(&self, offset: usize) -> bool {
        offset >= self.start_byte && offset < self.end_byte
    }

    /// Returns `true` if the given byte offset falls within this range
    /// (inclusive start, inclusive end).
    #[inline]
    pub fn contains_byte_inclusive(&self, offset: usize) -> bool {
        offset >= self.start_byte && offset <= self.end_byte
    }

    /// Start position as `(row, byte_col)` tuple.
    #[inline]
    pub fn start_position(&self) -> (u32, u32) {
        (self.start_row, self.start_col)
    }

    /// End position as `(row, byte_col)` tuple.
    #[inline]
    pub fn end_position(&self) -> (u32, u32) {
        (self.end_row, self.end_col)
    }
}

pub fn node_text<'a>(node: tree_sitter::Node<'a>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("<error>")
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.replace('\n', "\\n")
    } else {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}...", &s[..cut].replace('\n', "\\n"))
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

pub fn extract_field_chain<'a>(
    mut node: tree_sitter::Node<'a>,
    source: &[u8],
) -> Option<(tree_sitter::Node<'a>, Vec<String>)> {
    let mut fields = Vec::new();

    while matches!(node.kind(), "variable" | "field_expression") {
        let Some(field) = node.child_by_field_name("field") else {
            break;
        };
        let object = node.child_by_field_name("object")?;
        fields.push(node_text(field, source).to_string());
        node = object;
    }

    if fields.is_empty() {
        return None;
    }

    fields.reverse();
    Some((node, fields))
}

/// Hard ceiling for "walk ancestors looking for X" loops across the
/// codebase. Lua AST ancestor chains for the patterns we inspect
/// (variable / field_expression / function_name / function_call)
/// are at most a handful of levels deep; anything beyond `ANCESTOR_WALK_LIMIT`
/// signals a malformed tree or a pathological source we'd rather
/// bail out of than spin on.
pub const ANCESTOR_WALK_LIMIT: usize = 64;

/// Walk up from `node` calling `pred` on each ancestor; return the
/// first ancestor for which `pred` yields `Some(T)`. Guards against
/// runaway trees by capping depth at [`ANCESTOR_WALK_LIMIT`] and
/// logging a warning via `crate::logger::log` if the cap is hit.
///
/// Note: `node` itself is NOT inspected — the walk starts at
/// `node.parent()`. This matches the common "find an enclosing Xyz"
/// pattern in hover / goto / completion.
pub fn walk_ancestors<'a, T>(
    node: tree_sitter::Node<'a>,
    mut pred: impl FnMut(tree_sitter::Node<'a>) -> Option<T>,
) -> Option<T> {
    let mut current = node;
    for _ in 0..ANCESTOR_WALK_LIMIT {
        let parent = current.parent()?;
        if let Some(v) = pred(parent) {
            return Some(v);
        }
        current = parent;
    }
    crate::logger::log(&format!(
        "[walk_ancestors] hit depth limit ({}) starting at {} — malformed tree?",
        ANCESTOR_WALK_LIMIT,
        node.kind(),
    ));
    None
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
    // Build the line index once for the pre-edit text and reuse it for
    // all position lookups within this function.
    let idx = LineIndex::new(text.as_bytes());
    let raw_start = idx.position_to_byte_offset(text.as_bytes(), range.start);
    let raw_end = idx.position_to_byte_offset(text.as_bytes(), range.end);

    // Happy path: trust the client's row numbers (the byte offsets above
    // were computed from them anyway). Only fall back to a full-text
    // scan of `byte_offset_to_ts_point` when we clamp to EOF, so editing
    // near the end of a large file doesn't force an O(file_size) rescan
    // for every keystroke.
    let (start_byte, start_point, old_end_byte, old_end_point) =
        match (raw_start, raw_end) {
            (Some(s), Some(e)) if s <= e => {
                let s_line_start =
                    idx.byte_offset_of_line(range.start.line as usize).unwrap_or(0);
                let e_line_start =
                    idx.byte_offset_of_line(range.end.line as usize).unwrap_or(s_line_start);
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

/// Returns `true` when `ancestor` is, or contains, `descendant` in the
/// tree-sitter AST. Walks from `descendant` upward comparing node IDs.
pub fn is_ancestor_or_equal(ancestor: tree_sitter::Node, descendant: tree_sitter::Node) -> bool {
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

/// Extract the textual content of a string-literal AST node.
///
/// Supports two extraction strategies, tried in order:
/// 1. **Recursive child search** — walk named children looking for a
///    `short_string_content` node (the grammar's canonical inner-text
///    node for `"..."` / `'...'` strings). This is the most reliable
///    approach and matches what `goto.rs` / `summary_builder.rs` used.
/// 2. **Quote stripping** — if no `short_string_content` child exists
///    (e.g. the grammar version differs), fall back to stripping a
///    matching pair of `"` or `'` from the raw node text. This was the
///    original `hover.rs` strategy.
///
/// The node may be a `string` node directly, or any ancestor that
/// contains one (the recursive walk handles both). If `node` is an
/// `expression_list`, callers should unwrap it first.
pub fn extract_string_literal(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    // Strategy 1: recursive search for `short_string_content` child.
    fn find_string_content(n: tree_sitter::Node, source: &[u8]) -> Option<String> {
        if n.kind().starts_with("short_string_content") {
            return Some(node_text(n, source).to_string());
        }
        for i in 0..n.named_child_count() {
            if let Some(child) = n.named_child(i as u32) {
                if let Some(s) = find_string_content(child, source) {
                    return Some(s);
                }
            }
        }
        None
    }

    if let Some(s) = find_string_content(node, source) {
        return Some(s);
    }

    // Strategy 2: strip matching quotes from the raw text.
    if node.kind() == "string" {
        let text = node_text(node, source);
        if text.len() >= 2 {
            let bytes = text.as_bytes();
            let first = bytes[0];
            let last = bytes[text.len() - 1];
            if (first == b'"' || first == b'\'') && first == last {
                return Some(text[1..text.len() - 1].to_string());
            }
        }
    }

    None
}

/// Extract individual argument-expression nodes from a `function_call`'s
/// `arguments` node.
///
/// Handles the three grammar forms:
/// - Paren form `f(a, b)` — the `arguments` node contains an
///   `expression_list` child whose named children are the actual args.
/// - Table form `f{...}` — the `arguments` node itself is the single arg.
/// - String form `f "x"` — the `arguments` node itself is the single arg.
///
/// For the paren form, if the `expression_list` is absent (zero-arg call),
/// returns an empty vector.
pub fn extract_call_arg_nodes<'tree>(
    args: tree_sitter::Node<'tree>,
    source: &[u8],
) -> Vec<tree_sitter::Node<'tree>> {
    // Non-paren form: the `arguments` node IS the single argument.
    if source.get(args.start_byte()).copied() != Some(b'(') {
        return vec![args];
    }
    // Paren form: look for `expression_list` child.
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
    exprs
}

/// Check whether a `table_constructor` AST node uses ONLY bracket-key
/// fields (`[exp] = value`). Returns `false` if any field uses
/// `Name = value` or positional (`value`) syntax, or if the table is
/// empty. Used by multiple subsystems (summary_builder, scope,
/// diagnostics, semantic_tokens) to skip expensive per-field
/// processing on large data-mapping tables.
///
/// For efficiency, only the first few fields are inspected — if they
/// are all bracket-key, the rest are assumed to follow the same
/// pattern.
pub fn is_bracket_key_only_table(constructor: tree_sitter::Node) -> bool {
    let mut has_fields = false;
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
            has_fields = true;
            let key_node = field_node.child_by_field_name("key");
            match key_node {
                // `Name = value` — identifier key → NOT bracket-key-only
                Some(k) if k.kind() == "identifier" => return false,
                // `[exp] = value` — bracket key → OK, continue checking
                Some(_) => {}
                // Positional `value` — no key → NOT bracket-key-only
                None => return false,
            }
            // Early exit: once we've confirmed a few fields are bracket-key,
            // trust the rest follow the same pattern.
            if j >= 3 {
                return true;
            }
        }
    }
    has_fields
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only convenience wrapper: builds a temporary `LineIndex`
    /// so existing tests don't need to be rewritten.
    fn position_to_byte_offset(source: &str, pos: Position) -> Option<usize> {
        let idx = LineIndex::new(source.as_bytes());
        idx.position_to_byte_offset(source.as_bytes(), pos)
    }

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

    // Minimal parse-only helper for walk_ancestors tests; mirrors
    // `tests/test_helpers::new_parser` but avoids pulling the test
    // helpers crate into the lib's unit-test scope.
    fn parse_source(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_mylua::LANGUAGE.into())
            .expect("mylua grammar");
        parser.parse(src, None).expect("parse")
    }

    #[test]
    fn walk_ancestors_finds_enclosing_kind() {
        // `function foo() return 1 end` — starting from the `1` number
        // literal, find the enclosing `function_declaration`.
        let tree = parse_source("function foo() return 1 end\n");
        let root = tree.root_node();
        let number = root.descendant_for_byte_range(22, 22).expect("number node");
        assert_eq!(number.kind(), "number");
        let func = walk_ancestors(number, |p| {
            if p.kind() == "function_declaration" {
                Some(p)
            } else {
                None
            }
        });
        assert!(func.is_some(), "should find function_declaration ancestor");
    }

    #[test]
    fn walk_ancestors_returns_none_when_pred_never_matches() {
        let tree = parse_source("local x = 1\n");
        let root = tree.root_node();
        let number = root.descendant_for_byte_range(10, 10).expect("number");
        let hit: Option<()> = walk_ancestors(number, |p| {
            // A kind that doesn't exist in Lua AST.
            if p.kind() == "nonexistent_node_kind" {
                Some(())
            } else {
                None
            }
        });
        assert!(hit.is_none());
    }

    #[test]
    fn walk_ancestors_caps_at_limit_on_deep_tree() {
        // Build a source with enough nested parentheses to exceed the
        // walk limit. Each `(` adds a `parenthesized_expression`
        // wrapper, so `(...(((1)))...)` with N opens gives a chain
        // whose inner `1` sits >= N ancestors down.
        let depth = ANCESTOR_WALK_LIMIT + 20;
        let mut src = String::from("local x = ");
        for _ in 0..depth {
            src.push('(');
        }
        src.push('1');
        for _ in 0..depth {
            src.push(')');
        }
        src.push('\n');

        let tree = parse_source(&src);
        let root = tree.root_node();
        // Locate the `1` — sitting deep inside the parens.
        let number_byte = "local x = ".len() + depth;
        let number = root
            .descendant_for_byte_range(number_byte, number_byte)
            .expect("number node");
        assert_eq!(number.kind(), "number");

        let mut calls = 0usize;
        let hit: Option<()> = walk_ancestors(number, |_p| {
            calls += 1;
            None
        });
        assert!(hit.is_none(), "never matching pred should return None");
        assert_eq!(
            calls, ANCESTOR_WALK_LIMIT,
            "walk must cap iterations at ANCESTOR_WALK_LIMIT ({}), got {}",
            ANCESTOR_WALK_LIMIT, calls,
        );
    }

    #[test]
    fn line_index_cache_invalidates_on_different_source() {
        // Regression guard for the thread-local line-start cache. Exercises:
        //   - fresh build for source A (cache miss)
        //   - repeated queries on A (cache hits)
        //   - switch to a structurally different source B (cache must
        //     rebuild, not serve stale offsets from A)
        //   - switch back to A (either rebuild or already-stale-proof)
        // Uses the public wrappers instead of calling `byte_offset_of_line`
        // directly so the test also verifies the end-to-end LSP
        // position → byte offset path that every tree-sitter range
        // conversion depends on.
        let src_a = "aaa\nbbb\nccc"; // 3 lines (no trailing newline)
        let src_b = "x\ny\nz\nw\nv"; // 5 lines (no trailing newline)

        // Prime cache with src_a and verify each line start.
        for (row, expected) in [(0u32, 0usize), (1, 4), (2, 8)] {
            let pos = Position { line: row, character: 0 };
            assert_eq!(
                position_to_byte_offset(src_a, pos),
                Some(expected),
                "src_a row {}", row
            );
        }
        // Out-of-range row yields None (past the last line).
        assert_eq!(
            position_to_byte_offset(src_a, Position { line: 3, character: 0 }),
            None,
            "src_a row 3 should be None (out of range, no trailing newline)"
        );

        // Switch to src_b — cache must rebuild with B's line starts.
        // If it didn't, row 4 would either return None (A had only 3
        // lines) or the stale offset from A.
        for (row, expected) in [(0u32, 0usize), (1, 2), (2, 4), (3, 6), (4, 8)] {
            let pos = Position { line: row, character: 0 };
            assert_eq!(
                position_to_byte_offset(src_b, pos),
                Some(expected),
                "src_b row {}", row
            );
        }

        // Switch back to src_a — must still produce A's offsets.
        assert_eq!(
            position_to_byte_offset(src_a, Position { line: 1, character: 0 }),
            Some(4),
            "src_a row 1 after round-trip through src_b"
        );
    }

    // ── ByteRange tests ──────────────────────────────────────────

    #[test]
    fn ts_node_to_byte_range_ascii() {
        let src = "local x = 1\n";
        let tree = parse_source(src);
        let root = tree.root_node();
        let idx = LineIndex::new(src.as_bytes());
        // Find the `1` number literal.
        let number = root.descendant_for_byte_range(10, 10).expect("number");
        assert_eq!(number.kind(), "number");
        let br = idx.ts_node_to_byte_range(number, src.as_bytes());
        assert_eq!(br.start_byte, 10);
        assert_eq!(br.end_byte, 11);
        assert_eq!(br.start_row, 0);
        assert_eq!(br.start_col, 10);
        assert_eq!(br.end_row, 0);
        assert_eq!(br.end_col, 11);
    }

    #[test]
    fn byte_range_to_lsp_range_ascii_roundtrip() {
        let src = "local x = 1\n";
        let tree = parse_source(src);
        let root = tree.root_node();
        let idx = LineIndex::new(src.as_bytes());
        let number = root.descendant_for_byte_range(10, 10).expect("number");
        let br = idx.ts_node_to_byte_range(number, src.as_bytes());
        let lsp_range = idx.byte_range_to_lsp_range(br, src.as_bytes());
        let expected = idx.ts_node_to_range(number, src.as_bytes());
        assert_eq!(lsp_range, expected, "ASCII: byte_range_to_lsp_range must match ts_node_to_range");
    }

    #[test]
    fn byte_range_to_lsp_range_chinese() {
        // "local x = \"中abc\"\n" — the string literal starts after `= `.
        let src = "local x = \"中abc\"\n";
        let tree = parse_source(src);
        let root = tree.root_node();
        let idx = LineIndex::new(src.as_bytes());
        // Find the string node.
        let string_node = root.descendant_for_byte_range(11, 11).expect("string");
        let br = idx.ts_node_to_byte_range(string_node, src.as_bytes());
        let lsp_range = idx.byte_range_to_lsp_range(br, src.as_bytes());
        let expected = idx.ts_node_to_range(string_node, src.as_bytes());
        assert_eq!(lsp_range, expected, "Chinese: byte_range_to_lsp_range must match ts_node_to_range");
    }

    #[test]
    fn lsp_range_to_byte_range_roundtrip() {
        let src = "local x = \"中abc\"\n";
        let tree = parse_source(src);
        let root = tree.root_node();
        let idx = LineIndex::new(src.as_bytes());
        let string_node = root.descendant_for_byte_range(11, 11).expect("string");
        let br = idx.ts_node_to_byte_range(string_node, src.as_bytes());
        let lsp_range = idx.byte_range_to_lsp_range(br, src.as_bytes());
        let back = idx.lsp_range_to_byte_range(lsp_range, src.as_bytes());
        assert_eq!(back, br, "roundtrip: lsp_range_to_byte_range(byte_range_to_lsp_range(br)) must equal br");
    }

    #[test]
    fn byte_range_contains() {
        let br = ByteRange {
            start_byte: 10, end_byte: 20,
            start_row: 0, start_col: 10, end_row: 0, end_col: 20,
        };
        assert!(br.contains_byte(10));
        assert!(br.contains_byte(15));
        assert!(!br.contains_byte(20)); // exclusive end
        assert!(br.contains_byte_inclusive(20)); // inclusive end
        assert!(!br.contains_byte(9));
    }

    #[test]
    fn byte_range_default_is_zero() {
        let br = ByteRange::default();
        assert_eq!(br.start_byte, 0);
        assert_eq!(br.end_byte, 0);
        assert_eq!(br.start_row, 0);
        assert_eq!(br.start_col, 0);
        assert_eq!(br.end_row, 0);
        assert_eq!(br.end_col, 0);
    }

    #[test]
    fn truncate_respects_utf8_char_boundary() {
        // Regression for panic "byte index N is not a char boundary;
        // it is inside '动' (bytes ...)". The original implementation
        // did `&s[..max]` with a raw byte index, which explodes if the
        // cut falls inside a multibyte UTF-8 character. We now snap the
        // cut down to the nearest char boundary.
        //
        // Input crafted so `max == 40` lands inside the 3-byte '动'
        // (U+52A8): "nil\r\nend\r\n\r\n---获取七日签到BP活动..." where
        // '动' occupies bytes 38..41, so max=40 is mid-char.
        let s = "nil\r\nend\r\n\r\n---获取七日签到BP活动详细数据\r\nfunction NoviceRewardModel";
        assert!(s.len() > 40, "precondition: source longer than cut point");
        let t = truncate(s, 40);
        assert!(t.ends_with("..."), "must include ellipsis when truncated");
        // Ensure the result ends cleanly before '动' (U+52A8, bytes 38..41).
        // After `\n → \\n` expansion the prefix length is not directly
        // comparable to the original cut, so instead check the char
        // straddling the boundary was dropped (not partially sliced).
        let prefix = t.strip_suffix("...").unwrap();
        assert!(
            !prefix.contains('动'),
            "prefix should not contain the char straddling the cut: {:?}",
            prefix
        );

        // ASCII path stays unchanged.
        assert_eq!(truncate("hello", 40), "hello");
        assert_eq!(truncate("hello\nworld", 40), "hello\\nworld");

        // Exactly-on-boundary cut works: '中' is 3 bytes (0..3).
        // max=3 is a valid boundary, so the prefix is '中' and ellipsis appended.
        let z = truncate("中国人民", 3);
        assert_eq!(z, "中...");
    }
}
