use std::collections::HashMap;
use tower_lsp_server::ls_types::*;
use crate::config::ReferencesStrategy;
use crate::document::Document;
use crate::util::{node_text, find_node_at_position, LineIndex};
use crate::aggregation::WorkspaceAggregation;
use crate::type_system::{TypeFact, KnownType};

pub fn find_references(
    doc: &Document,
    uri: &Uri,
    position: Position,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
    strategy: &ReferencesStrategy,
) -> Option<Vec<Location>> {
    let byte_offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;

    // Prefer an identifier AST node at the cursor; fall back to extracting
    // the surrounding ASCII word from raw source. The fallback covers the
    // case of clicking on an Emmy type name inside `---@class Foo` / etc.,
    // which lives inside an `emmy_line` text node rather than an identifier.
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset);
    let name_owned: String;
    let name: &str = if let Some(n) = ident_node {
        node_text(n, doc.source())
    } else {
        name_owned = extract_word_at(doc.text(), byte_offset)?;
        name_owned.as_str()
    };

    // Field/method reference: if the identifier is in a field position
    // (e.g. `foo` in `M.foo` or `M:foo()`), find all accesses to the
    // same field on the same owner rather than searching for a global `foo`.
    if let Some(n) = ident_node {
        if let Some(mut ctx) = detect_field_context(n, doc.source()) {
            // Try to resolve the owner's Emmy type from scope_tree (for
            // locals like `local obj ---@type Foo`) or from the global
            // index (for globals like `M` with `---@class M`).
            ctx.owner_type = resolve_field_owner_type(
                &ctx.owner_segments, byte_offset, &doc.scope_tree, index,
            );
            return Some(find_field_references(
                &ctx,
                include_declaration,
                index,
                all_docs,
            ));
        }
    }

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
    let source = doc.source();

    if include_declaration {
        locations.push(Location {
            uri: uri.clone(),
            range: def.selection_range.into(),
        });
    }

    let def_byte = def.selection_range.start_byte;

    // For `local` decls, `visible_after_byte == stmt.end_byte`, meaning the
    // name is not yet visible inside its own declaration statement. So we
    // probe at the end of the decl's full range (statement end) to land in
    // a position where the ScopeDecl is in scope. For params / for-vars
    // where `visible_after_byte == decl_byte`, this also works.
    let probe_byte = def.range.end_byte;
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
            doc.line_index(),
        );
    }

    locations
}

/// Shared context for identifier-occurrence collection, avoiding
/// long parameter lists in the recursive walker.
struct RefSearchCtx<'a> {
    name: &'a str,
    source: &'a [u8],
    line_index: &'a LineIndex,
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
    line_index: &LineIndex,
) {
    let mut ctx = RefSearchCtx {
        name, source, line_index, uri, locations, def, target_decl_byte, scope_tree,
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
        let range = ctx.line_index.ts_node_to_range(node, ctx.source);
        // Compare using byte offsets to avoid ByteRange vs Range mismatch.
        let node_start_byte = node.start_byte();
        if node_start_byte != ctx.def.selection_range.start_byte {
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
                            range: best.selection_range.into(),
                        });
                    }
                }
                if let Some(candidates) = index.type_shard.get(name) {
                    if let Some(best) = candidates.first() {
                        locations.push(Location {
                            uri: best.source_uri.clone(),
                            range: best.range.into(),
                        });
                    }
                }
            }
            ReferencesStrategy::Merge | ReferencesStrategy::Select => {
                if let Some(candidates) = index.global_shard.get(name) {
                    for candidate in candidates {
                        locations.push(Location {
                            uri: candidate.source_uri.clone(),
                            range: candidate.selection_range.into(),
                        });
                    }
                }
                if let Some(candidates) = index.type_shard.get(name) {
                    for candidate in candidates {
                        locations.push(Location {
                            uri: candidate.source_uri.clone(),
                            range: candidate.range.into(),
                        });
                    }
                }
            }
        }
    }

    for (doc_uri, doc) in all_docs {
        let source = doc.source();
        let mut cursor = doc.tree.root_node().walk();
        collect_global_name_occurrences(&mut cursor, name, source, doc_uri, &mut locations, doc.line_index(), &doc.scope_tree);
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
        let source = doc.source();
        let mut cursor = doc.tree.root_node().walk();
        scan_type_in_comments(&mut cursor, type_name, source, doc_uri, locations, doc.line_index());
    }
}

fn scan_type_in_comments(
    cursor: &mut tree_sitter::TreeCursor,
    type_name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    match node.kind() {
        "emmy_line" | "comment" => {
            emit_type_matches_in_node(node, type_name, source, uri, locations, line_index);
            return;
        }
        _ => {}
    }
    if cursor.goto_first_child() {
        loop {
            scan_type_in_comments(cursor, type_name, source, uri, locations, line_index);
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
    line_index: &LineIndex,
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
                    line_index.byte_offset_to_position(source, abs_start),
                    line_index.byte_offset_to_position(source, abs_end),
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

// ---------------------------------------------------------------------------
// Field/method reference finding
// ---------------------------------------------------------------------------

/// Context for a field/method reference search.
struct FieldContext {
    /// Owner path segments, e.g. `["M"]` for `M.foo`, `["M", "N"]` for `M.N.foo`.
    owner_segments: Vec<String>,
    /// The field/method name being searched.
    field_name: String,
    /// If the owner resolves to a known Emmy type (via scope_tree type_fact
    /// or bound_class), store the type name here for type-based matching.
    /// When set, the search matches any access to this field on any variable
    /// typed as this class, not just those with the same owner name.
    owner_type: Option<String>,
}

/// Detect whether `node` (an identifier) is in a field/method position and
/// extract the owner path + field name. Returns `None` if the identifier is
/// not a field access (e.g. it's a standalone global or local variable).
fn detect_field_context(node: tree_sitter::Node, source: &[u8]) -> Option<FieldContext> {
    let parent = node.parent()?;
    match parent.kind() {
        // `M.foo` or `M.N.foo` — node is the `field` child
        "variable" | "field_expression" => {
            let field_node = parent.child_by_field_name("field")?;
            if field_node.id() != node.id() {
                return None;
            }
            let object = parent.child_by_field_name("object")?;
            let segments = extract_base_segments(object, source)?;
            Some(FieldContext {
                owner_segments: segments,
                field_name: node_text(node, source).to_string(),
                owner_type: None,
            })
        }
        // `M:foo()` — node is the `method` child
        "function_call" => {
            let method_node = parent.child_by_field_name("method")?;
            if method_node.id() != node.id() {
                return None;
            }
            let callee = parent.child_by_field_name("callee")?;
            let segments = extract_base_segments(callee, source)?;
            Some(FieldContext {
                owner_segments: segments,
                field_name: node_text(node, source).to_string(),
                owner_type: None,
            })
        }
        // `function M.foo()` / `function M:foo()` — node is a non-first child
        "function_name" => {
            let first_child = parent.child(0)?;
            if first_child.id() == node.id() {
                return None; // This is the root name, not a field
            }
            // Collect all identifiers before this node as owner segments
            let mut segments = Vec::new();
            let child_count = parent.child_count();
            for i in 0..child_count {
                let Some(child) = parent.child(i as u32) else { continue };
                if child.id() == node.id() {
                    break;
                }
                if child.kind() == "identifier" {
                    segments.push(node_text(child, source).to_string());
                }
            }
            if segments.is_empty() {
                return None;
            }
            Some(FieldContext {
                owner_segments: segments,
                field_name: node_text(node, source).to_string(),
                owner_type: None,
            })
        }
        _ => None,
    }
}

/// Extract the identifier chain from an expression node, producing segments
/// like `["M"]` for a bare name or `["M", "N"]` for `M.N`. Returns `None`
/// if the expression contains non-identifier parts (calls, indexing, etc.).
fn extract_base_segments(node: tree_sitter::Node, source: &[u8]) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![node_text(node, source).to_string()]),
        "variable" | "field_expression" => {
            let object = node.child_by_field_name("object")?;
            let field = node.child_by_field_name("field")?;
            let mut segments = extract_base_segments(object, source)?;
            segments.push(node_text(field, source).to_string());
            Some(segments)
        }
        _ => None,
    }
}

/// Find all references to a field/method on a specific owner path.
fn find_field_references(
    ctx: &FieldContext,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &HashMap<Uri, Document>,
) -> Vec<Location> {
    let mut locations = Vec::new();
    let owner_path = ctx.owner_segments.join(".");

    // Determine the effective type name for declaration lookup and type-based search.
    let effective_type = ctx.owner_type.as_deref()
        .map(|s| s.to_string())
        .or_else(|| resolve_owner_type_name(&owner_path, index));

    // --- Declaration locations ---
    if include_declaration {
        // 1. Check GlobalShard for "Owner.field" (dot) and "Owner:field" (colon)
        let dot_path = format!("{}.{}", owner_path, ctx.field_name);
        let colon_path = format!("{}:{}", owner_path, ctx.field_name);
        for path in [&dot_path, &colon_path] {
            if let Some(candidates) = index.global_shard.get(path) {
                for c in candidates {
                    locations.push(Location {
                        uri: c.source_uri.clone(),
                        range: c.selection_range.into(),
                    });
                }
            }
        }

        // 2. Check Emmy @field declarations on the owner's type
        if let Some(ref type_name) = effective_type {
            collect_emmy_field_declarations(
                type_name, &ctx.field_name, index, &mut locations,
            );
        }
    }

    // --- Usage locations: walk all files ---
    for (doc_uri, doc) in all_docs {
        let source = doc.source();
        let mut cursor = doc.tree.root_node().walk();
        // Structural match: find `owner.field` / `owner:field()` by name
        collect_field_occurrences(
            &mut cursor,
            &ctx.owner_segments,
            &ctx.field_name,
            source,
            doc_uri,
            &mut locations,
            doc.line_index(),
        );
        // Type-based match: if we know the owner's type, also find accesses
        // on any local variable typed as the same class (e.g.
        // `local obj ---@type Foo; obj.field`).
        if let Some(ref type_name) = effective_type {
            collect_typed_field_occurrences(
                doc,
                doc_uri,
                type_name,
                &ctx.field_name,
                &mut locations,
            );
        }
    }

    locations.sort_by(|a, b| {
        a.uri.to_string().cmp(&b.uri.to_string())
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
    });
    locations.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);

    locations
}

/// Try to determine the Emmy type name for a global owner path.
/// E.g. for owner "M", check if `global_shard["M"]` has a type_fact that
/// references an Emmy type, or if `type_shard["M"]` exists directly.
fn resolve_owner_type_name(owner_path: &str, index: &WorkspaceAggregation) -> Option<String> {
    // Direct: type_shard has a class with exactly this name
    if index.type_shard.contains_key(owner_path) {
        return Some(owner_path.to_string());
    }
    // Indirect: global_shard entry has a type_fact pointing to an Emmy type
    if let Some(candidates) = index.global_shard.get(owner_path) {
        if let Some(first) = candidates.first() {
            if let Some(name) = extract_emmy_type_name(&first.type_fact) {
                return Some(name);
            }
        }
    }
    None
}

/// Resolve the Emmy type of a field owner at the trigger position.
///
/// For a single-segment owner (e.g. `obj` in `obj.foo`), checks:
/// 1. `scope_tree.resolve_bound_class` — covers `---@class Foo` anchored locals
/// 2. `scope_tree.resolve_type` → extract Emmy type name from TypeFact
/// 3. Falls through to `resolve_owner_type_name` for globals
///
/// For multi-segment owners (e.g. `M.N` in `M.N.foo`), only checks the
/// global index since scope_tree doesn't track dotted paths.
fn resolve_field_owner_type(
    owner_segments: &[String],
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
) -> Option<String> {
    let owner_path = owner_segments.join(".");

    if owner_segments.len() == 1 {
        let owner_name = &owner_segments[0];
        // 1. Check bound_class (e.g. `---@class Foo` above `local M = {}`)
        if let Some(class_name) = scope_tree.resolve_bound_class(byte_offset, owner_name) {
            return Some(class_name.to_string());
        }
        // 2. Check type_fact for Emmy type reference (e.g. `---@type Foo`)
        if let Some(fact) = scope_tree.resolve_type(byte_offset, owner_name) {
            if let Some(name) = extract_emmy_type_name(fact) {
                return Some(name);
            }
        }
    }

    // 3. Fall through to global index lookup
    resolve_owner_type_name(&owner_path, index)
}

/// Extract an Emmy type name from a TypeFact if it directly references one.
fn extract_emmy_type_name(fact: &TypeFact) -> Option<String> {
    match fact {
        TypeFact::Known(KnownType::EmmyType(name)) => Some(name.clone()),
        TypeFact::Known(KnownType::EmmyGeneric(name, _)) => Some(name.clone()),
        _ => None,
    }
}

/// Find Emmy `---@field` declarations on a type and add them to locations.
fn collect_emmy_field_declarations(
    type_name: &str,
    field_name: &str,
    index: &WorkspaceAggregation,
    locations: &mut Vec<Location>,
) {
    // Search all summaries for type definitions matching this type name
    for summary in index.summaries.values() {
        for td in &summary.type_definitions {
            if td.name == type_name {
                for f in &td.fields {
                    if f.name == field_name {
                        let range = f.name_range.unwrap_or(f.range);
                        locations.push(Location {
                            uri: summary.uri.clone(),
                            range: range.into(),
                        });
                    }
                }
            }
        }
    }
}

/// Find field/method accesses on local variables whose type matches `type_name`.
///
/// For each local variable in the document whose `bound_class` or `type_fact`
/// resolves to `type_name`, scan the scope for `.field_name` / `:field_name()`
/// accesses on that variable. This covers patterns like:
///
/// ```lua
/// ---@type Foo
/// local obj = getFoo()
/// obj.bar  -- found by this function
/// ```
fn collect_typed_field_occurrences(
    doc: &Document,
    uri: &Uri,
    type_name: &str,
    field_name: &str,
    locations: &mut Vec<Location>,
) {
    let source = doc.source();
    let line_index = doc.line_index();

    // Find all local variables in this file whose type matches
    for decl in doc.scope_tree.all_declarations() {
        let is_typed_match = decl.bound_class.as_deref() == Some(type_name)
            || decl.type_fact.as_ref().and_then(|f| extract_emmy_type_name(f)).as_deref() == Some(type_name);

        if !is_typed_match {
            continue;
        }

        // This local is typed as `type_name`. Scan the file for
        // `decl.name.field_name` / `decl.name:field_name()` using the
        // existing structural matcher with a single-segment owner.
        let owner_segs = vec![decl.name.clone()];
        let mut cursor = doc.tree.root_node().walk();
        collect_field_occurrences(
            &mut cursor,
            &owner_segs,
            field_name,
            source,
            uri,
            locations,
            line_index,
        );
    }
}

/// Walk the AST of a single file looking for field/method accesses where:
/// - The field/method name matches `field_name`
/// - The base object is a chain of identifiers matching `owner_segments`
fn collect_field_occurrences(
    cursor: &mut tree_sitter::TreeCursor,
    owner_segments: &[String],
    field_name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
    line_index: &LineIndex,
) {
    let node = cursor.node();

    match node.kind() {
        // `M.foo` / `M.N.foo` — check if field matches and object matches owner
        "variable" | "field_expression" => {
            if let Some(field_node) = node.child_by_field_name("field") {
                if node_text(field_node, source) == field_name {
                    if let Some(object) = node.child_by_field_name("object") {
                        if base_matches_segments(object, owner_segments, source) {
                            locations.push(Location {
                                uri: uri.clone(),
                                range: line_index.ts_node_to_range(field_node, source),
                            });
                        }
                    }
                }
            }
        }
        // `M:foo()` — check if method matches and callee matches owner
        "function_call" => {
            if let Some(method_node) = node.child_by_field_name("method") {
                if node_text(method_node, source) == field_name {
                    if let Some(callee) = node.child_by_field_name("callee") {
                        if base_matches_segments(callee, owner_segments, source) {
                            locations.push(Location {
                                uri: uri.clone(),
                                range: line_index.ts_node_to_range(method_node, source),
                            });
                        }
                    }
                }
            }
        }
        // `function M.foo()` / `function M:foo()` — flat identifier list
        "function_name" => {
            let child_count = node.child_count();
            // Collect all identifier children
            let mut idents: Vec<tree_sitter::Node> = Vec::new();
            for i in 0..child_count {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "identifier" {
                        idents.push(child);
                    }
                }
            }
            // Last identifier is the field/method; preceding are owner segments
            if idents.len() >= 2 {
                let last = idents[idents.len() - 1];
                if node_text(last, source) == field_name {
                    let preceding: Vec<&str> = idents[..idents.len() - 1]
                        .iter()
                        .map(|n| node_text(*n, source))
                        .collect();
                    if preceding.len() == owner_segments.len()
                        && preceding.iter().zip(owner_segments).all(|(a, b)| *a == b)
                    {
                        locations.push(Location {
                            uri: uri.clone(),
                            range: line_index.ts_node_to_range(last, source),
                        });
                    }
                }
            }
        }
        _ => {}
    }

    // Recurse into children
    if cursor.goto_first_child() {
        loop {
            collect_field_occurrences(
                cursor, owner_segments, field_name, source, uri, locations, line_index,
            );
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Check if an expression node is a chain of identifiers matching `segments`.
/// E.g. for segments `["M", "N"]`, matches the AST for `M.N`.
fn base_matches_segments(node: tree_sitter::Node, segments: &[String], source: &[u8]) -> bool {
    if segments.is_empty() {
        return false;
    }
    if segments.len() == 1 {
        // Base case: bare identifier, or a `variable` node wrapping one
        return (node.kind() == "identifier" && node_text(node, source) == segments[0])
            || (matches!(node.kind(), "variable" | "field_expression")
                && node.child_count() == 1
                && node.child(0).map(|c| c.kind() == "identifier" && node_text(c, source) == segments[0]).unwrap_or(false));
    }
    // Multi-segment: node should be a variable/field_expression with matching field + object
    if !matches!(node.kind(), "variable" | "field_expression") {
        return false;
    }
    let Some(field) = node.child_by_field_name("field") else { return false };
    let Some(object) = node.child_by_field_name("object") else { return false };
    let last = &segments[segments.len() - 1];
    if node_text(field, source) != last {
        return false;
    }
    base_matches_segments(object, &segments[..segments.len() - 1], source)
}



/// Check whether an identifier node is in a non-variable-reference position,
/// meaning it should NOT be treated as a reference to a global variable.
///
/// Returns `true` for:
/// - Field/method positions (`M.foo`'s `foo`, `M:bar()`'s `bar`)
/// - Table constructor keys (`{foo = 1}`'s `foo`)
/// - Label/goto names (`::foo::`, `goto foo`)
/// - Local declaration names (`local foo = ...`'s `foo`)
/// - For-loop variable names, parameter names in `attribute_name_list`/`name_list`
fn is_non_reference_position(node: tree_sitter::Node) -> bool {
    let Some(parent) = node.parent() else { return false };
    match parent.kind() {
        // `M.foo` — the `foo` is the field child of a `variable`/`field_expression`.
        "variable" | "field_expression" => {
            parent.child_by_field_name("field")
                .is_some_and(|f| f.id() == node.id())
        }
        // `M:bar()` — the `bar` is the method child of a `function_call`.
        "function_call" => {
            parent.child_by_field_name("method")
                .is_some_and(|m| m.id() == node.id())
        }
        // `function M.foo()` / `function M:bar()` — `function_name` is a flat
        // list of identifiers. The first child is the global root name; all
        // subsequent identifiers are field/method positions.
        "function_name" => {
            let first_child = parent.child(0);
            first_child.map(|fc| fc.id() != node.id()).unwrap_or(false)
        }
        // `{foo = 1}` — the `foo` is the key child of a table `field` node.
        // In Lua this is a string key, not a variable reference.
        "field" => {
            parent.child_by_field_name("key")
                .is_some_and(|k| k.id() == node.id())
        }
        // `::foo::` / `goto foo` — label names, not variable references.
        "label_statement" | "goto_statement" => true,
        // `local foo, bar = ...` / `for i, v in ...` — declaration LHS names.
        // These introduce new bindings rather than referencing globals.
        "attribute_name_list" | "name_list" => true,
        // `local function foo()` — the function name in a local function decl.
        "local_function_declaration" => {
            parent.child_by_field_name("name")
                .is_some_and(|n| n.id() == node.id())
        }
        // `<close>` attribute identifier inside attribute_name_list.
        "attribute" => true,
        _ => false,
    }
}

fn collect_global_name_occurrences(
    cursor: &mut tree_sitter::TreeCursor,
    name: &str,
    source: &[u8],
    uri: &Uri,
    locations: &mut Vec<Location>,
    line_index: &LineIndex,
    scope_tree: &crate::scope::ScopeTree,
) {
    let node = cursor.node();

    if node.kind() == "identifier" && node_text(node, source) == name {
        // Include this identifier as a global reference only when:
        // 1. It is NOT in a field/method position (e.g. `foo` in `M.foo`)
        // 2. It is NOT shadowed by a local variable at this position
        if !is_non_reference_position(node)
            && scope_tree.resolve_decl(node.start_byte(), name).is_none()
        {
            locations.push(Location {
                uri: uri.clone(),
                range: line_index.ts_node_to_range(node, source),
            });
        }
    }

    if cursor.goto_first_child() {
        loop {
            collect_global_name_occurrences(cursor, name, source, uri, locations, line_index, scope_tree);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}
