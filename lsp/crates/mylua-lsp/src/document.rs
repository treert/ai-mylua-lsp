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

/// URI-facing lookup abstraction used while the backing document store
/// migrates from `Uri` keys to compact internal IDs.
pub trait DocumentLookup {
    fn get_document(&self, uri: &Uri) -> Option<&Document>;

    fn contains_document(&self, uri: &Uri) -> bool {
        self.get_document(uri).is_some()
    }

    fn for_each_document(&self, f: impl FnMut(&Uri, &Document));
}

impl DocumentLookup for HashMap<Uri, Document> {
    fn get_document(&self, uri: &Uri) -> Option<&Document> {
        self.get(uri)
    }

    fn for_each_document(&self, mut f: impl FnMut(&Uri, &Document)) {
        for (uri, doc) in self {
            f(uri, doc);
        }
    }
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
    fn get_document(&self, uri: &Uri) -> Option<&Document> {
        let uri_id = intern(uri.clone());
        self.documents.get(&uri_id)
    }

    fn for_each_document(&self, mut f: impl FnMut(&Uri, &Document)) {
        for (uri_id, doc) in self.documents {
            let uri = resolve(*uri_id);
            f(&uri, doc);
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
