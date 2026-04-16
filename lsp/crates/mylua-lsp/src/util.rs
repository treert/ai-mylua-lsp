use tower_lsp_server::ls_types::*;

pub fn ts_point_to_position(point: tree_sitter::Point) -> Position {
    Position {
        line: point.row as u32,
        character: point.column as u32,
    }
}

pub fn ts_node_to_range(node: tree_sitter::Node) -> Range {
    Range {
        start: ts_point_to_position(node.start_position()),
        end: ts_point_to_position(node.end_position()),
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

pub fn position_to_byte_offset(source: &str, pos: Position) -> Option<usize> {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in source.char_indices() {
        if line == pos.line && col == pos.character {
            return Some(i);
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf8() as u32;
        }
    }
    if line == pos.line && col == pos.character {
        return Some(source.len());
    }
    None
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
