use tower_lsp_server::ls_types::*;
use crate::config::ReferencesStrategy;
use crate::document::{Document, DocumentLookup};
use crate::util::{node_text, find_node_at_position, ByteRange, LineIndex};
use crate::aggregation::WorkspaceAggregation;
use crate::resolver;
use crate::resolver::ResolvedLocation;
use crate::uri_id::{resolve_uri, UriId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceLocation {
    pub uri_id: UriId,
    pub range: Range,
}

// ---------------------------------------------------------------------------
// Semantic Identity — what entity the cursor points to
// ---------------------------------------------------------------------------

/// The semantic identity of an identifier at the cursor position.
/// Two identifiers are "references to the same thing" iff they resolve
/// to the same Identity.
enum Identity {
    /// Local variable: uniquely identified by its declaration byte offset
    /// within a file.
    Local {
        name: String,
        decl_byte: usize,
    },
    /// Global variable: identified by name + not shadowed by a local.
    Global {
        name: String,
    },
    /// Field/method on a type: identified by the declaration location
    /// (UriId + range). Naturally handles inheritance: `Bar:Foo`
    /// accessing an inherited field resolves to the same declaration in Foo.
    Field {
        field_name: String,
        location: ResolvedLocation,
    },
    /// An Emmy type name (e.g. `Foo` in `---@class Foo` or `---@type Foo`).
    TypeName {
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn find_references(
    doc: &Document,
    uri_id: UriId,
    position: Position,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
    strategy: &ReferencesStrategy,
) -> Option<Vec<Location>> {
    find_references_by_uri_id(doc, uri_id, position, include_declaration, index, all_docs, strategy)
        .map(|hits| {
            hits.into_iter()
                .map(|hit| Location {
                    uri: resolve_uri(hit.uri_id),
                    range: hit.range,
                })
                .collect()
        })
}

pub fn find_references_by_uri_id(
    doc: &Document,
    uri_id: UriId,
    position: Position,
    include_declaration: bool,
    index: &WorkspaceAggregation,
    all_docs: &impl DocumentLookup,
    strategy: &ReferencesStrategy,
) -> Option<Vec<ReferenceLocation>> {
    let byte_offset = doc.line_index().position_to_byte_offset(doc.source(), position)?;

    let identity = identify_at_cursor(doc, uri_id, byte_offset, index, all_docs)?;

    let mut locations = Vec::new();

    match &identity {
        Identity::Local { name, decl_byte } => {
            // Declaration
            if include_declaration {
                if let Some(decl) = doc.scope_tree.resolve_decl(*decl_byte, name) {
                    locations.push(ReferenceLocation {
                        uri_id,
                        range: decl.selection_range.into(),
                    });
                }
            }
            // Scan only this file
            let source = doc.source();
            for offset in find_word_occurrences(source, name) {
                let Some(node) = find_identifier_at(doc.tree.root_node(), offset, name.len()) else {
                    continue;
                };
                // Skip the declaration itself
                if node.start_byte() == *decl_byte {
                    continue;
                }
                if verify_local(node, name, *decl_byte, &doc.scope_tree) {
                    locations.push(ReferenceLocation {
                        uri_id,
                        range: doc.line_index().ts_node_to_range(node, source),
                    });
                }
            }
        }
        Identity::Global { name } => {
            // Declarations
            if include_declaration {
                collect_global_declarations(name, index, strategy, &mut locations);
            }
            // Scan all files
            all_docs.for_each_document_id(|doc_uri_id, file_doc| {
                let source = file_doc.source();
                for offset in find_word_occurrences(source, name) {
                    let Some(node) = find_identifier_at(file_doc.tree.root_node(), offset, name.len()) else {
                        continue;
                    };
                    if verify_global(node, name, &file_doc.scope_tree) {
                        locations.push(ReferenceLocation {
                            uri_id: doc_uri_id,
                            range: file_doc.line_index().ts_node_to_range(node, source),
                        });
                    }
                }
            });
            // Also scan Emmy annotations for references to this name
            // (e.g. if it's also a type name)
            if index.type_shard.contains_key(name.as_str()) {
                collect_emmy_type_references(name, all_docs, &mut locations);
            }
        }
        Identity::Field { field_name, location } => {
            let identity_def_range = location.range;
            // Declaration: the def_range itself
            if include_declaration {
                locations.push(ReferenceLocation {
                    uri_id: location.uri_id,
                    range: range_from_byte_range(location.uri_id, identity_def_range, all_docs),
                });
            }
            // Scan all files for the field name
            all_docs.for_each_document_id(|doc_uri_id, file_doc| {
                let source = file_doc.source();
                for offset in find_word_occurrences(source, field_name) {
                    let Some(node) = find_identifier_at(file_doc.tree.root_node(), offset, field_name.len()) else {
                        continue;
                    };
                    // Skip the declaration position itself
                    if doc_uri_id == location.uri_id
                        && node.start_byte() == identity_def_range.start_byte
                    {
                        continue;
                    }
                    if verify_field(node, *location, field_name, source, doc_uri_id, &file_doc.scope_tree, index) {
                        locations.push(ReferenceLocation {
                            uri_id: doc_uri_id,
                            range: file_doc.line_index().ts_node_to_range(node, source),
                        });
                    }
                }
            });
        }
        Identity::TypeName { name } => {
            // Declarations from type_shard
            if include_declaration {
                if let Some(candidates) = index.type_shard.get(name.as_str()) {
                    for candidate in candidates {
                        locations.push(ReferenceLocation {
                            uri_id: candidate.source_uri_id(),
                            range: candidate.range.into(),
                        });
                    }
                }
            }
            // Also include global_shard entries (the runtime value assignment)
            if include_declaration {
                if let Some(candidates) = index.global_shard.get(name.as_str()) {
                    match strategy {
                        ReferencesStrategy::Best => {
                            if let Some(best) = candidates.first() {
                                {
                                    locations.push(ReferenceLocation {
                                        uri_id: best.source_uri_id(),
                                        range: best.selection_range.into(),
                                    });
                                }
                            }
                        }
                        ReferencesStrategy::Merge | ReferencesStrategy::Select => {
                            for c in candidates {
                                locations.push(ReferenceLocation {
                                    uri_id: c.source_uri_id(),
                                    range: c.selection_range.into(),
                                });
                            }
                        }
                    }
                }
            }
            // Scan all files: both as global identifier references and as
            // Emmy annotation text references
            all_docs.for_each_document_id(|doc_uri_id, file_doc| {
                let source = file_doc.source();
                for offset in find_word_occurrences(source, name) {
                    let Some(node) = find_identifier_at(file_doc.tree.root_node(), offset, name.len()) else {
                        continue;
                    };
                    if verify_global(node, name, &file_doc.scope_tree) {
                        locations.push(ReferenceLocation {
                            uri_id: doc_uri_id,
                            range: file_doc.line_index().ts_node_to_range(node, source),
                        });
                    }
                }
            });
            // Emmy annotation text matches (type names in comments)
            collect_emmy_type_references(name, all_docs, &mut locations);
        }
    }

    // Deduplicate
    locations.sort_by(|a, b| {
        a.uri_id.cmp(&b.uri_id)
            .then(a.range.start.line.cmp(&b.range.start.line))
            .then(a.range.start.character.cmp(&b.range.start.character))
            .then(a.range.end.line.cmp(&b.range.end.line))
            .then(a.range.end.character.cmp(&b.range.end.character))
    });
    locations.dedup_by(|a, b| a.uri_id == b.uri_id && a.range == b.range);

    Some(locations)
}

// ---------------------------------------------------------------------------
// identify_at_cursor: determine what semantic entity the cursor is on
// ---------------------------------------------------------------------------

fn identify_at_cursor(
    doc: &Document,
    uri_id: UriId,
    byte_offset: usize,
    index: &WorkspaceAggregation,
    _all_docs: &impl DocumentLookup,
) -> Option<Identity> {
    // 1. Check if cursor is on a type name inside an Emmy annotation
    if let Some(type_name) = crate::emmy::emmy_type_name_at_byte(doc.source(), byte_offset) {
        return Some(Identity::TypeName { name: type_name });
    }

    // 2. Find the identifier AST node at cursor
    let ident_node = find_node_at_position(doc.tree.root_node(), byte_offset);
    let name_owned: String;
    let name: &str;

    if let Some(n) = ident_node {
        name = node_text(n, doc.source());
    } else {
        // Fallback: extract word from raw source (e.g. in emmy_line text)
        name_owned = extract_word_at(doc.text(), byte_offset)?;
        name = name_owned.as_str();
        // If it's a word in an emmy annotation and a known type, treat as TypeName
        if index.type_shard.contains_key(name) {
            return Some(Identity::TypeName { name: name.to_string() });
        }
        // Otherwise try as global
        if index.global_shard.contains_key(name) {
            return Some(Identity::Global { name: name.to_string() });
        }
        return None;
    }

    let ident_node = ident_node.unwrap();

    // 3. Check if in field/method position → Field identity
    if let Some(identity) = try_identify_field(ident_node, doc, uri_id, index) {
        return Some(identity);
    }

    // 4. Local variable?
    if let Some(decl) = doc.scope_tree.resolve_decl(byte_offset, name) {
        return Some(Identity::Local {
            name: name.to_string(),
            decl_byte: decl.decl_byte,
        });
    }

    // 5. Type name in type_shard?
    if index.type_shard.contains_key(name) {
        return Some(Identity::TypeName { name: name.to_string() });
    }

    // 6. Global
    Some(Identity::Global { name: name.to_string() })
}

/// Try to identify a field/method access. Returns `Some(Identity::Field)` if
/// the identifier is in a field/method position and the base type can be inferred.
fn try_identify_field(
    ident_node: tree_sitter::Node,
    doc: &Document,
    uri_id: UriId,
    index: &WorkspaceAggregation,
) -> Option<Identity> {
    let source = doc.source();
    let parent = ident_node.parent()?;

    let (base_node, field_name) = match parent.kind() {
        // `obj.field` — ident is the `field` child
        "variable" | "field_expression" => {
            let field_child = parent.child_by_field_name("field")?;
            if field_child.id() != ident_node.id() {
                return None;
            }
            let object = parent.child_by_field_name("object")?;
            (object, node_text(ident_node, source).to_string())
        }
        // `obj:method()` — ident is the `method` child
        "function_call" => {
            let method_child = parent.child_by_field_name("method")?;
            if method_child.id() != ident_node.id() {
                return None;
            }
            let callee = parent.child_by_field_name("callee")?;
            (callee, node_text(ident_node, source).to_string())
        }
        // `function M.foo()` / `function M:foo()` — ident is non-first in function_name
        "function_name" => {
            let first_child = parent.child(0)?;
            if first_child.id() == ident_node.id() {
                return None; // root name, not a field
            }
            // Build base segments from all identifiers before this one,
            // then infer the base type from the qualified path.
            let field_name = node_text(ident_node, source).to_string();
            // For function_name, we need to construct the "base" fact
            // from the owner segments. Collect all idents before ident_node.
            let mut segments = Vec::new();
            let mut first_segment_byte = 0usize;
            let child_count = parent.child_count();
            for i in 0..child_count {
                let Some(child) = parent.child(i as u32) else { continue };
                if child.id() == ident_node.id() {
                    break;
                }
                if child.kind() == "identifier" {
                    if segments.is_empty() {
                        first_segment_byte = child.start_byte();
                    }
                    segments.push(node_text(child, source).to_string());
                }
            }
            if segments.is_empty() {
                return None;
            }
            // Resolve the owner type through the segments chain
            let resolved = resolve_segments_to_field(
                &segments, &field_name, source, uri_id, &doc.scope_tree, index, first_segment_byte,
            )?;
            return Some(Identity::Field {
                field_name,
                location: resolved,
            });
        }
        _ => return None,
    };

    // Infer base type and resolve the field
    let base_fact = crate::type_inference::infer_node_type_in_file_id(
        base_node, source, uri_id, &doc.scope_tree, index,
    );
    let resolved = resolver::resolve_field_chain_in_file_id(
        uri_id, &base_fact, &[field_name.clone()], index,
    );

    let location = resolved.def_location?;

    Some(Identity::Field {
        field_name,
        location,
    })
}

/// Resolve a chain of owner segments + field_name to a definition location.
/// Used for `function M.N.foo()` where segments = ["M", "N"] and field = "foo".
///
/// `lookup_byte` is the byte offset at which the root segment name should be
/// resolved in the scope tree (typically the root identifier's start_byte).
/// This matters for local variables whose visibility starts after their
/// declaration statement.
fn resolve_segments_to_field(
    segments: &[String],
    field_name: &str,
    _source: &[u8],
    uri_id: UriId,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
    lookup_byte: usize,
) -> Option<ResolvedLocation> {
    // First, infer the type of the root segment.
    // Prefer bound_class (@class binding) over resolve_type, because @class
    // is the most explicit type declaration. When a local has both
    // `---@class Foo` and a type_fact from its initializer (e.g. a function
    // call whose return is unknown), bound_class is authoritative.
    let root_name = &segments[0];
    let root_fact = if let Some(class_name) = scope_tree.resolve_bound_class(lookup_byte, root_name) {
        crate::type_system::TypeFact::Known(crate::type_system::KnownType::EmmyType(class_name.to_string()))
    } else if let Some(tf) = scope_tree.resolve_type(lookup_byte, root_name) {
        tf.clone()
    } else {
        crate::type_system::TypeFact::Stub(crate::type_system::SymbolicStub::GlobalRef {
            name: root_name.clone(),
        })
    };

    // Resolve through intermediate segments if any
    let intermediate_fields: Vec<String> = segments[1..].to_vec();
    let base_fact = if intermediate_fields.is_empty() {
        root_fact
    } else {
        let resolved = resolver::resolve_field_chain_in_file_id(
            uri_id, &root_fact, &intermediate_fields, index,
        );
        resolved.type_fact
    };

    // Now resolve the final field
    let resolved = resolver::resolve_field_chain_in_file_id(
        uri_id, &base_fact, &[field_name.to_string()], index,
    );
    resolved.def_location
}

// ---------------------------------------------------------------------------
// Verification functions
// ---------------------------------------------------------------------------

/// Verify that a candidate identifier node refers to the same local variable
/// (same decl_byte).
fn verify_local(
    node: tree_sitter::Node,
    name: &str,
    target_decl_byte: usize,
    scope_tree: &crate::scope::ScopeTree,
) -> bool {
    scope_tree
        .resolve_decl(node.start_byte(), name)
        .is_some_and(|d| d.decl_byte == target_decl_byte)
}

/// Verify that a candidate identifier node is a global reference (not in
/// field position, not shadowed by a local).
fn verify_global(
    node: tree_sitter::Node,
    name: &str,
    scope_tree: &crate::scope::ScopeTree,
) -> bool {
    !is_non_reference_position(node)
        && scope_tree.resolve_decl(node.start_byte(), name).is_none()
}

/// Verify that a candidate identifier node is a field access that resolves
/// to the same declaration (UriId + range).
fn verify_field(
    node: tree_sitter::Node,
    target_location: ResolvedLocation,
    field_name: &str,
    source: &[u8],
    doc_uri_id: UriId,
    scope_tree: &crate::scope::ScopeTree,
    index: &WorkspaceAggregation,
) -> bool {
    let Some(parent) = node.parent() else { return false };

    let base_node = match parent.kind() {
        "variable" | "field_expression" => {
            let Some(field_child) = parent.child_by_field_name("field") else { return false };
            if field_child.id() != node.id() { return false; }
            match parent.child_by_field_name("object") {
                Some(obj) => obj,
                None => return false,
            }
        }
        "function_call" => {
            let Some(method_child) = parent.child_by_field_name("method") else { return false };
            if method_child.id() != node.id() { return false; }
            match parent.child_by_field_name("callee") {
                Some(c) => c,
                None => return false,
            }
        }
        "function_name" => {
            // `function M.foo()` — collect segments before this ident
            let first_child = match parent.child(0) {
                Some(c) => c,
                None => return false,
            };
            if first_child.id() == node.id() { return false; }
            let mut segments = Vec::new();
            let mut first_segment_byte = 0usize;
            let child_count = parent.child_count();
            for i in 0..child_count {
                let Some(child) = parent.child(i as u32) else { continue };
                if child.id() == node.id() { break; }
                if child.kind() == "identifier" {
                    if segments.is_empty() {
                        first_segment_byte = child.start_byte();
                    }
                    segments.push(node_text(child, source).to_string());
                }
            }
            if segments.is_empty() { return false; }
            // Resolve through segments
            if let Some(location) = resolve_segments_to_field(
                &segments, field_name, source, doc_uri_id, scope_tree, index, first_segment_byte,
            ) {
                return location == target_location;
            }
            return false;
        }
        _ => return false,
    };

    // Infer base type → resolve field → compare declaration location
    let base_fact = crate::type_inference::infer_node_type_in_file_id(
        base_node, source, doc_uri_id, scope_tree, index,
    );
    let resolved = resolver::resolve_field_chain_in_file_id(
        doc_uri_id, &base_fact, &[field_name.to_string()], index,
    );

    resolved.def_location == Some(target_location)
}

// ---------------------------------------------------------------------------
// String search helpers
// ---------------------------------------------------------------------------

/// Find all byte offsets where `name` appears as a whole word (respecting
/// identifier boundaries) in `source`. This is a fast O(n) scan that
/// avoids walking the AST tree.
fn find_word_occurrences(source: &[u8], name: &str) -> Vec<usize> {
    let pattern = name.as_bytes();
    if pattern.is_empty() || pattern.len() > source.len() {
        return Vec::new();
    }
    let mut results = Vec::new();
    let mut i = 0;
    while i + pattern.len() <= source.len() {
        if &source[i..i + pattern.len()] == pattern {
            let before_ok = i == 0 || !is_ident_byte(source[i - 1]);
            let after_ok = i + pattern.len() == source.len()
                || !is_ident_byte(source[i + pattern.len()]);
            if before_ok && after_ok {
                results.push(i);
                i += pattern.len();
                continue;
            }
        }
        i += 1;
    }
    results
}

/// Given a byte offset from string search, find the AST identifier node there.
/// Returns None if the node at that position is not an identifier matching
/// the expected length (filters out matches inside strings/comments).
fn find_identifier_at<'a>(
    root: tree_sitter::Node<'a>,
    byte_offset: usize,
    name_len: usize,
) -> Option<tree_sitter::Node<'a>> {
    let node = root.descendant_for_byte_range(byte_offset, byte_offset + name_len.saturating_sub(1))?;
    if node.kind() == "identifier"
        && node.start_byte() == byte_offset
        && node.end_byte() == byte_offset + name_len
    {
        Some(node)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Declaration collection helpers
// ---------------------------------------------------------------------------

fn collect_global_declarations(
    name: &str,
    index: &WorkspaceAggregation,
    strategy: &ReferencesStrategy,
    locations: &mut Vec<ReferenceLocation>,
) {
    match strategy {
        ReferencesStrategy::Best => {
            if let Some(candidates) = index.global_shard.get(name) {
                if let Some(best) = candidates.first() {
                    locations.push(ReferenceLocation {
                        uri_id: best.source_uri_id(),
                        range: best.selection_range.into(),
                    });
                }
            }
            if let Some(candidates) = index.type_shard.get(name) {
                if let Some(best) = candidates.first() {
                    locations.push(ReferenceLocation {
                        uri_id: best.source_uri_id(),
                        range: best.range.into(),
                    });
                }
            }
        }
        ReferencesStrategy::Merge | ReferencesStrategy::Select => {
            if let Some(candidates) = index.global_shard.get(name) {
                for candidate in candidates {
                    locations.push(ReferenceLocation {
                        uri_id: candidate.source_uri_id(),
                        range: candidate.selection_range.into(),
                    });
                }
            }
            if let Some(candidates) = index.type_shard.get(name) {
                for candidate in candidates {
                    locations.push(ReferenceLocation {
                        uri_id: candidate.source_uri_id(),
                        range: candidate.range.into(),
                    });
                }
            }
        }
    }
}

/// Convert a ByteRange to an LSP Range by looking up the document.
fn range_from_byte_range(
    uri_id: UriId,
    byte_range: ByteRange,
    all_docs: &impl DocumentLookup,
) -> Range {
    if let Some(doc) = all_docs.get_document_by_id(uri_id) {
        let start = doc.line_index().byte_offset_to_position(doc.source(), byte_range.start_byte);
        let end = doc.line_index().byte_offset_to_position(doc.source(), byte_range.end_byte);
        if let (Some(s), Some(e)) = (start, end) {
            return Range { start: s, end: e };
        }
    }
    // Fallback: use the Into<Range> impl from ByteRange
    byte_range.into()
}

// ---------------------------------------------------------------------------
// Non-reference position detection (reused from old implementation)
// ---------------------------------------------------------------------------

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
        "variable" | "field_expression" => {
            parent.child_by_field_name("field")
                .is_some_and(|f| f.id() == node.id())
        }
        "function_call" => {
            parent.child_by_field_name("method")
                .is_some_and(|m| m.id() == node.id())
        }
        "function_name" => {
            let first_child = parent.child(0);
            first_child.map(|fc| fc.id() != node.id()).unwrap_or(false)
        }
        "field" => {
            parent.child_by_field_name("key")
                .is_some_and(|k| k.id() == node.id())
        }
        "label_statement" | "goto_statement" => true,
        "attribute_name_list" | "name_list" => true,
        "local_function_declaration" => {
            parent.child_by_field_name("name")
                .is_some_and(|n| n.id() == node.id())
        }
        "attribute" => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Emmy type reference scanning (preserved from old implementation)
// ---------------------------------------------------------------------------

/// Walk every document's tree for occurrences of `type_name` inside comment
/// nodes. Annotation text is not materialized as identifier AST nodes, so we
/// match against raw line text with ASCII word boundaries.
fn collect_emmy_type_references(
    type_name: &str,
    all_docs: &impl DocumentLookup,
    locations: &mut Vec<ReferenceLocation>,
) {
    all_docs.for_each_document_id(|doc_uri_id, doc| {
        let source = doc.source();
        let mut cursor = doc.tree.root_node().walk();
        scan_type_in_comments(&mut cursor, type_name, source, doc_uri_id, locations, doc.line_index());
    });
}

fn scan_type_in_comments(
    cursor: &mut tree_sitter::TreeCursor,
    type_name: &str,
    source: &[u8],
    uri_id: UriId,
    locations: &mut Vec<ReferenceLocation>,
    line_index: &LineIndex,
) {
    let node = cursor.node();
    match node.kind() {
        "emmy_line" | "comment" => {
            emit_type_matches_in_node(node, type_name, source, uri_id, locations, line_index);
            return;
        }
        _ => {}
    }
    if cursor.goto_first_child() {
        loop {
            scan_type_in_comments(cursor, type_name, source, uri_id, locations, line_index);
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
    uri_id: UriId,
    locations: &mut Vec<ReferenceLocation>,
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
                    locations.push(ReferenceLocation {
                        uri_id,
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

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

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
    if bytes[start].is_ascii_digit() {
        return None;
    }
    std::str::from_utf8(&bytes[start..end]).ok().map(String::from)
}
