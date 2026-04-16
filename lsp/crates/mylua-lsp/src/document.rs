use crate::scope::ScopeTree;

pub struct Document {
    pub text: String,
    pub tree: tree_sitter::Tree,
    pub scope_tree: ScopeTree,
}
