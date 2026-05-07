use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::scope::ScopeTree;
use crate::uri_id::{intern_uri, UriId};
use crate::util::{LineIndex, LuaSource};
use tower_lsp_server::ls_types::{Diagnostic, Uri};

pub struct Document {
    pub lua_source: LuaSource,
    pub tree: Option<tree_sitter::Tree>,
    pub scope_tree: ScopeTree,
    pub last_diagnostic_signature: Option<u64>,
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

    /// Returns the cached tree, if this document kept or already rebuilt it.
    #[inline]
    pub fn tree(&self) -> Option<&tree_sitter::Tree> {
        self.tree.as_ref()
    }

    /// Returns the cached root node, if the tree is currently available.
    #[inline]
    pub fn root_node(&self) -> Option<tree_sitter::Node<'_>> {
        self.tree().map(|tree| tree.root_node())
    }

    /// Lazily rebuilds the tree from the resident source text.
    pub(crate) fn ensure_tree(&mut self) -> Option<&tree_sitter::Tree> {
        if self.tree.is_none() {
            self.tree = self.parse_tree();
        }
        self.tree()
    }

    /// Parses the resident source without writing the result back.
    pub(crate) fn parse_tree(&self) -> Option<tree_sitter::Tree> {
        let mut parser = crate::new_parser();
        parser.parse(self.source(), None)
    }

    pub(crate) fn diagnostic_signature(diagnostics: &[Diagnostic]) -> u64 {
        let bytes = serde_json::to_vec(diagnostics)
            .expect("LSP diagnostics should always serialize");
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }

    /// Records the latest diagnostics and returns whether clients need an update.
    pub(crate) fn remember_diagnostic_signature(&mut self, diagnostics: &[Diagnostic]) -> bool {
        let signature = Self::diagnostic_signature(diagnostics);
        if self.last_diagnostic_signature == Some(signature) {
            return false;
        }
        self.last_diagnostic_signature = Some(signature);
        true
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
    let uri_id = intern_uri(uri);
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


#[cfg(test)]
mod tests {
    use super::{find_document, Document};
    use crate::{new_parser, summary_builder};
    use crate::uri_id::UriId;
    use crate::util::LuaSource;
    use std::collections::HashMap;
    use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, Position, Range, Uri};

    #[test]
    fn find_document_returns_none_for_unknown_uri() {
        let documents: HashMap<UriId, Document> = HashMap::new();
        let uri: Uri = "file:///tmp/missing.lua".parse().unwrap();

        assert!(find_document(&documents, &uri).is_none());
    }

    fn diagnostic(message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line: 0, character: 0 },
                end: Position { line: 0, character: 1 },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            code: None,
            code_description: None,
            source: Some("mylua".to_string()),
            message: message.to_string(),
            related_information: None,
            tags: None,
            data: None,
        }
    }

    #[test]
    fn diagnostic_signature_changes_when_diagnostics_change() {
        let first = vec![diagnostic("first")];
        let same = vec![diagnostic("first")];
        let changed = vec![diagnostic("changed")];

        assert_eq!(
            Document::diagnostic_signature(&first),
            Document::diagnostic_signature(&same)
        );
        assert_ne!(
            Document::diagnostic_signature(&first),
            Document::diagnostic_signature(&changed)
        );
    }

    #[test]
    fn remember_diagnostic_signature_suppresses_repeated_results() {
        let src = "local x = 1";
        let mut parser = new_parser();
        let tree = parser.parse(src.as_bytes(), None).unwrap();
        let lua_source = LuaSource::new(src.to_string());
        let uri: Uri = "file:///tmp/signature.lua".parse().unwrap();
        let (_, scope_tree) = summary_builder::build_file_analysis(
            &uri,
            &tree,
            lua_source.source(),
            lua_source.line_index(),
        );
        let mut doc = Document {
            lua_source,
            tree: Some(tree),
            scope_tree,
            last_diagnostic_signature: None,
        };

        let first = vec![diagnostic("first")];
        let same = vec![diagnostic("first")];
        let changed = vec![diagnostic("changed")];

        assert!(doc.remember_diagnostic_signature(&first));
        assert!(!doc.remember_diagnostic_signature(&same));
        assert!(doc.remember_diagnostic_signature(&changed));
    }

    #[test]
    fn ensure_tree_rebuilds_missing_tree_from_source() {
        let src = "local cached = 1";
        let mut parser = new_parser();
        let tree = parser.parse(src.as_bytes(), None).unwrap();
        let lua_source = LuaSource::new(src.to_string());
        let uri: Uri = "file:///tmp/cache.lua".parse().unwrap();
        let (_, scope_tree) = summary_builder::build_file_analysis(
            &uri,
            &tree,
            lua_source.source(),
            lua_source.line_index(),
        );
        let mut doc = Document {
            lua_source,
            tree: None,
            scope_tree,
            last_diagnostic_signature: None,
        };

        let rebuilt = doc.ensure_tree().expect("tree should parse");

        assert_eq!(rebuilt.root_node().kind(), "source_file");
        assert!(doc.tree.is_some());
    }
}
