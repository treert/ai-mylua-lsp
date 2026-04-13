use tower_lsp_server::ls_types::{Range, Uri};

#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub kind: DefKind,
    pub range: Range,
    pub selection_range: Range,
    pub uri: Uri,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefKind {
    LocalVariable,
    LocalFunction,
    GlobalVariable,
    GlobalFunction,
    Parameter,
    ForVariable,
}

#[derive(Debug, Clone)]
pub struct GlobalEntry {
    pub name: String,
    pub kind: DefKind,
    pub range: Range,
    pub selection_range: Range,
    pub uri: Uri,
}
