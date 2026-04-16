use std::collections::{HashMap, HashSet};
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

    /// Module path → target URI, used by `resolve_module_to_uri`.
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
    /// Resolve a global name itself (not a field on it).
    Global { name: String },
    /// Resolve a field on a global: `name.field`.
    GlobalField { global_name: String, field: String },
    /// Resolve an Emmy type name itself (not a field on it).
    Type { name: String },
    /// Resolve a field on an Emmy type: `type.field`.
    TypeField { type_name: String, field: String },
    CallReturn { base_key: Box<CacheKey>, func_name: String },
    FieldAccess { base_key: Box<CacheKey>, field: String },
}

/// Cached result of cross-file type resolution.
#[derive(Debug, Clone)]
pub struct CachedResolution {
    pub resolved_type: TypeFact,
    pub dirty: bool,
}

/// Names affected by a file update, used for targeted cache invalidation.
struct AffectedNames {
    module_paths: HashSet<String>,
    global_names: HashSet<String>,
    type_names: HashSet<String>,
}

/// Check whether a cache key transitively depends on any of the affected names.
fn cache_key_affected(key: &CacheKey, affected: &AffectedNames) -> bool {
    match key {
        CacheKey::RequireReturn { module_path } => {
            affected.module_paths.contains(module_path)
        }
        CacheKey::Global { name } | CacheKey::GlobalField { global_name: name, .. } => {
            affected.global_names.contains(name)
        }
        CacheKey::Type { name } | CacheKey::TypeField { type_name: name, .. } => {
            affected.type_names.contains(name)
        }
        CacheKey::CallReturn { base_key, .. } | CacheKey::FieldAccess { base_key, .. } => {
            cache_key_affected(base_key, affected)
        }
    }
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

        // Collect affected names from BOTH old and new summaries before removal,
        // so invalidation can target the right cache entries.
        let affected = self.collect_affected_names(&uri, &summary);

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
            self.invalidate_dependants_targeted(&affected);
        }

        self.summaries.insert(uri, summary);
    }

    /// Remove a file from the aggregation layer entirely.
    pub fn remove_file(&mut self, uri: &Uri) {
        self.remove_contributions(uri);
        self.summaries.remove(uri);
    }

    /// Set require mapping directly (module path → target URI).
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

        // Only remove entries where THIS file is the *source* (requirer),
        // not where it is the *target* (required module).
        self.require_by_return.retain(|_, deps| {
            deps.retain(|d| &d.source_uri != uri);
            !deps.is_empty()
        });

        self.require_map.retain(|_, target_uri| target_uri != uri);
    }

    /// Collect the set of module paths, global names, and type names affected
    /// by updating a file. Merges contributions from the old summary (if any)
    /// and the new summary so that both added and removed names are covered.
    fn collect_affected_names(
        &self,
        uri: &Uri,
        new_summary: &DocumentSummary,
    ) -> AffectedNames {
        let mut module_paths: HashSet<String> = HashSet::new();
        let mut global_names: HashSet<String> = HashSet::new();
        let mut type_names: HashSet<String> = HashSet::new();

        // Names from the old summary (about to be removed)
        if let Some(old) = self.summaries.get(uri) {
            for gc in &old.global_contributions {
                global_names.insert(gc.name.clone());
            }
            for td in &old.type_definitions {
                type_names.insert(td.name.clone());
            }
        }

        // Names from the new summary (about to be inserted)
        for gc in &new_summary.global_contributions {
            global_names.insert(gc.name.clone());
        }
        for td in &new_summary.type_definitions {
            type_names.insert(td.name.clone());
        }

        // Module paths that resolve to this URI
        for (mod_path, target_uri) in &self.require_map {
            if target_uri == uri {
                module_paths.insert(mod_path.clone());
            }
        }

        AffectedNames { module_paths, global_names, type_names }
    }

    /// Mark only the resolution cache entries that transitively depend on the
    /// affected names (global contributions, type definitions, or require
    /// targets from the changed file).
    fn invalidate_dependants_targeted(&mut self, affected: &AffectedNames) {
        if affected.module_paths.is_empty()
            && affected.global_names.is_empty()
            && affected.type_names.is_empty()
        {
            return;
        }
        for (key, entry) in self.resolution_cache.iter_mut() {
            if !entry.dirty && cache_key_affected(key, affected) {
                entry.dirty = true;
            }
        }
    }

    pub fn resolve_module_to_uri(&self, module_path: &str) -> Option<Uri> {
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
