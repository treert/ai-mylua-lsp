use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::config::ReferencesStrategy;
use crate::document::Document;
use crate::util::{node_text, ts_node_to_range, position_to_byte_offset, find_node_at_position, byte_offset_to_position};
use crate::aggregation::WorkspaceAggregation;

pub fn find_references(
    doc: &Document,
    uri: &Uri,
    position: Position,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
    strategy: &ReferencesStrategy,
) -> Option<Vec<Location>> {
    let byte_offset = position_to_byte_offset(&doc.text, position)?;

    // Prefer an identifier AST node at the cursor; fall back to extracting
    // the surrounding ASCII word from raw source. The fallback covers the
    // case of clicking on an Emmy type name inside `---@class Foo` / etc.,
    // which lives inside an `emmy_line` text node rather than an identifier.
    let name_owned: String;
    let name: &str = if let Some(n) = find_node_at_position(doc.tree.root_node(), byte_offset) {
        node_text(n, doc.text.as_bytes())
    } else {
        name_owned = extract_word_at(&doc.text, byte_offset)?;
        name_owned.as_str()
    };

    if let Some(def) = doc.scope_tree.resolve(byte_offset, name, uri) {
        return Some(find_local_references(
            doc,
            uri,
            name,
            &def,
            include_declaration,
        ));
    }

    let mut results = find_global_references(
        name,
        include_declaration,
        index,
        all_docs,
        strategy,
    );

    // If the clicked identifier is an EmmyLua type name, also scan every
    // workspace file's emmy comments for references like `@type Foo`,
    // `@param x Foo`, `@class Bar: Foo`, `@return Foo`, etc. The Lua-side
    // global scan above won't catch these because annotation text isn't
    // materialized as identifier AST nodes.
    if index.type_shard.contains_key(name) {
        collect_emmy_type_references(name, all_docs, &mut results);
        results.sort_by(|a, b| {
            a.uri.to_string().cmp(&b.uri.to_string())
                .then(a.range.start.line.cmp(&b.range.start.line))
                .then(a.range.start.character.cmp(&b.range.start.character))
        });
        results.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);
    }

    Some(results)
}

fn find_local_references(
    doc: &Document,
    uri: &Uri,
    name: &str,
    def: &crate::types::Definition,
    include_declaration: bool,
) -> Vec<Location> {
    let mut locations = Vec::new();
    let source = doc.text.as_bytes();

    if include_declaration {
        locations.push(Location {
            uri: uri.clone(),
            range: def.selection_range,
        });
    }

    let def_byte = position_to_byte_offset(&doc.text, def.selection_range.start)
        .unwrap_or(0);

    // For `local` decls, `visible_after_byte == stmt.end_byte`, meaning the
    // name is not yet visible inside its own declaration statement. So we
    // probe at the end of the decl's full range (statement end) to land in
    // a position where the ScopeDecl is in scope. For params / for-vars
    // where `visible_after_byte == decl_byte`, this also works.
    let probe_byte = position_to_byte_offset(&doc.text, def.range.end)
        .unwrap_or(def_byte.saturating_add(name.len()));
    let target_decl_byte = doc.scope_tree
        .resolve_decl(probe_byte, name)
        .map(|d| d.decl_byte)
        .unwrap_or(def_byte);

    let scope_range = doc.scope_tree.scope_byte_range_for_def(probe_byte, name);
    let scope_node = if let Some((start, end)) = scope_range {
        doc.tree.root_node().descendant_for_byte_range(start, end.saturating_sub(1))
    } else {
        Some(doc.tree.root_node())
    };

    if let Some(scope) = scope_node {
        collect_identifier_occurrences(
            scope,
            name,
            source,
            uri,
            &mut locations,
            def,
            target_decl_byte,
            &doc.scope_tree,
        );
    }

    locations
}

/// Shared context for identifier-occurrence collection, avoiding
/// long parameter lists in the recursive walker.
struct RefSearchCtx<'a> {
    name: &'a str,
    source: &'a [u8],
    uri: &'a Uri,
    locations: &'a mut Vec<Location>,
    def: &'a crate::types::Definition,
    target_decl_byte: usize,
    scope_tree: &'a crate::scope::ScopeTree,
}

fn collect_identifier_occurrences(
    scope: tree_sitter::Node,
    name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
    def: &crate::types::Definition,
    target_decl_byte: usize,
    scope_tree: &crate::scope::ScopeTree,
) {
    let mut ctx = RefSearchCtx {
        name, source, uri, locations, def, target_decl_byte, scope_tree,
    };
    let mut cursor = scope.walk();
    collect_idents_recursive(&mut cursor, &mut ctx);
}

fn collect_idents_recursive(
    cursor: &mut tree_sitter::TreeCursor,
    ctx: &mut RefSearchCtx,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && node_text(node, ctx.source) == ctx.name {
        let range = ts_node_to_range(node, ctx.source);
        if range != ctx.def.selection_range {
            let ident_byte = node.start_byte();
            let resolves_to_target = ctx.scope_tree
                .resolve_decl(ident_byte, ctx.name)
                .is_some_and(|d| d.decl_byte == ctx.target_decl_byte);
            if resolves_to_target {
                ctx.locations.push(Location {
                    uri: ctx.uri.clone(),
                    range,
                });
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_idents_recursive(cursor, ctx);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn find_global_references(
    name: &str,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
    strategy: &ReferencesStrategy,
) -> Vec<Location> {
    let mut locations = Vec::new();

    if include_declaration {
        match strategy {
            ReferencesStrategy::Best => {
                if let Some(candidates) = index.global_shard.get(name) {
                    if let Some(best) = candidates.first() {
                        locations.push(Location {
                            uri: best.source_uri.clone(),
                            range: best.selection_range,
                        });
                    }
                }
                if let Some(candidates) = index.type_shard.get(name) {
                    if let Some(best) = candidates.first() {
                        locations.push(Location {
                            uri: best.source_uri.clone(),
                            range: best.range,
                        });
                    }
                }
            }
            ReferencesStrategy::Merge | ReferencesStrategy::Select => {
                if let Some(candidates) = index.global_shard.get(name) {
                    for candidate in candidates {
                        locations.push(Location {
                            uri: candidate.source_uri.clone(),
                            range: candidate.selection_range,
                        });
                    }
                }
                if let Some(candidates) = index.type_shard.get(name) {
                    for candidate in candidates {
                        locations.push(Location {
                            uri: candidate.source_uri.clone(),
                            range: candidate.range,
                        });
                    }
                }
            }
        }
    }

    for (doc_uri, doc) in all_docs {
        let source = doc.text.as_bytes();
        let mut cursor = doc.tree.root_node().walk();
        collect_global_name_occurrences(&mut cursor, name, source, doc_uri, &mut locations);
    }

    locations.sort_by(|a, b| {
        a.uri.to_string().cmp(&b.uri.to_string())
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
    });
    locations.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);

    locations
}

/// Walk every indexed document's tree for occurrences of `type_name`
/// inside comment nodes (`---@type Foo`, `---@param x Foo`,
/// `---@class Bar: Foo`, `---@return Foo`, etc.). Annotation text is not
/// materialized as identifier AST nodes, so we match against raw line text
/// with ASCII word boundaries to avoid false positives inside larger
/// identifiers. `emmy_line` is the grammar's per-line child of
/// `emmy_comment`, which is why the scanner matches `emmy_line` (and plain
/// `comment`) rather than the outer `emmy_comment`.
fn collect_emmy_type_references(
    type_name: &str,
    all_docs: &HashMap<Uri, Document>,
    locations: &mut Vec<Location>,
) {
    for (doc_uri, doc) in all_docs {
        let source = doc.text.as_bytes();
        let mut cursor = doc.tree.root_node().walk();
        scan_type_in_comments(&mut cursor, type_name, source, doc_uri, locations);
    }
}

fn scan_type_in_comments(
    cursor: &mut tree_sitter::TreeCursor,
    type_name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
) {
    let node = cursor.node();
    match node.kind() {
        "emmy_line" | "comment" => {
            emit_type_matches_in_node(node, type_name, source, uri, locations);
            return;
        }
        _ => {}
    }
    if cursor.goto_first_child() {
        loop {
            scan_type_in_comments(cursor, type_name, source, uri, locations);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn emit_type_matches_in_node(
    node: tree_sitter::Node,
    type_name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
) {
    let node_start_byte = node.start_byte();
    let line_bytes = match source.get(node_start_byte..node.end_byte()) {
        Some(b) => b,
        None => return,
    };
    let pattern = type_name.as_bytes();
    if pattern.is_empty() || pattern.len() > line_bytes.len() {
        return;
    }
    let mut i = 0;
    while i + pattern.len() <= line_bytes.len() {
        if &line_bytes[i..i + pattern.len()] == pattern {
            // Word boundary check: neighbor must be neither an ASCII ident
            // byte (`[A-Za-z0-9_]`) nor a UTF-8 continuation byte
            // (`10xxxxxx`). The latter guards against `中Foo` matching
            // `Foo` — without it the trailing byte of `中` (0xAD) would be
            // treated as a valid boundary.
            let is_boundary_byte = |b: u8| !is_ident_byte(b) && (b & 0xC0 != 0x80);
            let before_ok = i == 0 || is_boundary_byte(line_bytes[i - 1]);
            let after_ok = i + pattern.len() == line_bytes.len()
                || is_boundary_byte(line_bytes[i + pattern.len()]);
            if before_ok && after_ok {
                let abs_start = node_start_byte + i;
                let abs_end = abs_start + pattern.len();
                if let (Some(start), Some(end)) = (
                    byte_offset_to_position(source, abs_start),
                    byte_offset_to_position(source, abs_end),
                ) {
                    locations.push(Location {
                        uri: uri.clone(),
                        range: Range { start, end },
                    });
                }
                i += pattern.len();
                continue;
            }
        }
        i += 1;
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Extract the ASCII identifier-like word ([A-Za-z_][A-Za-z0-9_]*) that
/// spans the given byte offset. Returns `None` if the offset does not sit
/// on a word character.
pub fn extract_word_at(text: &str, byte_offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if byte_offset > bytes.len() {
        return None;
    }
    let mut start = byte_offset;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = byte_offset;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    // Ensure the first char isn't a digit (not a valid identifier start).
    if bytes[start].is_ascii_digit() {
        return None;
    }
    std::str::from_utf8(&bytes[start..end]).ok().map(String::from)
}



fn collect_global_name_occurrences(
    cursor: &mut tree_sitter::TreeCursor,
    name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && node_text(node, source) == name {
        if let Some(parent) = node.parent() {
            if parent.kind() == "variable" {
                let is_bare_name = parent.named_child_count() == 0
                    || (parent.child_count() == 1);
                if is_bare_name {
                    locations.push(Location {
                        uri: uri.clone(),
                        range: ts_node_to_range(node, source),
                    });
                }
            }
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_global_name_occurrences(cursor, name, source, uri, locations);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
