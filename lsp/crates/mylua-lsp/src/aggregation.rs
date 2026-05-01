use std::collections::{HashMap, HashSet};
use tower_lsp_server::ls_types::Uri;

use crate::summary::{DocumentSummary, GlobalContributionKind};
use crate::type_system::TypeFact;
use crate::util::ByteRange;

/// Workspace-level aggregation of all per-file summaries.
///
/// This is the "bridge" between single-file `DocumentSummary` instances
/// and cross-file queries (goto/hover/references/diagnostics).
/// See `index-architecture.md` §2.2.
#[derive(Debug)]
pub struct WorkspaceAggregation {
    /// All file summaries, keyed by URI.
    pub summaries: HashMap<Uri, DocumentSummary>,
    /// Global name tree — candidate definitions from all files.
    pub global_shard: GlobalShard,
    /// Emmy type name → candidate definitions.
    pub type_shard: HashMap<String, Vec<TypeCandidate>>,
    /// Target URI → files that `require` it (reverse dependency index).
    pub require_by_return: HashMap<Uri, Vec<RequireDependant>>,
    /// Emmy type name → URIs whose summaries reference that type
    /// (P1-7 reverse type-dependency graph). Enables cascade
    /// invalidation when a class definition changes — any file that
    /// `@type`s / `@param`s / inherits from / has fields typed as
    /// the changed class needs its semantic diagnostics recomputed,
    /// even if it doesn't `require()` the defining file.
    pub type_dependants: HashMap<String, HashSet<Uri>>,
    /// Resolved cross-file type cache; entries are lazily populated and
    /// marked dirty on upstream signature changes.
    pub resolution_cache: HashMap<CacheKey, CachedResolution>,

    /// Module index: last_segment → Vec<(full_module_name, Uri)>.
    /// Used by `resolve_module_to_uri` for O(1) last-segment lookup
    /// followed by longest-suffix matching among candidates.
    pub module_index: HashMap<String, Vec<(String, Uri)>>,
    /// Require path aliases (e.g. `{"@": "src/"}`), applied during module resolution.
    /// The longest matching prefix alias is chosen.
    pub require_aliases: HashMap<String, String>,
}

/// A single candidate definition for a global name.
#[derive(Debug, Clone)]
pub struct GlobalCandidate {
    pub name: String,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: ByteRange,
    pub selection_range: ByteRange,
    pub source_uri: Uri,
}

// ---------------------------------------------------------------------------
// GlobalShard — tree-structured global name index
// ---------------------------------------------------------------------------

/// A node in the global-name trie.
///
/// For a path like `"UE4.FVector.new"`, the tree looks like:
/// ```text
/// roots["UE4"]
///   └─ children["FVector"]
///        └─ children["new"]
/// ```
///
/// Both `.` and `:` are treated as segment separators (colon is just
/// syntactic sugar for a method with implicit `self`; the colon/dot
/// distinction is already captured in `FunctionSignature.params`).
#[derive(Debug, Default)]
pub struct GlobalNode {
    /// Candidate definitions at this exact path.
    /// Empty when this node is a structural-only ancestor of deeper entries.
    pub candidates: Vec<GlobalCandidate>,
    /// Child nodes keyed by the next segment name.
    pub children: HashMap<String, GlobalNode>,
}

/// Tree-structured global shard, replacing the flat
/// `HashMap<String, Vec<GlobalCandidate>>`.
///
/// Provides O(depth) exact lookup, O(children) direct-child enumeration,
/// and O(contributions-per-file) URI-based removal via a reverse index.
#[derive(Debug, Default)]
pub struct GlobalShard {
    /// Top-level entries: `"print"`, `"UE4"`, `"Foo"`, etc.
    roots: HashMap<String, GlobalNode>,
    /// Reverse index: URI → full-path strings contributed by that URI.
    /// Maintained by `push_candidate` / `remove_by_uri` / `clear`.
    uri_to_paths: HashMap<Uri, Vec<String>>,
}

/// Split a path string on `.` and `:` separators.
/// Returns `(root, [segment, ...])`.
///
/// Examples:
/// - `"print"` → `("print", [])`
/// - `"UE4.FVector"` → `("UE4", ["FVector"])`
/// - `"Foo:bar"` → `("Foo", ["bar"])`
/// - `"UE4.FVector:normalize"` → `("UE4", ["FVector", "normalize"])`
fn split_global_path(path: &str) -> (&str, Vec<&str>) {
    let mut segments = Vec::new();
    let root_end = path.find(|c: char| c == '.' || c == ':');
    let Some(root_end) = root_end else {
        return (path, segments);
    };
    let root = &path[..root_end];
    let mut pos = root_end;
    while pos < path.len() {
        pos += 1; // skip separator
        let next = path[pos..].find(|c: char| c == '.' || c == ':')
            .map(|off| pos + off)
            .unwrap_or(path.len());
        segments.push(&path[pos..next]);
        pos = next;
    }
    (root, segments)
}

impl GlobalNode {
    fn new() -> Self {
        Self::default()
    }

    /// Recursively sort every node's candidate list.
    fn sort_all_recursive<F, K>(&mut self, key_fn: &F)
    where
        F: Fn(&GlobalCandidate) -> K,
        K: Ord,
    {
        if !self.candidates.is_empty() {
            self.candidates.sort_by_cached_key(key_fn);
        }
        for child in self.children.values_mut() {
            child.sort_all_recursive(key_fn);
        }
    }

    /// DFS collect all entries with non-empty candidates.
    /// `prefix` is the full path up to (and including) this node.
    fn collect_entries<'a>(&'a self, prefix: &str, out: &mut Vec<(String, &'a Vec<GlobalCandidate>)>) {
        if !self.candidates.is_empty() {
            // Use the candidate's own `name` field to preserve the
            // original separator (`:` vs `.`). The trie merges both
            // separator types into a single `children` map, so
            // reconstructing from the tree would always produce `.`.
            let key = self.candidates[0].name.clone();
            out.push((key, &self.candidates));
        }
        for (seg, child) in &self.children {
            let child_path = format!("{}.{}", prefix, seg);
            child.collect_entries(&child_path, out);
        }
    }
}

impl GlobalShard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up candidates at an exact path.
    /// Returns `None` if the path doesn't exist or has no candidates.
    pub fn get(&self, path: &str) -> Option<&Vec<GlobalCandidate>> {
        let node = self.get_node(path)?;
        if node.candidates.is_empty() {
            None
        } else {
            Some(&node.candidates)
        }
    }

    /// Mutable lookup for candidates at an exact path.
    pub fn get_mut(&mut self, path: &str) -> Option<&mut Vec<GlobalCandidate>> {
        let node = self.get_node_mut(path)?;
        if node.candidates.is_empty() {
            None
        } else {
            Some(&mut node.candidates)
        }
    }

    /// Check whether candidates exist at an exact path.
    pub fn contains_key(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    /// Navigate to a node at the given path.
    pub fn get_node(&self, path: &str) -> Option<&GlobalNode> {
        let (root, segments) = split_global_path(path);
        let mut node = self.roots.get(root)?;
        for seg in segments {
            node = node.children.get(seg)?;
        }
        Some(node)
    }

    /// Navigate to a node at the given path (mutable).
    fn get_node_mut(&mut self, path: &str) -> Option<&mut GlobalNode> {
        let (root, segments) = split_global_path(path);
        let mut node = self.roots.get_mut(root)?;
        for seg in segments {
            node = node.children.get_mut(seg)?;
        }
        Some(node)
    }

    /// Insert a candidate at the given path, creating intermediate nodes
    /// as needed. Also updates the reverse URI→path index.
    pub fn push_candidate(&mut self, path: &str, candidate: GlobalCandidate) {
        // Update reverse index.
        self.uri_to_paths
            .entry(candidate.source_uri.clone())
            .or_default()
            .push(path.to_string());

        // Walk/create nodes.
        let (root, segments) = split_global_path(path);
        let mut node = self.roots
            .entry(root.to_string())
            .or_insert_with(GlobalNode::new);
        for seg in segments {
            node = node.children
                .entry(seg.to_string())
                .or_insert_with(GlobalNode::new);
        }
        node.candidates.push(candidate);
    }

    /// Sort candidates at a specific path.
    pub fn sort_at<F, K>(&mut self, path: &str, key_fn: F)
    where
        F: Fn(&GlobalCandidate) -> K,
        K: Ord,
    {
        if let Some(candidates) = self.get_mut(path) {
            candidates.sort_by_cached_key(key_fn);
        }
    }

    /// Recursively sort all candidate lists in the tree.
    pub fn sort_all<F, K>(&mut self, key_fn: F)
    where
        F: Fn(&GlobalCandidate) -> K,
        K: Ord,
    {
        for node in self.roots.values_mut() {
            node.sort_all_recursive(&key_fn);
        }
    }

    /// Remove all candidates contributed by a given URI.
    /// Uses the reverse index for O(contributions-per-file) work.
    pub fn remove_by_uri(&mut self, uri: &Uri) {
        let Some(paths) = self.uri_to_paths.remove(uri) else { return };
        for path in &paths {
            if let Some(node) = self.get_node_mut(path) {
                node.candidates.retain(|c| &c.source_uri != uri);
            }
        }
        // Note: we leave empty structural nodes in place.
        // They are harmless (get() returns None for empty candidates)
        // and are wiped on the next build_initial() → clear().
    }

    /// Clear all data.
    pub fn clear(&mut self) {
        self.roots.clear();
        self.uri_to_paths.clear();
    }

    /// DFS iterate over all entries with non-empty candidates.
    /// Yields `(full_path, &Vec<GlobalCandidate>)`.
    pub fn iter_all_entries(&self) -> Vec<(String, &Vec<GlobalCandidate>)> {
        let mut out = Vec::new();
        for (root_name, root_node) in &self.roots {
            root_node.collect_entries(root_name, &mut out);
        }
        out
    }

    /// Iterate entries whose root name starts with `prefix`, collecting
    /// matching roots and all their descendants.
    ///
    /// This is used for global completion: the user types `"UE"` and we
    /// need to match `"UE4"`, `"UE4.FVector"`, `"UE4.FVector.new"` etc.
    /// The prefix never contains `.` or `:` (it's a bare identifier prefix).
    pub fn iter_roots_with_prefix(&self, prefix: &str) -> Vec<(String, &Vec<GlobalCandidate>)> {
        let mut out = Vec::new();
        for (root_name, root_node) in &self.roots {
            if root_name.starts_with(prefix) {
                root_node.collect_entries(root_name, &mut out);
            }
        }
        out
    }
}

/// A single candidate definition for an Emmy type name.
#[derive(Debug, Clone)]
pub struct TypeCandidate {
    pub name: String,
    pub kind: crate::summary::TypeDefinitionKind,
    pub source_uri: Uri,
    pub range: ByteRange,
}

/// A file that depends on a given URI via `require`.
#[derive(Debug, Clone)]
pub struct RequireDependant {
    pub source_uri: Uri,
    pub local_name: String,
}

/// Key for the cross-file resolution cache.
///
/// `FieldAccess { base_key, field }` covers both "field on a global" and
/// "field on a type" via its `base_key` — there is no separate `GlobalField`
/// / `TypeField` variant. Keep this enum minimal to avoid dead code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CacheKey {
    RequireReturn { module_path: String },
    /// Resolve a global name itself (not a field on it).
    Global { name: String },
    /// Resolve an Emmy type name itself (not a field on it).
    Type { name: String },
    CallReturn { base_key: Box<CacheKey>, func_name: String, is_method_call: bool },
    FieldAccess { base_key: Box<CacheKey>, field: String },
}

/// Cached result of cross-file type resolution.
///
/// `def_uri` / `def_range` are preserved so that subsequent cache hits
/// produce the *same* `ResolvedType` as the cold-path resolution —
/// crucial for `Known(Table(shape_id))` where the shape id is per-file
/// and a dropped `def_uri` silently turns table field lookups into
/// `Unknown` (manifested as false-positive `Unknown field` diagnostics
/// and hover returning no type on the second access).
#[derive(Debug, Clone)]
pub struct CachedResolution {
    pub resolved_type: TypeFact,
    pub def_uri: Option<Uri>,
    pub def_range: Option<ByteRange>,
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
        CacheKey::Global { name } => affected.global_names.contains(name),
        CacheKey::Type { name } => affected.type_names.contains(name),
        CacheKey::CallReturn { base_key, .. } | CacheKey::FieldAccess { base_key, .. } => {
            cache_key_affected(base_key, affected)
        }
    }
}

/// Priority key for sorting candidates (smaller = higher priority):
/// 1. More occurrences of "annotation" (case-insensitive) in the path = higher priority
/// 2. Shallower paths (fewer `/` segments) win
/// 3. Shorter total path length as tiebreaker
/// 4. Lexicographic URI string for full determinism
fn uri_priority_key(uri: &Uri) -> (usize, usize, usize) {
    let path = uri.as_str();
    let lower = path.to_ascii_lowercase();
    let annotation_count = lower.matches("annotation").count();
    // Negate: more occurrences → smaller key → higher priority
    let annotation_key = usize::MAX - annotation_count;
    let depth = path.matches('/').count();
    (annotation_key, depth, path.len())
}

impl Default for WorkspaceAggregation {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceAggregation {
    pub fn new() -> Self {
        Self {
            summaries: HashMap::new(),
            global_shard: GlobalShard::new(),
            type_shard: HashMap::new(),
            require_by_return: HashMap::new(),
            type_dependants: HashMap::new(),
            resolution_cache: HashMap::new(),
            module_index: HashMap::new(),
            require_aliases: HashMap::new(),
        }
    }

    /// Build the initial global index atomically from a complete set of
    /// file summaries. This is the cold-start path: all summaries are
    /// available at once, so we skip `remove_contributions` (nothing to
    /// remove) and `resolve_module_to_uri` benefits from a fully
    /// populated `require_map` + `summaries` — eliminating the
    /// batch-ordering bug where early files couldn't resolve modules
    /// defined in later batches.
    ///
    /// After this call, `upsert_summary` handles incremental updates.
    pub fn build_initial(&mut self, summaries: Vec<DocumentSummary>) {
        // Clear all shards — build_initial is a full rebuild. Any
        // contributions from did_open's upsert_summary during the
        // cold-start window are included in `summaries` (the caller
        // collects them from `idx.summaries` for open URIs), so we
        // must wipe the shards to avoid duplicates.
        self.summaries.clear();
        self.global_shard.clear();
        self.type_shard.clear();
        self.require_by_return.clear();
        self.type_dependants.clear();
        self.resolution_cache.clear();

        // 1. Insert all summaries first (consuming the owned Vec to avoid
        //    deep-cloning every DocumentSummary) so `resolve_module_to_uri`
        //    fallback path (scanning `self.summaries.keys()`) works
        //    for every file.
        for s in summaries {
            let uri = s.uri.clone();
            self.summaries.insert(uri, s);
        }

        // 2. Build all shards in a single pass.
        for summary in self.summaries.values() {
            let uri = &summary.uri;

            for gc in &summary.global_contributions {
                self.global_shard.push_candidate(&gc.name, GlobalCandidate {
                    name: gc.name.clone(),
                    kind: gc.kind.clone(),
                    type_fact: gc.type_fact.clone(),
                    range: gc.range,
                    selection_range: gc.selection_range,
                    source_uri: uri.clone(),
                });
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

            for type_name in &summary.referenced_type_names {
                self.type_dependants.entry(type_name.clone()).or_default()
                    .insert(uri.clone());
            }
        }

        // 3. Sort candidate lists once (not per-insert like upsert_summary).
        //    Pre-compute URI priority so each URI is evaluated only once
        //    (avoids repeated String allocations inside sort comparisons).
        let uri_priority: HashMap<&Uri, (usize, usize, usize)> = self.summaries.keys()
            .map(|uri| (uri, uri_priority_key(uri)))
            .collect();
        let default_priority = (usize::MAX, usize::MAX, usize::MAX);

        self.global_shard.sort_all(|c| {
            *uri_priority.get(&c.source_uri).unwrap_or(&default_priority)
        });
        for candidates in self.type_shard.values_mut() {
            candidates.sort_by_cached_key(|c| {
                *uri_priority.get(&c.source_uri).unwrap_or(&default_priority)
            });
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

        // `affected` only drives `invalidate_dependants_targeted`,
        // which walks `resolution_cache` looking for stale entries.
        // Skip the collection pass entirely when the cache is empty
        // (true for the entire cold-start scan, since no resolver
        // has run yet) — this avoids an O(require_map) + O(old
        // summary contributions) pass per upsert that compounded
        // the merge-phase O(N²) alongside `remove_contributions`.
        let affected = if self.resolution_cache.is_empty() {
            None
        } else {
            Some(self.collect_affected_names(&uri, &summary))
        };

        self.remove_contributions(&uri);

        for gc in &summary.global_contributions {
            self.global_shard.push_candidate(&gc.name, GlobalCandidate {
                name: gc.name.clone(),
                kind: gc.kind.clone(),
                type_fact: gc.type_fact.clone(),
                range: gc.range,
                selection_range: gc.selection_range,
                source_uri: uri.clone(),
            });
            self.global_shard.sort_at(&gc.name, |c| uri_priority_key(&c.source_uri));
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

        // Use the pre-computed referenced type names from the summary
        // (built during `build_file_analysis`) instead of re-walking every
        // TypeFact on each upsert.
        for type_name in &summary.referenced_type_names {
            self.type_dependants.entry(type_name.clone()).or_default()
                .insert(uri.clone());
        }

        let fingerprint_changed = old_fingerprint != Some(summary.signature_fingerprint);
        if fingerprint_changed {
            if let Some(ref affected) = affected {
                self.invalidate_dependants_targeted(affected);
            }
        }

        self.summaries.insert(uri, summary);
    }

    /// Remove a file from the aggregation layer entirely.
    pub fn remove_file(&mut self, uri: &Uri) {
        self.remove_contributions(uri);
        // Remove this URI from the module_index.
        self.module_index.retain(|_, entries| {
            entries.retain(|(_, u)| u != uri);
            !entries.is_empty()
        });
        self.summaries.remove(uri);
    }

    /// Register a module name → URI mapping in the module index.
    /// The module_name should already be normalized (lowercase, `.`-separated,
    /// init.lua handled).
    pub fn set_require_mapping(&mut self, module_name: String, uri: Uri) {
        use crate::workspace_scanner::module_last_segment;
        let last_seg = module_last_segment(&module_name).to_string();
        let entries = self.module_index.entry(last_seg).or_default();
        // Avoid duplicates: if this exact (module_name, uri) pair exists, skip.
        if !entries.iter().any(|(m, u)| m == &module_name && u == &uri) {
            entries.push((module_name, uri));
        }
    }

    /// Get all registered module names (for completion).
    pub fn all_module_names(&self) -> Vec<String> {
        let mut names: HashSet<String> = HashSet::new();
        for entries in self.module_index.values() {
            for (module_name, _) in entries {
                names.insert(module_name.clone());
            }
        }
        names.into_iter().collect()
    }

    fn remove_contributions(&mut self, uri: &Uri) {
        // Invariant (enforced contract): if `self.summaries` does not
        // contain `uri`, NO shard may contain any candidate /
        // dependant keyed on that URI. This holds because the only
        // shard writers in the whole crate are `upsert_summary` and
        // `remove_contributions` below, and `upsert_summary` always
        // writes to shards *then* inserts into `self.summaries` in
        // the same method with no fallible step between them.
        //
        // If a future refactor introduces a new per-URI shard on
        // `WorkspaceAggregation`, it MUST be pruned here AND its
        // writes MUST happen in the same atomic window under
        // `upsert_summary`'s `index.lock()`, otherwise this
        // short-circuit silently leaves orphan entries behind.
        //
        // The short-circuit turns first-insert upserts from
        // O(total_shard_size) into O(1). On a 20k-file cold start
        // the merge phase was previously O(N²) overall (~269 s out
        // of 295 s total in a recent debug-build run) because each
        // of the four `retain` passes walked the entire growing
        // shard state on every first-time upsert. Re-indexing an
        // already-known file still performs the full prune (needed
        // to drop contributions whose names disappeared between
        // revisions), matching the pre-existing semantics.
        if !self.summaries.contains_key(uri) {
            return;
        }

        self.global_shard.remove_by_uri(uri);

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

        // Prune this URI from every type_dependants bucket (file may
        // be re-indexed with a different set of referenced types).
        self.type_dependants.retain(|_, uris| {
            uris.remove(uri);
            !uris.is_empty()
        });
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
        for entries in self.module_index.values() {
            for (mod_path, target_uri) in entries {
                if target_uri == uri {
                    module_paths.insert(mod_path.clone());
                }
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

    /// Resolve a `require("module.path")` string to a file URI.
    ///
    /// Algorithm:
    /// 1. Normalize the module path (lowercase, strip trailing `.init`).
    /// 2. Apply alias expansion (longest-prefix match).
    /// 3. Look up candidates by last segment (O(1) HashMap lookup).
    /// 4. Among candidates whose last segment matches, pick the one
    ///    with the longest matching suffix (by `.`-separated segments).
    pub fn resolve_module_to_uri(&self, module_path: &str) -> Option<Uri> {
        use crate::workspace_scanner::normalize_require_path;

        let normalized = normalize_require_path(module_path);

        // Step 1: Try alias expansion (longest-prefix match first).
        let resolved = self.apply_alias_expansion(&normalized);

        // Step 2: Look up by last segment.
        self.find_best_match(&resolved)
    }

    /// Apply alias expansion: find the longest matching alias prefix
    /// and replace it. E.g. aliases={"@": "src", "@utils": "src.utils"},
    /// "@utils.foo" → "src.utils.foo" (longest prefix "@utils" wins).
    fn apply_alias_expansion(&self, module_path: &str) -> String {
        if self.require_aliases.is_empty() {
            return module_path.to_string();
        }

        let mut best_alias: Option<(&str, &str)> = None;
        let mut best_len = 0;

        for (alias, replacement) in &self.require_aliases {
            if alias.is_empty() {
                continue;
            }
            if module_path.starts_with(alias.as_str()) && alias.len() > best_len {
                // Ensure the alias matches at a segment boundary:
                // either the alias covers the entire path, or the
                // next char after the alias is `.`
                let rest = &module_path[alias.len()..];
                if rest.is_empty() || rest.starts_with('.') {
                    best_alias = Some((alias.as_str(), replacement.as_str()));
                    best_len = alias.len();
                }
            }
        }

        match best_alias {
            Some((alias, replacement)) => {
                let rest = &module_path[alias.len()..];
                if rest.is_empty() {
                    replacement.to_string()
                } else {
                    // rest starts with '.', so just concatenate
                    format!("{}{}", replacement, rest)
                }
            }
            None => module_path.to_string(),
        }
    }

    /// Find the best matching URI for a normalized module path.
    /// Uses last-segment lookup + strict suffix matching.
    /// A candidate matches if it equals the query or ends with `.{query}`.
    fn find_best_match(&self, module_path: &str) -> Option<Uri> {
        use crate::workspace_scanner::module_last_segment;

        let query_last = module_last_segment(module_path);
        let candidates = self.module_index.get(query_last)?;
        let dot_query = format!(".{}", module_path);

        for (candidate_name, candidate_uri) in candidates {
            if candidate_name == module_path
                || candidate_name.ends_with(dot_query.as_str())
            {
                return Some(candidate_uri.clone());
            }
        }

        None
    }
}

