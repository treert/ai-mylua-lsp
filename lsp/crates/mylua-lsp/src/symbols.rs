//! `textDocument/documentSymbol` — outline view support.
//!
//! The outline surfaces:
//!
//! - **Class** nodes for every `---@class` / `---@alias` / `---@enum`
//!   in `DocumentSummary.type_definitions`, with their `@field` entries
//!   nested as Field children and any `function Class:method()` /
//!   `function Class.method()` declarations nested as Method children.
//! - **Function** nodes for top-level `function foo() end` and
//!   `local function foo() end` whose names do not belong to a known
//!   class.
//! - **Variable** nodes for `local x = ...` declarations.
//! - **Variable** nodes for plain global `name = ...` assignments.
//!
//! We intentionally drop every dotted or subscripted LHS assignment
//! (`t.foo = 1`, `a.b.c = nil`, `m[1] = ...`): those are field writes
//! rather than new symbol declarations, and treating them as symbols
//! produced a noisy outline on large files. When a `function Foo.m`
//! or `function Foo:m` declaration exists *but* `Foo` is not a class
//! known to the summary, we still emit it at top level under its full
//! dotted name so users aren't surprised by a missing symbol.

use std::collections::{HashMap, HashSet};

use tower_lsp_server::ls_types::*;

use crate::summary::{DocumentSummary, TypeDefinitionKind};
use crate::util::{node_text, LineIndex};

pub fn collect_document_symbols(
    root: tree_sitter::Node,
    source: &[u8],
    summary: Option<&DocumentSummary>,
    line_index: &LineIndex,
) -> Vec<DocumentSymbol> {
    let mut builder = OutlineBuilder::new(summary, line_index);
    builder.visit_top_level(root, source);
    let mut symbols = builder.finalize();
    normalize_ranges(&mut symbols);
    symbols
}

/// LSP requires `selection_range ⊆ range` for every `DocumentSymbol`.
/// This invariant is violated when a `TypeDefinition.name_range`
/// lives on a `---@class <Name>` / `---@alias <Name>` / `---@enum
/// <Name>` comment line while `TypeDefinition.range` points at a
/// *subsequent* anchor statement (e.g. `Foo = { ... }` on the next
/// line). The outline's `selection_range` is populated from
/// `name_range` (a per-identifier precision gain) and its `range`
/// from `td.range`, so the two can sit on disjoint lines.
///
/// VS Code's client throws `"selectionRange must be contained in
/// fullRange"` and drops the *entire* outline payload when that
/// happens. Walk the tree once and expand `range` to cover
/// `selection_range` whenever it would otherwise escape. Keeping
/// this at the outline boundary (rather than in `summary_builder`)
/// preserves `TypeDefinition.range`'s "anchor statement" semantics
/// for `goto_type_definition` and `workspace/symbol` consumers.
fn normalize_ranges(symbols: &mut [DocumentSymbol]) {
    for sym in symbols.iter_mut() {
        if !range_contains(&sym.range, &sym.selection_range) {
            sym.range = union_range(sym.range, sym.selection_range);
        }
        if let Some(children) = sym.children.as_mut() {
            normalize_ranges(children);
        }
    }
}

fn range_contains(outer: &Range, inner: &Range) -> bool {
    let start_ok = (outer.start.line, outer.start.character)
        <= (inner.start.line, inner.start.character);
    let end_ok = (outer.end.line, outer.end.character)
        >= (inner.end.line, inner.end.character);
    start_ok && end_ok
}

fn union_range(a: Range, b: Range) -> Range {
    let start = if (a.start.line, a.start.character) <= (b.start.line, b.start.character) {
        a.start
    } else {
        b.start
    };
    let end = if (a.end.line, a.end.character) >= (b.end.line, b.end.character) {
        a.end
    } else {
        b.end
    };
    Range { start, end }
}

struct OutlineBuilder<'a> {
    line_index: &'a LineIndex,
    /// Class name → index into `class_nodes` for O(1) child-append.
    class_index: HashMap<String, usize>,
    /// Fully-populated `DocumentSymbol` Class/Enum/Alias nodes.
    class_nodes: Vec<DocumentSymbol>,
    /// Non-class top-level symbols, emitted in source order.
    top: Vec<DocumentSymbol>,
    /// Track emitted (member, kind) pairs per class to dedup (e.g. a
    /// `@field m` plus a `function Class:m()` for the same method
    /// shouldn't both appear as children).
    class_child_keys: HashMap<String, HashSet<String>>,
}

impl<'a> OutlineBuilder<'a> {
    fn new(summary: Option<&DocumentSummary>, line_index: &'a LineIndex) -> Self {
        let mut class_index = HashMap::new();
        let mut class_nodes: Vec<DocumentSymbol> = Vec::new();
        let mut class_child_keys: HashMap<String, HashSet<String>> = HashMap::new();

        if let Some(summary) = summary {
            for td in &summary.type_definitions {
                if td.name.is_empty() {
                    continue;
                }
                if class_index.contains_key(&td.name) {
                    continue;
                }
                let kind = match td.kind {
                    TypeDefinitionKind::Class => SymbolKind::CLASS,
                    TypeDefinitionKind::Enum => SymbolKind::ENUM,
                    TypeDefinitionKind::Alias => SymbolKind::INTERFACE,
                };
                let mut children: Vec<DocumentSymbol> = Vec::new();
                let mut seen_keys: HashSet<String> = HashSet::new();
                for fd in &td.fields {
                    if fd.name.is_empty() {
                        continue;
                    }
                    let key = format!("{}:field", fd.name);
                    if !seen_keys.insert(key) {
                        continue;
                    }
                    // Prefer the precise `name_range` (byte range of the
                    // field name token) for `selection_range` so the
                    // client highlights just the field identifier when
                    // the user clicks this outline entry. Fall back to
                    // the full `---@field` line range if the summary
                    // was produced before the precise range was tracked.
                    let selection_range = fd.name_range.unwrap_or(fd.range).into();
                    #[allow(deprecated)]
                    children.push(DocumentSymbol {
                        name: fd.name.clone(),
                        detail: None,
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: fd.range.into(),
                        selection_range,
                        children: None,
                    });
                }
                // Same rationale as fields: use the `---@class <Name>`
                // identifier token for the outline's selection target.
                let selection_range = td.name_range.unwrap_or(td.range).into();
                #[allow(deprecated)]
                class_nodes.push(DocumentSymbol {
                    name: td.name.clone(),
                    detail: None,
                    kind,
                    tags: None,
                    deprecated: None,
                    range: td.range.into(),
                    selection_range,
                    children: Some(children),
                });
                class_index.insert(td.name.clone(), class_nodes.len() - 1);
                class_child_keys.insert(td.name.clone(), seen_keys);
            }
        }

        Self {
            class_index,
            class_nodes,
            top: Vec::new(),
            class_child_keys,
            line_index,
        }
    }

    fn visit_top_level(&mut self, root: tree_sitter::Node, source: &[u8]) {
        let mut cursor = root.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let node = cursor.node();
            self.visit_statement(node, source);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn visit_statement(&mut self, node: tree_sitter::Node, source: &[u8]) {
        match node.kind() {
            "function_declaration" => self.visit_function_declaration(node, source),
            "local_function_declaration" => self.visit_local_function(node, source),
            "local_declaration" => self.visit_local_declaration(node, source),
            "assignment_statement" => self.visit_assignment(node, source),
            _ => {}
        }
    }

    fn visit_function_declaration(&mut self, node: tree_sitter::Node, source: &[u8]) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let full_name = node_text(name_node, source).to_string();
        if full_name.is_empty() {
            return;
        }

        // Parse `Class:method` / `Class.method` / `a.b.c` out of the
        // `function_name` node. Format is intentionally simple: a
        // dot chain optionally followed by `:method`.
        let (class_name, member_name) = split_function_name(&full_name);

        if let (Some(cls), Some(member)) = (class_name.as_deref(), member_name.as_deref()) {
            // Nest under the class if it's known; otherwise fall back
            // to a top-level entry under the full dotted name so the
            // symbol isn't lost.
            if self.class_index.contains_key(cls) {
                let kind = if full_name.contains(':') {
                    SymbolKind::METHOD
                } else {
                    SymbolKind::FUNCTION
                };
                self.push_class_child(cls, member, kind, node, name_node, source);
                return;
            }
        }

        #[allow(deprecated)]
        self.top.push(DocumentSymbol {
            name: full_name,
            detail: None,
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(node, source),
            selection_range: self.line_index.ts_node_to_range(name_node, source),
            children: None,
        });
    }

    fn visit_local_function(&mut self, node: tree_sitter::Node, source: &[u8]) {
        let Some(name_node) = node.child_by_field_name("name") else { return };
        let name = node_text(name_node, source).to_string();
        if name.is_empty() {
            return;
        }
        #[allow(deprecated)]
        self.top.push(DocumentSymbol {
            name,
            detail: Some("local".to_string()),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(node, source),
            selection_range: self.line_index.ts_node_to_range(name_node, source),
            children: None,
        });
    }

    fn visit_local_declaration(&mut self, node: tree_sitter::Node, source: &[u8]) {
        let Some(names_node) = node.child_by_field_name("names") else { return };
        for i in 0..names_node.named_child_count() {
            let Some(id_node) = names_node.named_child(i as u32) else { continue };
            if id_node.kind() != "identifier" {
                continue;
            }
            let name = node_text(id_node, source).to_string();
            if name.is_empty() {
                continue;
            }
            // Skip locals whose name matches a known class; the
            // `local Foo = class()` / `local Foo = {}` pattern is the
            // typical anchor for the class — showing both would
            // double up.
            if self.class_index.contains_key(&name) {
                continue;
            }
            #[allow(deprecated)]
            self.top.push(DocumentSymbol {
                name,
                detail: Some("local".to_string()),
                kind: SymbolKind::VARIABLE,
                tags: None,
                deprecated: None,
                range: self.line_index.ts_node_to_range(node, source),
                selection_range: self.line_index.ts_node_to_range(id_node, source),
                children: None,
            });
        }
    }

    fn visit_assignment(&mut self, node: tree_sitter::Node, source: &[u8]) {
        let Some(left) = node.child_by_field_name("left") else { return };
        let Some(first_var) = left.named_child(0) else { return };

        // Skip dotted / subscripted LHS — those are field writes on
        // an existing variable, not new top-level symbols.
        if has_dotted_or_subscript_form(first_var) {
            return;
        }

        let name = node_text(first_var, source).to_string();
        if name.is_empty() {
            return;
        }
        // Skip assignments whose name matches a class (same "double
        // anchor" logic as for locals).
        if self.class_index.contains_key(&name) {
            return;
        }
        #[allow(deprecated)]
        self.top.push(DocumentSymbol {
            name,
            detail: None,
            kind: SymbolKind::VARIABLE,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(node, source),
            selection_range: self.line_index.ts_node_to_range(first_var, source),
            children: None,
        });
    }

    fn push_class_child(
        &mut self,
        class_name: &str,
        member: &str,
        kind: SymbolKind,
        range_node: tree_sitter::Node,
        selection_node: tree_sitter::Node,
        source: &[u8],
    ) {
        if member.is_empty() {
            return;
        }
        let idx = match self.class_index.get(class_name) {
            Some(&i) => i,
            None => return,
        };
        let key = format!("{}:{:?}", member, kind);
        let dup = self
            .class_child_keys
            .get_mut(class_name)
            .map(|s| !s.insert(key))
            .unwrap_or(false);
        if dup {
            return;
        }

        let class_node = &mut self.class_nodes[idx];
        let children = class_node.children.get_or_insert_with(Vec::new);
        #[allow(deprecated)]
        children.push(DocumentSymbol {
            name: member.to_string(),
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(range_node, source),
            selection_range: self.line_index.ts_node_to_range(selection_node, source),
            children: None,
        });
    }

    fn finalize(mut self) -> Vec<DocumentSymbol> {
        // Sort children within each class by starting line so the
        // outline is stable regardless of @field / function ordering.
        for node in self.class_nodes.iter_mut() {
            if let Some(children) = node.children.as_mut() {
                children.sort_by_key(|c| {
                    (c.range.start.line, c.range.start.character)
                });
            }
        }

        // Merge class nodes and top-level symbols, ordered by range
        // start. Classes whose anchor statement comes before a given
        // top-level symbol appear first. No dedup needed: class
        // anchors are already filtered inside the visit helpers via
        // `class_index.contains_key`, and child injection is guarded
        // by `class_child_keys`.
        let mut all: Vec<DocumentSymbol> = self.class_nodes;
        all.extend(self.top);
        all.sort_by_key(|s| (s.range.start.line, s.range.start.character));
        all
    }
}

/// Split a `function_name` like `Foo:method` / `Foo.method` /
/// `a.b.c` into `(outermost_prefix, last_segment)`. Returns
/// `(None, None)` for a bare identifier with no `.` / `:`.
fn split_function_name(full: &str) -> (Option<String>, Option<String>) {
    if let Some((prefix, method)) = full.rsplit_once(':') {
        return (Some(prefix.to_string()), Some(method.to_string()));
    }
    if let Some((prefix, last)) = full.rsplit_once('.') {
        return (Some(prefix.to_string()), Some(last.to_string()));
    }
    (None, None)
}

/// True if the `variable` node at `node` has the nested form
/// `object.field` or `object[index]`. Bare identifiers return false.
fn has_dotted_or_subscript_form(node: tree_sitter::Node) -> bool {
    node.child_by_field_name("object").is_some()
        || node.child_by_field_name("field").is_some()
        || node.child_by_field_name("index").is_some()
}
