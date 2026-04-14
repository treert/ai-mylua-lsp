use std::collections::HashMap;
use tower_lsp_server::ls_types::{Range, Uri};

use crate::summary::{DocumentSummary, GlobalContributionKind};
use crate::type_system::TypeFact;

/// Workspace-level aggregation of all per-file summaries.
///
/// This is the "bridge" between single-file `DocumentSummary` instances
/// and cross-file queries (goto/hover/references/diagnostics).
/// See `index-architecture.md` §2.2.
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

impl WorkspaceAggregation {
    pub fn new() -> Self {
        Self {
            summaries: HashMap::new(),
            global_shard: HashMap::new(),
            type_shard: HashMap::new(),
            require_by_return: HashMap::new(),
            resolution_cache: HashMap::new(),
        }
    }

    /// Integrate a new or updated file summary into the aggregation layer.
    ///
    /// Performs a name-level diff: removes old contributions from this URI,
    /// inserts new ones, and marks affected resolution cache entries as dirty
    /// if the file's signature fingerprint changed.
    pub fn upsert_summary(&mut self, summary: DocumentSummary) {
        let uri = summary.uri.clone();
        let old_fingerprint = self
            .summaries
            .get(&uri)
            .map(|s| s.signature_fingerprint);

        self.remove_contributions(&uri);

        for gc in &summary.global_contributions {
            self.global_shard
                .entry(gc.name.clone())
                .or_default()
                .push(GlobalCandidate {
                    name: gc.name.clone(),
                    kind: gc.kind.clone(),
                    type_fact: gc.type_fact.clone(),
                    range: gc.range,
                    selection_range: gc.selection_range,
                    source_uri: uri.clone(),
                });
        }

        for td in &summary.type_definitions {
            self.type_shard
                .entry(td.name.clone())
                .or_default()
                .push(TypeCandidate {
                    name: td.name.clone(),
                    kind: td.kind.clone(),
                    source_uri: uri.clone(),
                    range: td.range,
                });
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

    fn remove_contributions(&mut self, uri: &Uri) {
        self.global_shard.retain(|_, candidates| {
            candidates.retain(|c| &c.source_uri != uri);
            !candidates.is_empty()
        });

        self.type_shard.retain(|_, candidates| {
            candidates.retain(|c| &c.source_uri != uri);
            !candidates.is_empty()
        });

        // Remove as target (this file was required by others).
        self.require_by_return.remove(uri);
        // Remove as source (this file required others).
        self.require_by_return.retain(|_, deps| {
            deps.retain(|d| &d.source_uri != uri);
            !deps.is_empty()
        });
    }

    /// Mark resolution cache entries as dirty when a file's signature changes.
    ///
    /// Current strategy is conservative: marks all cache entries dirty.
    /// TODO(step5): refine to only invalidate cache entries reachable from
    /// dependants listed in `require_by_return[uri]` and global/type references.
    fn invalidate_dependants(&mut self, _uri: &Uri) {
        for entry in self.resolution_cache.values_mut() {
            entry.dirty = true;
        }
    }

    /// Placeholder: resolve a module path string to a target URI.
    /// Will be backed by require path patterns + aliases from config.
    fn resolve_module_to_uri(&self, module_path: &str) -> Option<Uri> {
        // For now, look through all summaries to find one whose URI path matches.
        // This will be replaced with proper require resolution using config paths.
        for (uri, _) in &self.summaries {
            let uri_str = uri.to_string();
            let module_as_path = module_path.replace('.', "/");
            if uri_str.contains(&module_as_path) {
                return Some(uri.clone());
            }
        }
        None
    }
}
