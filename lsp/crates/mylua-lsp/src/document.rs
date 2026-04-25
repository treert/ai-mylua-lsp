use crate::scope::ScopeTree;
use crate::util::LineIndex;

pub struct Document {
    pub text: String,
    pub tree: tree_sitter::Tree,
    pub scope_tree: ScopeTree,
    pub line_index: LineIndex,
}
