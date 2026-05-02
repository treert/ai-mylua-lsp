use std::collections::{HashMap, HashSet};
use tower_lsp_server::ls_types::Uri;

use crate::summary::{DocumentSummary, GlobalContributionKind};
use crate::type_system::TypeFact;
use crate::uri_id::UriId;
use crate::util::ByteRange;

/// Workspace-level aggregation of all per-file summaries.
///
/// This is the "bridge" between single-file `DocumentSummary` instances
/// and cross-file queries (goto/hover/references/diagnostics).
/// See `index-architecture.md` §2.2.
#[derive(Debug)]
pub struct WorkspaceAggregation {
    /// All file summaries, keyed by aggregation-local UriId.
    summaries: HashMap<UriId, DocumentSummary>,
    summary_uri_ids: HashMap<Uri, UriId>,
    next_summary_uri_id: i32,
    /// Global name tree — candidate definitions from all files.
    pub global_shard: GlobalShard,
    /// Emmy type name → candidate definitions.
    pub type_shard: HashMap<String, Vec<TypeCandidate>>,

    /// Module index: last_segment → Vec<(full_module_name, UriId)>.
    /// Used by `resolve_module_to_uri` for O(1) last-segment lookup
    /// followed by longest-suffix matching among candidates.
    module_index: HashMap<String, Vec<(String, UriId)>>,
    /// Boundary lookup for `module_index` entries. Keeps existing
    /// `resolve_module_to_uri` callers on LSP-facing `Uri` until the
    /// rest of the aggregation layer migrates to `UriId`.
    module_uris: HashMap<UriId, Uri>,
    module_uri_ids: HashMap<Uri, UriId>,
    next_module_uri_id: i32,
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
    source_uri_id: UriId,
    source_uri: Uri,
}

impl GlobalCandidate {
    pub fn source_uri(&self) -> &Uri {
        &self.source_uri
    }

    fn source_uri_id(&self) -> UriId {
        self.source_uri_id
    }
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
#[derive(Debug)]
pub struct GlobalShard {
    /// Top-level entries: `"print"`, `"UE4"`, `"Foo"`, etc.
    roots: HashMap<String, GlobalNode>,
    /// Reverse index: UriId → full-path strings contributed by that URI.
    /// Maintained by `push_candidate` / `remove_by_uri` / `clear`.
    uri_to_paths: HashMap<UriId, Vec<String>>,
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
            .entry(candidate.source_uri_id())
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
    pub fn remove_by_uri(&mut self, uri: &Uri, uri_id: UriId) {
        let Some(paths) = self.uri_to_paths.remove(&uri_id) else { return };
        for path in &paths {
            if let Some(node) = self.get_node_mut(path) {
                node.candidates.retain(|c| c.source_uri_id() != uri_id && c.source_uri() != uri);
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

impl Default for GlobalShard {
    fn default() -> Self {
        Self {
            roots: HashMap::new(),
            uri_to_paths: HashMap::new(),
        }
    }
}

/// A single candidate definition for an Emmy type name.
#[derive(Debug, Clone)]
pub struct TypeCandidate {
    pub name: String,
    pub kind: crate::summary::TypeDefinitionKind,
    source_uri_id: UriId,
    source_uri: Uri,
    pub range: ByteRange,
}

impl TypeCandidate {
    pub fn source_uri(&self) -> &Uri {
        &self.source_uri
    }

    fn source_uri_id(&self) -> UriId {
        self.source_uri_id
    }
}

/// Priority key for sorting candidates (smaller = higher priority):
/// 1. More occurrences of "annotation" anywhere in the path = higher priority
/// 2. Shallower paths (fewer `/` segments) win
/// 3. Shorter total path length as tiebreaker
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
            summary_uri_ids: HashMap::new(),
            next_summary_uri_id: 1,
            global_shard: GlobalShard::new(),
            type_shard: HashMap::new(),
            module_index: HashMap::new(),
            module_uris: HashMap::new(),
            module_uri_ids: HashMap::new(),
            next_module_uri_id: 1,
            require_aliases: HashMap::new(),
        }
    }

    pub fn summary(&self, uri: &Uri) -> Option<&DocumentSummary> {
        let uri_id = self.summary_uri_ids.get(uri)?;
        self.summaries.get(uri_id)
    }

    pub fn summaries_iter(&self) -> impl Iterator<Item = (&Uri, &DocumentSummary)> {
        self.summaries.values().map(|summary| (&summary.uri, summary))
    }

    pub fn summaries_values(&self) -> impl Iterator<Item = &DocumentSummary> {
        self.summaries.values()
    }

    pub fn summary_count(&self) -> usize {
        self.summaries.len()
    }

    fn summary_uri_id(&mut self, uri: &Uri) -> UriId {
        if let Some(id) = self.summary_uri_ids.get(uri).copied() {
            return id;
        }

        let raw = self.next_summary_uri_id;
        let id = UriId::new(raw);
        self.next_summary_uri_id = raw.checked_add(1).expect("summary UriId exhausted");
        self.summary_uri_ids.insert(uri.clone(), id);
        id
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
        // collects them via `idx.summary(...)` for open URIs), so we
        // must wipe the shards to avoid duplicates.
        self.summaries.clear();
        self.summary_uri_ids.clear();
        self.next_summary_uri_id = 1;
        self.global_shard.clear();
        self.type_shard.clear();

        // 1. Insert all summaries first (consuming the owned Vec to avoid
        //    deep-cloning every DocumentSummary) so later shard builds can
        //    see every file's summary in a single consistent snapshot.
        for s in summaries {
            let uri = s.uri.clone();
            let uri_id = self.summary_uri_id(&uri);
            self.summaries.insert(uri_id, s);
        }

        {
            let summaries = &self.summaries;
            let global_shard = &mut self.global_shard;
            let type_shard = &mut self.type_shard;

            // 2. Build all shards in a single pass.
            for (uri_id, summary) in summaries {
                let uri = &summary.uri;

                for gc in &summary.global_contributions {
                    global_shard.push_candidate(&gc.name, GlobalCandidate {
                        name: gc.name.clone(),
                        kind: gc.kind.clone(),
                        type_fact: gc.type_fact.clone(),
                        range: gc.range,
                        selection_range: gc.selection_range,
                        source_uri_id: *uri_id,
                        source_uri: uri.clone(),
                    });
                }

                for td in &summary.type_definitions {
                    let candidates = type_shard
                        .entry(td.name.clone())
                        .or_default();
                    candidates.push(TypeCandidate {
                        name: td.name.clone(),
                        kind: td.kind.clone(),
                        source_uri_id: *uri_id,
                        source_uri: uri.clone(),
                        range: td.range,
                    });
                }
            }
        }

        // 3. Sort candidate lists once (not per-insert like upsert_summary).
        //    Pre-compute URI priority so each URI is evaluated only once
        //    (avoids repeated String allocations inside sort comparisons).
        let uri_priority: HashMap<&Uri, (usize, usize, usize)> = self.summaries.values()
            .map(|summary| (&summary.uri, uri_priority_key(&summary.uri)))
            .collect();
        let default_priority = (usize::MAX, usize::MAX, usize::MAX);

        self.global_shard.sort_all(|c| {
            *uri_priority.get(c.source_uri()).unwrap_or(&default_priority)
        });
        for candidates in self.type_shard.values_mut() {
            candidates.sort_by_cached_key(|c| {
                *uri_priority.get(c.source_uri()).unwrap_or(&default_priority)
            });
        }
    }

    /// Integrate a new or updated file summary into the aggregation layer.
    ///
    /// Performs a name-level diff: removes old contributions from this URI
    /// and inserts new ones.
    pub fn upsert_summary(&mut self, summary: DocumentSummary) {
        let uri = summary.uri.clone();

        self.remove_contributions(&uri);
        let uri_id = self.summary_uri_id(&uri);

        for gc in &summary.global_contributions {
            self.global_shard.push_candidate(&gc.name, GlobalCandidate {
                name: gc.name.clone(),
                kind: gc.kind.clone(),
                type_fact: gc.type_fact.clone(),
                range: gc.range,
                selection_range: gc.selection_range,
                source_uri_id: uri_id,
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
                source_uri_id: uri_id,
                source_uri: uri.clone(),
                range: td.range,
            });
            candidates.sort_by_cached_key(|c| uri_priority_key(&c.source_uri));
        }

        self.summaries.insert(uri_id, summary);
    }

    /// Remove a file from the aggregation layer entirely.
    pub fn remove_file(&mut self, uri: &Uri) {
        self.remove_contributions(uri);
        // Remove this URI from the module_index.
        let removed_ids: HashSet<UriId> = self.module_uris
            .iter()
            .filter_map(|(id, u)| if u == uri { Some(*id) } else { None })
            .collect();
        self.module_index.retain(|_, entries| {
            entries.retain(|(_, id)| !removed_ids.contains(id));
            !entries.is_empty()
        });
        for id in removed_ids {
            if let Some(removed_uri) = self.module_uris.remove(&id) {
                self.module_uri_ids.remove(&removed_uri);
            }
        }
        if let Some(uri_id) = self.summary_uri_ids.remove(uri) {
            self.summaries.remove(&uri_id);
        }
    }

    /// Register a module name → URI mapping in the module index.
    /// The module_name should already be normalized (lowercase, `.`-separated,
    /// init.lua handled).
    pub fn set_require_mapping(&mut self, module_name: String, uri: Uri) {
        use crate::workspace_scanner::module_last_segment;
        let last_seg = module_last_segment(&module_name).to_string();
        let uri_id = self.module_uri_id(&uri);
        let entries = self.module_index.entry(last_seg).or_default();
        // Avoid duplicates: if this exact (module_name, uri) pair exists, skip.
        if !entries.iter().any(|(m, id)| m == &module_name && *id == uri_id) {
            entries.push((module_name, uri_id));
        }
    }

    fn module_uri_id(&mut self, uri: &Uri) -> UriId {
        if let Some(id) = self.module_uri_ids.get(uri).copied() {
            return id;
        }

        let raw = self.next_module_uri_id;
        let id = UriId::new(raw);
        self.next_module_uri_id = raw.checked_add(1).expect("module UriId exhausted");
        self.module_uri_ids.insert(uri.clone(), id);
        self.module_uris.insert(id, uri.clone());
        id
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
        let Some(uri_id) = self.summary_uri_ids.get(uri).copied() else {
            return;
        };

        self.global_shard.remove_by_uri(uri, uri_id);

        self.type_shard.retain(|_, candidates| {
            candidates.retain(|c| c.source_uri_id() != uri_id && c.source_uri() != uri);
            !candidates.is_empty()
        });
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

        for (candidate_name, candidate_uri_id) in candidates {
            if candidate_name == module_path
                || candidate_name.ends_with(dot_query.as_str())
            {
                return self.module_uris.get(candidate_uri_id).cloned();
            }
        }

        None
    }
}
