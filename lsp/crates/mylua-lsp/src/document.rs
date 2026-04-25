use crate::scope::ScopeTree;
use crate::util::{LineIndex, LuaSource};

pub struct Document {
    pub lua_source: LuaSource,
    pub tree: tree_sitter::Tree,
    pub scope_tree: ScopeTree,
}

impl Document {
    /// Raw source bytes (`&[u8]`).
    #[inline]
    pub fn source(&self) -> &[u8] {
        self.lua_source.source()
    }

    /// Source text as `&str`.
    #[inline]
    pub fn text(&self) -> &str {
        self.lua_source.text()
    }

    /// The pre-computed line index.
    #[inline]
    pub fn line_index(&self) -> &LineIndex {
        self.lua_source.line_index()
    }
}
