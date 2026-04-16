use std::collections::HashMap;
use tower_lsp_server::ls_types::{Range, Uri};

use crate::summary::{DocumentSummary, GlobalContributionKind};
use crate::types::{DefKind, GlobalEntry};
use crate::type_system::TypeFact;
use crate::util::ts_node_to_range;

/// Workspace-level aggregation of all per-file summaries.
///
/// This is the "bridge" between single-file `DocumentSummary` instances
/// and cross-file queries (goto/hover/references/diagnostics).
/// See `index-architecture.md` §2.2.
///
/// Also maintains legacy `globals` and `require_map` fields for backward
/// compatibility with existing LSP handlers during the transition period.
#[derive(Debug)]
pub struct WorkspaceAggregation {
    /// All file summaries, keyed by URI.
    pub summaries: HashMap<Uri, DocumentSummary>,
    /// Global name → candidate definitions from all files.
    pub global_shard: HashMap<String, Vec<GlobalCandidate>>,
    /// Emmy type name → candidate definitions.
    pub type_shard: HashMap<String, Vec<TypeCandidate>>,
    /// Target URI → files that `require` it (reverse dependency index).
    pub require_by_return: HashMap<Uri, Vec<RequireDependant>>,
    /// Resolved cross-file type cache; entries are lazily populated and
    /// marked dirty on upstream signature changes.
    pub resolution_cache: HashMap<CacheKey, CachedResolution>,

    // -- Legacy compatibility fields (same shape as old WorkspaceIndex) --
    /// Global name → entries, consumed by goto/hover/references/completion/diagnostics.
    pub globals: HashMap<String, Vec<GlobalEntry>>,
    /// Module path → target URI, consumed by goto for require resolution.
    pub require_map: HashMap<String, Uri>,
}

/// A single candidate definition for a global name.
#[derive(Debug, Clone)]
pub struct GlobalCandidate {
    pub name: String,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: Range,
    pub selection_range: Range,
    pub source_uri: Uri,
}

/// A single candidate definition for an Emmy type name.
#[derive(Debug, Clone)]
pub struct TypeCandidate {
    pub name: String,
    pub kind: crate::summary::TypeDefinitionKind,
    pub source_uri: Uri,
    pub range: Range,
}

/// A file that depends on a given URI via `require`.
#[derive(Debug, Clone)]
pub struct RequireDependant {
    pub source_uri: Uri,
    pub local_name: String,
}

/// Key for the cross-file resolution cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CacheKey {
    RequireReturn { module_path: String },
    GlobalField { global_name: String, field: String },
    TypeField { type_name: String, field: String },
    CallReturn { base_key: Box<CacheKey>, func_name: String },
}

/// Cached result of cross-file type resolution.
#[derive(Debug, Clone)]
pub struct CachedResolution {
    pub resolved_type: TypeFact,
    pub dirty: bool,
}

/// Priority key for sorting candidates (smaller = higher priority):
/// 1. Paths containing "annotation" (case-insensitive) come first
/// 2. Shallower paths (fewer `/` segments) win
/// 3. Shorter total path length as tiebreaker
/// 4. Lexicographic URI string for full determinism
fn uri_priority_key(uri: &Uri) -> (u8, usize, usize, String) {
    let path = uri.to_string();
    let has_annotation = if path.to_ascii_lowercase().contains("annotation") { 0 } else { 1 };
    let depth = path.matches('/').count();
    (has_annotation, depth, path.len(), path)
}

impl WorkspaceAggregation {
    pub fn new() -> Self {
        Self {
            summaries: HashMap::new(),
            global_shard: HashMap::new(),
            type_shard: HashMap::new(),
            require_by_return: HashMap::new(),
            resolution_cache: HashMap::new(),
            globals: HashMap::new(),
            require_map: HashMap::new(),
        }
    }

    /// Integrate a new or updated file summary into the aggregation layer.
    ///
    /// Performs a name-level diff: removes old contributions from this URI,
    /// inserts new ones, and marks affected resolution cache entries as dirty
    /// if the file's signature fingerprint changed.
    /// Also synchronizes legacy `globals` field for backward compatibility.
    pub fn upsert_summary(&mut self, summary: DocumentSummary) {
        let uri = summary.uri.clone();
        let old_fingerprint = self
            .summaries
            .get(&uri)
            .map(|s| s.signature_fingerprint);

        self.remove_contributions(&uri);

        for gc in &summary.global_contributions {
            let candidates = self.global_shard
                .entry(gc.name.clone())
                .or_default();
            candidates.push(GlobalCandidate {
                name: gc.name.clone(),
                kind: gc.kind.clone(),
                type_fact: gc.type_fact.clone(),
                range: gc.range,
                selection_range: gc.selection_range,
                source_uri: uri.clone(),
            });
            candidates.sort_by_cached_key(|c| uri_priority_key(&c.source_uri));

            let def_kind = match gc.kind {
                GlobalContributionKind::Function => DefKind::GlobalFunction,
                _ => DefKind::GlobalVariable,
            };
            let entries = self.globals
                .entry(gc.name.clone())
                .or_default();
            entries.push(GlobalEntry {
                name: gc.name.clone(),
                kind: def_kind,
                range: gc.range,
                selection_range: gc.selection_range,
                uri: uri.clone(),
            });
            entries.sort_by_cached_key(|e| uri_priority_key(&e.uri));
        }

        for td in &summary.type_definitions {
            let candidates = self.type_shard
                .entry(td.name.clone())
                .or_default();
            candidates.push(TypeCandidate {
                name: td.name.clone(),
                kind: td.kind.clone(),
                source_uri: uri.clone(),
                range: td.range,
            });
            candidates.sort_by_cached_key(|c| uri_priority_key(&c.source_uri));
        }

        for rb in &summary.require_bindings {
            if let Some(target_uri) = self.resolve_module_to_uri(&rb.module_path) {
                self.require_by_return
                    .entry(target_uri)
                    .or_default()
                    .push(RequireDependant {
                        source_uri: uri.clone(),
                        local_name: rb.local_name.clone(),
                    });
            }
        }

        let fingerprint_changed = old_fingerprint
            .map_or(true, |old| old != summary.signature_fingerprint);
        if fingerprint_changed {
            self.invalidate_dependants(&uri);
        }

        self.summaries.insert(uri, summary);
    }

    /// Remove a file from the aggregation layer entirely.
    pub fn remove_file(&mut self, uri: &Uri) {
        self.remove_contributions(uri);
        self.summaries.remove(uri);
    }

    /// Legacy compatibility: update index from raw AST (used during workspace scan
    /// for files that haven't gone through summary_builder yet).
    pub fn update_document_legacy(
        &mut self,
        uri: &Uri,
        tree: &tree_sitter::Tree,
        source: &[u8],
    ) {
        self.remove_legacy_globals(uri);
        self.scan_globals_legacy(uri, tree.root_node(), source);
    }

    /// Legacy compatibility: set require mapping directly.
    pub fn set_require_mapping(&mut self, module_path: String, uri: Uri) {
        self.require_map.insert(module_path, uri);
    }

    fn remove_contributions(&mut self, uri: &Uri) {
        self.global_shard.retain(|_, candidates| {
            candidates.retain(|c| &c.source_uri != uri);
            !candidates.is_empty()
        });

        self.type_shard.retain(|_, candidates| {
            candidates.retain(|c| &c.source_uri != uri);
            !candidates.is_empty()
        });

        self.require_by_return.remove(uri);
        self.require_by_return.retain(|_, deps| {
            deps.retain(|d| &d.source_uri != uri);
            !deps.is_empty()
        });

        self.require_map.retain(|_, target_uri| target_uri != uri);

        self.remove_legacy_globals(uri);
    }

    fn remove_legacy_globals(&mut self, uri: &Uri) {
        self.globals.retain(|_, entries| {
            entries.retain(|e| &e.uri != uri);
            !entries.is_empty()
        });
    }

    fn scan_globals_legacy(
        &mut self,
        uri: &Uri,
        root: tree_sitter::Node,
        source: &[u8],
    ) {
        use crate::util::node_text;
        let mut cursor = root.walk();
        if !cursor.goto_first_child() {
            return;
        }
        loop {
            let node = cursor.node();
            match node.kind() {
                "function_declaration" => {
                    if let Some(name_node) = node.child_by_field_name("name") {
                        let name = node_text(name_node, source).to_string();
                        let entries = self.globals.entry(name.clone()).or_default();
                        entries.push(GlobalEntry {
                            name,
                            kind: DefKind::GlobalFunction,
                            range: ts_node_to_range(node),
                            selection_range: ts_node_to_range(name_node),
                            uri: uri.clone(),
                        });
                        entries.sort_by_cached_key(|e| uri_priority_key(&e.uri));
                    }
                }
                "assignment_statement" => {
                    if let Some(left_node) = node.child_by_field_name("left") {
                        if let Some(first_var) = left_node.named_child(0) {
                            let name = node_text(first_var, source).to_string();
                            let entries = self.globals.entry(name.clone()).or_default();
                            entries.push(GlobalEntry {
                                name,
                                kind: DefKind::GlobalVariable,
                                range: ts_node_to_range(node),
                                selection_range: ts_node_to_range(first_var),
                                uri: uri.clone(),
                            });
                            entries.sort_by_cached_key(|e| uri_priority_key(&e.uri));
                        }
                    }
                }
                _ => {}
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    /// Mark resolution cache entries as dirty when a file's signature changes.
    ///
    /// Current strategy is conservative: marks all cache entries dirty.
    /// TODO: refine to only invalidate entries reachable from dependants
    /// listed in `require_by_return[uri]` and global/type references.
    fn invalidate_dependants(&mut self, _uri: &Uri) {
        for entry in self.resolution_cache.values_mut() {
            entry.dirty = true;
        }
    }

    fn resolve_module_to_uri(&self, module_path: &str) -> Option<Uri> {
        if let Some(uri) = self.require_map.get(module_path) {
            return Some(uri.clone());
        }
        // Fallback: check summaries for a URI path match (handles
        // modules discovered after require_map was built).
        let module_as_path = module_path.replace('.', "/");
        for (uri, _) in &self.summaries {
            let uri_str = uri.to_string();
            if uri_str.contains(&module_as_path) {
                return Some(uri.clone());
            }
        }
        None
    }
}
