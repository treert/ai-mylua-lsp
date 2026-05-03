use std::collections::HashMap;

use crate::scope::ScopeTree;
use crate::uri_id::{intern, resolve, UriId};
use crate::util::{LineIndex, LuaSource};
use tower_lsp_server::ls_types::Uri;

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

/// Document lookup abstraction keyed by compact internal IDs.
pub trait DocumentLookup {
    fn get_document_by_id(&self, uri_id: UriId) -> Option<&Document>;

    fn contains_document_id(&self, uri_id: UriId) -> bool {
        self.get_document_by_id(uri_id).is_some()
    }

    fn for_each_document_id(&self, f: impl FnMut(UriId, &Document));
}

pub struct DocumentStoreView<'a> {
    documents: &'a HashMap<UriId, Document>,
}

impl<'a> DocumentStoreView<'a> {
    pub fn new(documents: &'a HashMap<UriId, Document>) -> Self {
        Self { documents }
    }
}

pub(crate) fn find_document<'a>(
    documents: &'a HashMap<UriId, Document>,
    uri: &Uri,
) -> Option<(UriId, &'a Document)> {
    let uri_id = intern(uri.clone());
    documents.get(&uri_id).map(|doc| (uri_id, doc))
}

impl DocumentLookup for DocumentStoreView<'_> {
    fn get_document_by_id(&self, uri_id: UriId) -> Option<&Document> {
        self.documents.get(&uri_id)
    }

    fn for_each_document_id(&self, mut f: impl FnMut(UriId, &Document)) {
        for (uri_id, doc) in self.documents {
            f(*uri_id, doc);
        }
    }
}

impl DocumentLookup for HashMap<Uri, Document> {
    fn get_document_by_id(&self, uri_id: UriId) -> Option<&Document> {
        self.get(&resolve(uri_id))
    }

    fn for_each_document_id(&self, mut f: impl FnMut(UriId, &Document)) {
        for (uri, doc) in self {
            f(intern(uri.clone()), doc);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{find_document, Document};
    use crate::uri_id::UriId;
    use std::collections::HashMap;
    use tower_lsp_server::ls_types::Uri;

    #[test]
    fn find_document_returns_none_for_unknown_uri() {
        let documents: HashMap<UriId, Document> = HashMap::new();
        let uri: Uri = "file:///tmp/missing.lua".parse().unwrap();

        assert!(find_document(&documents, &uri).is_none());
    }
}
