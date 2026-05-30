//! `textDocument/documentSymbol` — outline view support.
//!
//! The outline surfaces:
//!
//! - **Class** nodes for every `---@class` / `---@alias` / `---@enum`
//!   in `DocumentSummary.type_definitions`, with their `@field` entries
//!   nested as Field children and any `function Class:method()` /
//!   `function Class.method()` declarations nested as Method children.
//!   Function fields synthesized from method declarations are left to
//!   the method/function entry so the outline does not show duplicates.
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

use crate::syntax_kind::{field, kind, NodeKindExt, SyntaxKind};

use std::collections::{HashMap, HashSet};

use tower_lsp_server::ls_types::*;

pub use crate::config::DocumentSymbolDetailLevel;
use crate::summary::{DocumentSummary, TypeDefinitionKind};
use crate::type_system::{KnownType, TypeFact};
use crate::util::{node_text, LineIndex};

pub fn collect_document_symbols(
    root: tree_sitter::Node,
    source: &[u8],
    summary: Option<&DocumentSummary>,
    line_index: &LineIndex,
    detail_level: DocumentSymbolDetailLevel,
) -> Vec<DocumentSymbol> {
    let mut builder = OutlineBuilder::new(summary, line_index, detail_level);
    builder.visit_root(root, source);
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
    let start_ok =
        (outer.start.line, outer.start.character) <= (inner.start.line, inner.start.character);
    let end_ok = (outer.end.line, outer.end.character) >= (inner.end.line, inner.end.character);
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
    detail_level: DocumentSymbolDetailLevel,
}

impl<'a> OutlineBuilder<'a> {
    fn new(
        summary: Option<&DocumentSummary>,
        line_index: &'a LineIndex,
        detail_level: DocumentSymbolDetailLevel,
    ) -> Self {
        let mut class_index = HashMap::new();
        let mut class_nodes: Vec<DocumentSymbol> = Vec::new();
        let mut class_child_keys: HashMap<String, HashSet<String>> = HashMap::new();

        if let Some(summary) = summary {
            for td in &summary.type_definitions {
                if td.name.is_empty() {
                    continue;
                }
                if class_index.contains_key(td.name.as_str()) {
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
                    if is_synthesized_function_field(fd) {
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
                        name: fd.name.to_string(),
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
                    name: td.name.to_string(),
                    detail: None,
                    kind,
                    tags: None,
                    deprecated: None,
                    range: td.range.into(),
                    selection_range,
                    children: Some(children),
                });
                class_index.insert(td.name.to_string(), class_nodes.len() - 1);
                class_child_keys.insert(td.name.to_string(), seen_keys);
            }
        }

        Self {
            class_index,
            class_nodes,
            top: Vec::new(),
            class_child_keys,
            detail_level,
            line_index,
        }
    }

    fn visit_root(&mut self, root: tree_sitter::Node, source: &[u8]) {
        match self.detail_level {
            DocumentSymbolDetailLevel::Compact => self.visit_top_level(root, source),
            DocumentSymbolDetailLevel::Functions
            | DocumentSymbolDetailLevel::AllDeclarations
            | DocumentSymbolDetailLevel::AnonymousFunctions => {
                self.visit_top_level_detailed(root, source)
            }
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

    fn visit_top_level_detailed(&mut self, root: tree_sitter::Node, source: &[u8]) {
        let mut cursor = root.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let node = cursor.node();
            match node.syntax_kind() {
                kind::FUNCTION_DECLARATION => {
                    if let Some(sym) = self.function_declaration_symbol(node, source) {
                        self.top.push(sym);
                    }
                }
                kind::LOCAL_FUNCTION_DECLARATION => {
                    if let Some(sym) = self.local_function_symbol(node, source) {
                        self.top.push(sym);
                    }
                }
                kind::LOCAL_DECLARATION => self.visit_local_declaration(node, source),
                kind::ASSIGNMENT_STATEMENT => self.visit_assignment(node, source),
                _ if self.includes_anonymous_functions() => {
                    let mut nested = Vec::new();
                    self.collect_anonymous_function_symbols(node, source, &mut nested);
                    self.top.extend(nested);
                }
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn visit_statement(&mut self, node: tree_sitter::Node, source: &[u8]) {
        match node.syntax_kind() {
            kind::FUNCTION_DECLARATION => self.visit_function_declaration(node, source),
            kind::LOCAL_FUNCTION_DECLARATION => self.visit_local_function(node, source),
            kind::LOCAL_DECLARATION => self.visit_local_declaration(node, source),
            kind::ASSIGNMENT_STATEMENT => self.visit_assignment(node, source),
            _ => {}
        }
    }

    fn visit_function_declaration(&mut self, node: tree_sitter::Node, source: &[u8]) {
        if let Some(sym) = self.function_declaration_symbol(node, source) {
            self.top.push(sym);
        }
    }

    fn function_declaration_symbol(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Option<DocumentSymbol> {
        let Some(name_node) = node.child_by_field(field::NAME) else {
            return None;
        };
        let full_name = node_text(name_node, source).to_string();
        if full_name.is_empty() {
            return None;
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
                let children = self.detail_children_for_function(node, source);
                self.push_class_child(cls, member, kind, node, name_node, source, children);
                return None;
            }
        }

        let children = self.detail_children_for_function(node, source);
        #[allow(deprecated)]
        Some(DocumentSymbol {
            name: full_name,
            detail: None,
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(node, source),
            selection_range: self.line_index.ts_node_to_range(name_node, source),
            children,
        })
    }

    fn visit_local_function(&mut self, node: tree_sitter::Node, source: &[u8]) {
        if let Some(sym) = self.local_function_symbol(node, source) {
            self.top.push(sym);
        }
    }

    fn local_function_symbol(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Option<DocumentSymbol> {
        let Some(name_node) = node.child_by_field(field::NAME) else {
            return None;
        };
        let name = node_text(name_node, source).to_string();
        if name.is_empty() {
            return None;
        }
        let children = self.detail_children_for_function(node, source);
        #[allow(deprecated)]
        Some(DocumentSymbol {
            name,
            detail: Some("local".to_string()),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(node, source),
            selection_range: self.line_index.ts_node_to_range(name_node, source),
            children,
        })
    }

    fn visit_local_declaration(&mut self, node: tree_sitter::Node, source: &[u8]) {
        let symbols = self.local_declaration_symbols_for_detail(node, source);
        self.top.extend(symbols);
    }

    fn local_declaration_symbols_for_detail(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Vec<DocumentSymbol> {
        if !self.includes_anonymous_functions() {
            return self.local_declaration_symbols(node, source);
        }

        let Some(names_node) = node.child_by_field(field::NAMES) else {
            return Vec::new();
        };
        let values_node = node.child_by_field(field::VALUES);
        let mut symbols = Vec::new();
        for i in 0..names_node.named_child_count() {
            let Some(id_node) = names_node.named_child(i as u32) else {
                continue;
            };
            if !id_node.is_kind(kind::IDENTIFIER) {
                continue;
            }
            let name = node_text(id_node, source).to_string();
            if name.is_empty() || self.class_index.contains_key(&name) {
                continue;
            }
            let value_node = values_node.and_then(|v| v.named_child(i as u32));
            if let Some(value) = value_node.filter(|n| n.is_kind(kind::FUNCTION_DEFINITION)) {
                symbols.push(self.anonymous_function_symbol(
                    name,
                    Some("anonymous".to_string()),
                    node,
                    id_node,
                    value,
                    source,
                ));
            } else {
                symbols.push(self.variable_symbol(
                    name,
                    Some("local".to_string()),
                    node,
                    id_node,
                    source,
                ));
                if let Some(value) = value_node {
                    self.collect_anonymous_function_symbols(value, source, &mut symbols);
                }
            }
        }
        symbols
    }

    fn local_declaration_symbols(
        &self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Vec<DocumentSymbol> {
        let Some(names_node) = node.child_by_field(field::NAMES) else {
            return Vec::new();
        };
        let mut symbols = Vec::new();
        for i in 0..names_node.named_child_count() {
            let Some(id_node) = names_node.named_child(i as u32) else {
                continue;
            };
            if !id_node.is_kind(kind::IDENTIFIER) {
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
            symbols.push(self.variable_symbol(
                name,
                Some("local".to_string()),
                node,
                id_node,
                source,
            ));
        }
        symbols
    }

    fn visit_assignment(&mut self, node: tree_sitter::Node, source: &[u8]) {
        let Some(left) = node.child_by_field(field::LEFT) else {
            return;
        };
        let Some(first_var) = left.named_child(0) else {
            return;
        };

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

        if let Some(sym) = self.assignment_anonymous_function_symbol(node, source) {
            self.top.push(sym);
            return;
        }

        self.top
            .push(self.variable_symbol(name, None, node, first_var, source));
        if self.includes_anonymous_functions() {
            if let Some(right) = node.child_by_field(field::RIGHT) {
                let mut nested = Vec::new();
                self.collect_anonymous_function_symbols(right, source, &mut nested);
                self.top.extend(nested);
            }
        }
    }

    fn assignment_anonymous_function_symbol(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Option<DocumentSymbol> {
        if !self.includes_anonymous_functions() {
            return None;
        }
        let left = node.child_by_field(field::LEFT)?;
        let first_var = left.named_child(0)?;
        if has_dotted_or_subscript_form(first_var) {
            return None;
        }
        let name = node_text(first_var, source).to_string();
        if name.is_empty() || self.class_index.contains_key(&name) {
            return None;
        }
        let value = node
            .child_by_field(field::RIGHT)
            .and_then(|right| right.named_child(0))
            .filter(|n| n.is_kind(kind::FUNCTION_DEFINITION))?;

        Some(self.anonymous_function_symbol(
            name,
            Some("anonymous".to_string()),
            node,
            first_var,
            value,
            source,
        ))
    }

    fn variable_symbol(
        &self,
        name: String,
        detail: Option<String>,
        range_node: tree_sitter::Node,
        selection_node: tree_sitter::Node,
        source: &[u8],
    ) -> DocumentSymbol {
        #[allow(deprecated)]
        DocumentSymbol {
            name,
            detail,
            kind: SymbolKind::VARIABLE,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(range_node, source),
            selection_range: self.line_index.ts_node_to_range(selection_node, source),
            children: None,
        }
    }

    fn anonymous_function_symbol(
        &mut self,
        name: String,
        detail: Option<String>,
        range_node: tree_sitter::Node,
        selection_node: tree_sitter::Node,
        function_node: tree_sitter::Node,
        source: &[u8],
    ) -> DocumentSymbol {
        let children = self.detail_children_for_function(function_node, source);
        #[allow(deprecated)]
        DocumentSymbol {
            name,
            detail,
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(range_node, source),
            selection_range: self.line_index.ts_node_to_range(selection_node, source),
            children,
        }
    }

    fn push_class_child(
        &mut self,
        class_name: &str,
        member: &str,
        kind: SymbolKind,
        range_node: tree_sitter::Node,
        selection_node: tree_sitter::Node,
        source: &[u8],
        detail_children: Option<Vec<DocumentSymbol>>,
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
        let class_children = class_node.children.get_or_insert_with(Vec::new);
        #[allow(deprecated)]
        class_children.push(DocumentSymbol {
            name: member.to_string(),
            detail: None,
            kind,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(range_node, source),
            selection_range: self.line_index.ts_node_to_range(selection_node, source),
            children: detail_children,
        });
    }

    fn detail_children_for_function(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Option<Vec<DocumentSymbol>> {
        match self.detail_level {
            DocumentSymbolDetailLevel::Compact => None,
            DocumentSymbolDetailLevel::Functions
            | DocumentSymbolDetailLevel::AllDeclarations
            | DocumentSymbolDetailLevel::AnonymousFunctions => {
                let mut children = Vec::new();
                if self.includes_all_declarations() {
                    children.extend(self.parameter_symbols_for_function(node, source));
                }
                self.collect_nested_symbols(node, source, &mut children);
                if children.is_empty() {
                    None
                } else {
                    Some(children)
                }
            }
        }
    }

    fn collect_nested_symbols(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
        out: &mut Vec<DocumentSymbol>,
    ) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            match child.syntax_kind() {
                kind::FUNCTION_DECLARATION => {
                    if let Some(sym) = self.function_declaration_symbol(child, source) {
                        out.push(sym);
                    }
                }
                kind::LOCAL_FUNCTION_DECLARATION => {
                    if let Some(sym) = self.local_function_symbol(child, source) {
                        out.push(sym);
                    }
                }
                kind::LOCAL_DECLARATION if self.includes_all_declarations() => {
                    out.extend(self.local_declaration_symbols_for_detail(child, source));
                }
                kind::FOR_NUMERIC_STATEMENT | kind::FOR_GENERIC_STATEMENT
                    if self.includes_all_declarations() =>
                {
                    out.extend(self.for_variable_symbols(child, source));
                    self.collect_nested_symbols(child, source, out);
                }
                kind::ASSIGNMENT_STATEMENT if self.includes_anonymous_functions() => {
                    if let Some(sym) = self.assignment_anonymous_function_symbol(child, source) {
                        out.push(sym);
                    } else {
                        self.collect_anonymous_function_symbols(child, source, out);
                    }
                }
                kind::FUNCTION_DEFINITION if self.includes_anonymous_functions() => {
                    out.push(self.anonymous_function_symbol(
                        "<anonymous>".to_string(),
                        Some("anonymous".to_string()),
                        child,
                        child,
                        child,
                        source,
                    ));
                }
                kind::FUNCTION_DEFINITION => {}
                _ => self.collect_nested_symbols(child, source, out),
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn collect_anonymous_function_symbols(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
        out: &mut Vec<DocumentSymbol>,
    ) {
        let mut cursor = node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let child = cursor.node();
            match child.syntax_kind() {
                kind::FUNCTION_DEFINITION => {
                    out.push(self.anonymous_function_symbol(
                        "<anonymous>".to_string(),
                        Some("anonymous".to_string()),
                        child,
                        child,
                        child,
                        source,
                    ));
                }
                kind::ASSIGNMENT_STATEMENT => {
                    if let Some(sym) = self.assignment_anonymous_function_symbol(child, source) {
                        out.push(sym);
                    } else {
                        self.collect_anonymous_function_symbols(child, source, out);
                    }
                }
                kind::LOCAL_DECLARATION => {
                    self.collect_local_anonymous_function_symbols(child, source, out);
                }
                _ => self.collect_anonymous_function_symbols(child, source, out),
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn collect_local_anonymous_function_symbols(
        &mut self,
        node: tree_sitter::Node,
        source: &[u8],
        out: &mut Vec<DocumentSymbol>,
    ) {
        let Some(names_node) = node.child_by_field(field::NAMES) else {
            return;
        };
        let values_node = node.child_by_field(field::VALUES);
        for i in 0..names_node.named_child_count() {
            let Some(id_node) = names_node.named_child(i as u32) else {
                continue;
            };
            if !id_node.is_kind(kind::IDENTIFIER) {
                continue;
            }
            let name = node_text(id_node, source).to_string();
            if name.is_empty() || self.class_index.contains_key(&name) {
                continue;
            }
            let Some(value) = values_node.and_then(|v| v.named_child(i as u32)) else {
                continue;
            };
            if value.is_kind(kind::FUNCTION_DEFINITION) {
                out.push(self.anonymous_function_symbol(
                    name,
                    Some("anonymous".to_string()),
                    node,
                    id_node,
                    value,
                    source,
                ));
            } else {
                self.collect_anonymous_function_symbols(value, source, out);
            }
        }
    }

    fn parameter_symbols_for_function(
        &self,
        node: tree_sitter::Node,
        source: &[u8],
    ) -> Vec<DocumentSymbol> {
        let Some(body) = find_named_child_kind(node, kind::FUNCTION_BODY) else {
            return Vec::new();
        };
        let Some(params) = body.child_by_field(field::PARAMETERS) else {
            return Vec::new();
        };
        let mut symbols = Vec::new();
        self.collect_parameter_symbols(params, source, &mut symbols);
        symbols
    }

    fn collect_parameter_symbols(
        &self,
        node: tree_sitter::Node,
        source: &[u8],
        out: &mut Vec<DocumentSymbol>,
    ) {
        match node.syntax_kind() {
            kind::IDENTIFIER | kind::DOT_DOT_DOT => {
                let name = node_text(node, source).to_string();
                if name.is_empty() {
                    return;
                }

                #[allow(deprecated)]
                out.push(DocumentSymbol {
                    name,
                    detail: Some("param".to_string()),
                    kind: SymbolKind::VARIABLE,
                    tags: None,
                    deprecated: None,
                    range: self.line_index.ts_node_to_range(node, source),
                    selection_range: self.line_index.ts_node_to_range(node, source),
                    children: None,
                });
            }
            _ => {
                for i in 0..node.named_child_count() {
                    if let Some(child) = node.named_child(i as u32) {
                        self.collect_parameter_symbols(child, source, out);
                    }
                }
            }
        }
    }

    fn for_variable_symbols(&self, node: tree_sitter::Node, source: &[u8]) -> Vec<DocumentSymbol> {
        let mut symbols = Vec::new();
        match node.syntax_kind() {
            kind::FOR_NUMERIC_STATEMENT => {
                if let Some(name_node) = node.child_by_field(field::NAME) {
                    self.push_for_variable_symbol(node, name_node, source, &mut symbols);
                }
            }
            kind::FOR_GENERIC_STATEMENT => {
                if let Some(names_node) = node.child_by_field(field::NAMES) {
                    for i in 0..names_node.named_child_count() {
                        if let Some(id_node) = names_node.named_child(i as u32) {
                            if id_node.is_kind(kind::IDENTIFIER) {
                                self.push_for_variable_symbol(node, id_node, source, &mut symbols);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        symbols
    }

    fn push_for_variable_symbol(
        &self,
        range_node: tree_sitter::Node,
        selection_node: tree_sitter::Node,
        source: &[u8],
        out: &mut Vec<DocumentSymbol>,
    ) {
        let name = node_text(selection_node, source).to_string();
        if name.is_empty() {
            return;
        }
        #[allow(deprecated)]
        out.push(DocumentSymbol {
            name,
            detail: Some("for".to_string()),
            kind: SymbolKind::VARIABLE,
            tags: None,
            deprecated: None,
            range: self.line_index.ts_node_to_range(range_node, source),
            selection_range: self.line_index.ts_node_to_range(selection_node, source),
            children: None,
        });
    }

    fn includes_all_declarations(&self) -> bool {
        matches!(
            self.detail_level,
            DocumentSymbolDetailLevel::AllDeclarations
                | DocumentSymbolDetailLevel::AnonymousFunctions
        )
    }

    fn includes_anonymous_functions(&self) -> bool {
        self.detail_level == DocumentSymbolDetailLevel::AnonymousFunctions
    }

    fn finalize(mut self) -> Vec<DocumentSymbol> {
        // Sort children within each class by starting line so the
        // outline is stable regardless of @field / function ordering.
        for node in self.class_nodes.iter_mut() {
            if let Some(children) = node.children.as_mut() {
                children.sort_by_key(|c| (c.range.start.line, c.range.start.character));
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

fn is_synthesized_function_field(fd: &crate::summary::TypeFieldDef) -> bool {
    fd.name_range.is_none() && matches!(fd.type_fact, TypeFact::Known(KnownType::FunctionRef(_)))
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
    node.child_by_field(field::OBJECT).is_some()
        || node.child_by_field(field::FIELD).is_some()
        || node.child_by_field(field::INDEX).is_some()
}

fn find_named_child_kind<'tree>(
    node: tree_sitter::Node<'tree>,
    kind: SyntaxKind,
) -> Option<tree_sitter::Node<'tree>> {
    for i in 0..node.named_child_count() {
        let child = node.named_child(i as u32)?;
        if child.is_kind(kind) {
            return Some(child);
        }
    }
    None
}
