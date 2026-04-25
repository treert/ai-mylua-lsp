use tower_lsp_server::ls_types::Uri;
use crate::util::ByteRange;

#[derive(Debug, Clone)]
pub struct Definition {
    pub name: String,
    pub kind: DefKind,
    pub range: ByteRange,
    pub selection_range: ByteRange,
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
    pub range: ByteRange,
    pub selection_range: ByteRange,
    pub uri: Uri,
}
