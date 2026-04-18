use tower_lsp_server::ls_types::{Range, Uri};
use crate::types::{DefKind, Definition};
use crate::util::{node_text, ts_node_to_range};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    File,
    FunctionBody,
    DoBlock,
    WhileBlock,
    RepeatBlock,
    IfThenBlock,
    ElseIfBlock,
    ElseBlock,
    ForNumeric,
    ForGeneric,
}

#[derive(Debug, Clone)]
pub struct ScopeDecl {
    pub name: String,
    pub kind: DefKind,
    pub decl_byte: usize,
    /// Byte offset after which this declaration becomes visible.
    /// For parameters/for-variables this equals `decl_byte`.
    /// For `local` declarations this equals the statement's end byte,
    /// matching Lua semantics where `local x = x + 1` RHS sees the outer `x`.
    pub visible_after_byte: usize,
    pub range: Range,
    pub selection_range: Range,
}

#[derive(Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub byte_start: usize,
    pub byte_end: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub declarations: Vec<ScopeDecl>,
}

#[derive(Debug)]
pub struct ScopeTree {
    scopes: Vec<Scope>,
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

pub fn build_scope_tree(tree: &tree_sitter::Tree, source: &[u8]) -> ScopeTree {
    let mut builder = TreeBuilder {
        scopes: Vec::new(),
        source,
    };
    let root = tree.root_node();
    builder.push_scope(ScopeKind::File, root.start_byte(), root.end_byte(), None);
    builder.visit_children(root, 0);
    ScopeTree { scopes: builder.scopes }
}

struct TreeBuilder<'a> {
    scopes: Vec<Scope>,
    source: &'a [u8],
}

impl<'a> TreeBuilder<'a> {
    fn push_scope(&mut self, kind: ScopeKind, start: usize, end: usize, parent: Option<usize>) -> usize {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            kind,
            byte_start: start,
            byte_end: end,
            parent,
            children: Vec::new(),
            declarations: Vec::new(),
        });
        if let Some(pid) = parent {
            self.scopes[pid].children.push(id);
        }
        id
    }

    fn add_decl(&mut self, scope_id: usize, decl: ScopeDecl) {
        self.scopes[scope_id].declarations.push(decl);
    }

    fn visit_children(&mut self, parent_node: tree_sitter::Node<'a>, scope_id: usize) {
        let mut cursor = parent_node.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let node = cursor.node();
            self.visit_node(node, scope_id);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    fn visit_node(&mut self, node: tree_sitter::Node<'a>, scope_id: usize) {
        match node.kind() {
            "local_declaration" => {
                self.collect_local_decl(node, scope_id);
                self.visit_children(node, scope_id);
            }
            "local_function_declaration" => {
                self.collect_local_func_decl(node, scope_id);
                self.visit_children(node, scope_id);
            }
            "function_body" => {
                let child_scope = self.push_scope(
                    ScopeKind::FunctionBody,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.collect_parameters(node, child_scope);
                self.collect_implicit_self(node, child_scope);
                self.visit_children(node, child_scope);
            }
            "do_statement" => {
                let child_scope = self.push_scope(
                    ScopeKind::DoBlock,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.visit_children(node, child_scope);
            }
            "while_statement" => {
                let child_scope = self.push_scope(
                    ScopeKind::WhileBlock,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.visit_children(node, child_scope);
            }
            "repeat_statement" => {
                let child_scope = self.push_scope(
                    ScopeKind::RepeatBlock,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.visit_children(node, child_scope);
            }
            "if_statement" => {
                let child_scope = self.push_scope(
                    ScopeKind::IfThenBlock,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.visit_children(node, child_scope);
            }
            "elseif_clause" => {
                let child_scope = self.push_scope(
                    ScopeKind::ElseIfBlock,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.visit_children(node, child_scope);
            }
            "else_clause" => {
                let child_scope = self.push_scope(
                    ScopeKind::ElseBlock,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                self.visit_children(node, child_scope);
            }
            "for_numeric_statement" => {
                let child_scope = self.push_scope(
                    ScopeKind::ForNumeric,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                if let Some(name_node) = node.child_by_field_name("name") {
                    let db = name_node.start_byte();
                    self.add_decl(child_scope, ScopeDecl {
                        name: node_text(name_node, self.source).to_string(),
                        kind: DefKind::ForVariable,
                        decl_byte: db,
                        visible_after_byte: db,
                        range: ts_node_to_range(node, self.source),
                        selection_range: ts_node_to_range(name_node, self.source),
                    });
                }
                self.visit_children(node, child_scope);
            }
            "for_generic_statement" => {
                let child_scope = self.push_scope(
                    ScopeKind::ForGeneric,
                    node.start_byte(),
                    node.end_byte(),
                    Some(scope_id),
                );
                if let Some(names_node) = node.child_by_field_name("names") {
                    for i in 0..names_node.named_child_count() {
                        if let Some(id_node) = names_node.named_child(i as u32) {
                            if id_node.kind() == "identifier" {
                                let db = id_node.start_byte();
                                self.add_decl(child_scope, ScopeDecl {
                                    name: node_text(id_node, self.source).to_string(),
                                    kind: DefKind::ForVariable,
                                    decl_byte: db,
                                    visible_after_byte: db,
                                    range: ts_node_to_range(node, self.source),
                                    selection_range: ts_node_to_range(id_node, self.source),
                                });
                            }
                        }
                    }
                }
                self.visit_children(node, child_scope);
            }
            _ => {
                self.visit_children(node, scope_id);
            }
        }
    }

    fn collect_local_decl(&mut self, node: tree_sitter::Node<'a>, scope_id: usize) {
        if let Some(names_node) = node.child_by_field_name("names") {
            self.collect_identifiers_as_decl(names_node, DefKind::LocalVariable, node, scope_id);
        }
    }

    fn collect_local_func_decl(&mut self, node: tree_sitter::Node<'a>, scope_id: usize) {
        if let Some(name_node) = node.child_by_field_name("name") {
            if name_node.kind() == "identifier" {
                let db = name_node.start_byte();
                self.add_decl(scope_id, ScopeDecl {
                    name: node_text(name_node, self.source).to_string(),
                    kind: DefKind::LocalFunction,
                    decl_byte: db,
                    visible_after_byte: db,
                    range: ts_node_to_range(node, self.source),
                    selection_range: ts_node_to_range(name_node, self.source),
                });
            }
        }
    }

    fn collect_parameters(&mut self, func_body: tree_sitter::Node<'a>, scope_id: usize) {
        if let Some(params) = func_body.child_by_field_name("parameters") {
            for i in 0..params.named_child_count() {
                if let Some(child) = params.named_child(i as u32) {
                    if child.kind() == "identifier" {
                        let db = child.start_byte();
                        self.add_decl(scope_id, ScopeDecl {
                            name: node_text(child, self.source).to_string(),
                            kind: DefKind::Parameter,
                            decl_byte: db,
                            visible_after_byte: db,
                            range: ts_node_to_range(child, self.source),
                            selection_range: ts_node_to_range(child, self.source),
                        });
                    } else if child.kind() == "name_list" {
                        for j in 0..child.named_child_count() {
                            if let Some(id) = child.named_child(j as u32) {
                                if id.kind() == "identifier" {
                                    let db = id.start_byte();
                                    self.add_decl(scope_id, ScopeDecl {
                                        name: node_text(id, self.source).to_string(),
                                        kind: DefKind::Parameter,
                                        decl_byte: db,
                                        visible_after_byte: db,
                                        range: ts_node_to_range(id, self.source),
                                        selection_range: ts_node_to_range(id, self.source),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn collect_implicit_self(&mut self, func_body: tree_sitter::Node<'a>, scope_id: usize) {
        if let Some(parent) = func_body.parent() {
            if parent.kind() == "function_declaration" {
                if let Some(fname) = parent.child_by_field_name("name") {
                    if fname.child_by_field_name("method").is_some() {
                        let db = func_body.start_byte();
                        self.add_decl(scope_id, ScopeDecl {
                            name: "self".to_string(),
                            kind: DefKind::Parameter,
                            decl_byte: db,
                            visible_after_byte: db,
                            range: ts_node_to_range(func_body, self.source),
                            selection_range: ts_node_to_range(func_body, self.source),
                        });
                    }
                }
            }
        }
    }

    fn collect_identifiers_as_decl(
        &mut self,
        names_node: tree_sitter::Node<'a>,
        kind: DefKind,
        stmt_node: tree_sitter::Node<'a>,
        scope_id: usize,
    ) {
        let visible_after = stmt_node.end_byte();
        for i in 0..names_node.named_child_count() {
            if let Some(child) = names_node.named_child(i as u32) {
                if child.kind() == "identifier" {
                    self.add_decl(scope_id, ScopeDecl {
                        name: node_text(child, self.source).to_string(),
                        kind: kind.clone(),
                        decl_byte: child.start_byte(),
                        visible_after_byte: visible_after,
                        range: ts_node_to_range(stmt_node, self.source),
                        selection_range: ts_node_to_range(child, self.source),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Querying
// ---------------------------------------------------------------------------

impl ScopeTree {
    pub fn resolve(&self, byte_offset: usize, name: &str, uri: &Uri) -> Option<Definition> {
        let decl = self.resolve_decl(byte_offset, name)?;
        Some(Definition {
            name: decl.name.clone(),
            kind: decl.kind.clone(),
            range: decl.range,
            selection_range: decl.selection_range,
            uri: uri.clone(),
        })
    }

    pub fn resolve_decl(&self, byte_offset: usize, name: &str) -> Option<&ScopeDecl> {
        let scope_id = self.innermost_scope(byte_offset)?;
        let mut current = scope_id;
        loop {
            if let Some(decl) = self.find_decl_in_scope(current, byte_offset, name) {
                return Some(decl);
            }
            match self.scopes[current].parent {
                Some(pid) => current = pid,
                None => return None,
            }
        }
    }

    /// Iterate every declaration in every scope. Used by the
    /// `unused-local` diagnostic to walk every binding regardless of
    /// position. Order is scope-creation order, then within-scope
    /// declaration order.
    pub fn all_declarations(&self) -> impl Iterator<Item = &ScopeDecl> {
        self.scopes.iter().flat_map(|s| s.declarations.iter())
    }

    pub fn visible_locals(&self, byte_offset: usize) -> Vec<&ScopeDecl> {
        let mut result = Vec::new();
        let Some(scope_id) = self.innermost_scope(byte_offset) else {
            return result;
        };
        let mut current = scope_id;
        loop {
            let scope = &self.scopes[current];
            for decl in &scope.declarations {
                if decl.visible_after_byte < byte_offset {
                    result.push(decl);
                }
            }
            match scope.parent {
                Some(pid) => current = pid,
                None => break,
            }
        }
        result
    }

    pub fn scope_byte_range_for_def(&self, byte_offset: usize, name: &str) -> Option<(usize, usize)> {
        let scope_id = self.innermost_scope(byte_offset)?;
        let mut current = scope_id;
        loop {
            if self.find_decl_in_scope(current, byte_offset, name).is_some() {
                let scope = &self.scopes[current];
                return Some((scope.byte_start, scope.byte_end));
            }
            match self.scopes[current].parent {
                Some(pid) => current = pid,
                None => return None,
            }
        }
    }

    fn innermost_scope(&self, byte_offset: usize) -> Option<usize> {
        if self.scopes.is_empty() {
            return None;
        }
        let mut current = 0usize;
        'outer: loop {
            let scope = &self.scopes[current];
            for &child_id in &scope.children {
                let child = &self.scopes[child_id];
                if byte_offset >= child.byte_start && byte_offset < child.byte_end {
                    current = child_id;
                    continue 'outer;
                }
            }
            return Some(current);
        }
    }

    fn find_decl_in_scope(&self, scope_id: usize, byte_offset: usize, name: &str) -> Option<&ScopeDecl> {
        let scope = &self.scopes[scope_id];
        let mut best: Option<&ScopeDecl> = None;
        for decl in &scope.declarations {
            if decl.name != name {
                continue;
            }
            let on_decl_name = byte_offset >= decl.decl_byte
                && byte_offset < decl.decl_byte + decl.name.len();
            if on_decl_name || decl.visible_after_byte <= byte_offset {
                best = Some(decl);
            }
        }
        best
    }
}

