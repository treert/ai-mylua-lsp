use tower_lsp_server::ls_types::Uri;
use crate::types::{DefKind, Definition};
use crate::util::ByteRange;

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
    pub range: ByteRange,
    pub selection_range: ByteRange,
    /// Inferred type for this declaration; None when unknown.
    pub type_fact: Option<crate::type_system::TypeFact>,
    /// Class anchor binding for Phase 2; None when not yet resolved.
    pub bound_class: Option<String>,
    /// `true` when the type was specified via an Emmy annotation
    /// (e.g. `---@type X`). Used by diagnostics that only fire
    /// for explicitly annotated locals.
    pub is_emmy_annotated: bool,
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
// Querying
// ---------------------------------------------------------------------------

impl ScopeTree {
    /// Construct a ScopeTree from a pre-built scope vector.
    /// Used by `build_file_analysis` which builds scopes during summary construction.
    pub fn from_scopes(scopes: Vec<Scope>) -> Self {
        ScopeTree { scopes }
    }
}

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

    pub fn resolve_type(&self, byte_offset: usize, name: &str) -> Option<&crate::type_system::TypeFact> {
        self.resolve_decl(byte_offset, name)?.type_fact.as_ref()
    }

    pub fn resolve_bound_class(&self, byte_offset: usize, name: &str) -> Option<&str> {
        self.resolve_decl(byte_offset, name)?.bound_class.as_deref()
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

