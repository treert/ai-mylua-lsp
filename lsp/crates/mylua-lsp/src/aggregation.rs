use std::collections::{HashMap, HashSet};

use crate::lua_symbol::{get_lua_symbol, intern_lua_symbol, LuaSymbol};
use crate::summary::{DocumentSummary, GlobalContributionKind};
use crate::type_system::TypeFact;
use crate::uri_id::{priority_uri as uri_priority, UriId, UriPriority};
use crate::util::ByteRange;

/// Workspace-level aggregation of all per-file summaries.
///
/// This is the "bridge" between single-file `DocumentSummary` instances
/// and cross-file queries (goto/hover/references/diagnostics).
/// See `index-architecture.md` §2.2.
#[derive(Debug)]
pub struct WorkspaceAggregation {
    /// All file summaries, keyed by the server session-local UriId.
    summaries: HashMap<UriId, DocumentSummary>,
    /// Global name tree — candidate definitions from all files.
    pub global_shard: GlobalShard,
    /// Emmy type name → candidate definitions.
    pub type_shard: HashMap<LuaSymbol, Vec<TypeCandidate>>,

    /// Module index: last_segment → Vec<(full_module_name, UriId)>.
    /// Used by `resolve_module_to_id` for O(1) last-segment lookup
    /// followed by longest-suffix matching among candidates.
    module_index: HashMap<LuaSymbol, Vec<(LuaSymbol, UriId)>>,
    /// Require path aliases (e.g. `{"@": "src/"}`), applied during module resolution.
    /// The longest matching prefix alias is chosen.
    pub require_aliases: HashMap<LuaSymbol, LuaSymbol>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WorkspaceAggregationStats {
    pub summary_count: usize,
    pub global_contribution_count: usize,
    pub function_summary_count: usize,
    pub function_name_index_count: usize,
    pub type_definition_count: usize,
    pub type_field_count: usize,
    pub table_shape_count: usize,
    pub table_field_count: usize,
    pub call_site_count: usize,
    pub global_root_count: usize,
    pub global_node_count: usize,
    pub global_candidate_count: usize,
    pub global_reverse_path_count: usize,
    pub type_name_count: usize,
    pub type_candidate_count: usize,
    pub module_last_segment_count: usize,
    pub module_entry_count: usize,
    pub require_alias_count: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct GlobalShardStats {
    root_count: usize,
    node_count: usize,
    candidate_count: usize,
    reverse_path_count: usize,
}

/// A single candidate definition for a global name.
#[derive(Debug, Clone)]
pub struct GlobalCandidate {
    pub name: LuaSymbol,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: ByteRange,
    pub selection_range: ByteRange,
    source_uri_id: UriId,
}

impl GlobalCandidate {
    pub(crate) fn source_uri_id(&self) -> UriId {
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
    pub children: HashMap<LuaSymbol, GlobalNode>,
}

/// Tree-structured global shard, replacing the flat
/// `HashMap<String, Vec<GlobalCandidate>>`.
///
/// Provides O(depth) exact lookup, O(children) direct-child enumeration,
/// and O(contributions-per-file) URI-based removal via a reverse index.
#[derive(Debug)]
pub struct GlobalShard {
    /// Top-level entries: `"print"`, `"UE4"`, `"Foo"`, etc.
    roots: HashMap<LuaSymbol, GlobalNode>,
    /// Reverse index: UriId → full-path strings contributed by that URI.
    /// Maintained by `push_candidate` / `remove_by_uri` / `clear`.
    uri_to_paths: HashMap<UriId, Vec<LuaSymbol>>,
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
        let next = path[pos..]
            .find(|c: char| c == '.' || c == ':')
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
    fn collect_entries<'a>(
        &'a self,
        prefix: &str,
        out: &mut Vec<(String, &'a Vec<GlobalCandidate>)>,
    ) {
        if !self.candidates.is_empty() {
            // Use the candidate's own `name` field to preserve the
            // original separator (`:` vs `.`). The trie merges both
            // separator types into a single `children` map, so
            // reconstructing from the tree would always produce `.`.
            let key = self.candidates[0].name.to_string();
            out.push((key, &self.candidates));
        }
        for (seg, child) in &self.children {
            let child_path = format!("{}.{}", prefix, seg.as_str());
            child.collect_entries(&child_path, out);
        }
    }

    fn stats(&self) -> (usize, usize) {
        let mut node_count = 1;
        let mut candidate_count = self.candidates.len();
        for child in self.children.values() {
            let (child_nodes, child_candidates) = child.stats();
            node_count += child_nodes;
            candidate_count += child_candidates;
        }
        (node_count, candidate_count)
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
        let root = get_lua_symbol(root)?;
        let mut node = self.roots.get(&root)?;
        for seg in segments {
            let seg = get_lua_symbol(seg)?;
            node = node.children.get(&seg)?;
        }
        Some(node)
    }

    /// Navigate to a node at the given path (mutable).
    fn get_node_mut(&mut self, path: &str) -> Option<&mut GlobalNode> {
        let (root, segments) = split_global_path(path);
        let root = get_lua_symbol(root)?;
        let mut node = self.roots.get_mut(&root)?;
        for seg in segments {
            let seg = get_lua_symbol(seg)?;
            node = node.children.get_mut(&seg)?;
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
            .push(intern_lua_symbol(path));

        // Walk/create nodes.
        let (root, segments) = split_global_path(path);
        let mut node = self
            .roots
            .entry(intern_lua_symbol(root))
            .or_insert_with(GlobalNode::new);
        for seg in segments {
            node = node
                .children
                .entry(intern_lua_symbol(seg))
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
    pub fn remove_by_uri(&mut self, uri_id: UriId) {
        let Some(paths) = self.uri_to_paths.remove(&uri_id) else {
            return;
        };
        for path in &paths {
            if let Some(node) = self.get_node_mut(path) {
                node.candidates.retain(|c| c.source_uri_id() != uri_id);
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
            root_node.collect_entries(root_name.as_str(), &mut out);
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
            if root_name.as_str().starts_with(prefix) {
                root_node.collect_entries(root_name.as_str(), &mut out);
            }
        }
        out
    }

    fn stats(&self) -> GlobalShardStats {
        let mut stats = GlobalShardStats {
            root_count: self.roots.len(),
            reverse_path_count: self.uri_to_paths.values().map(Vec::len).sum(),
            ..Default::default()
        };
        for root in self.roots.values() {
            let (nodes, candidates) = root.stats();
            stats.node_count += nodes;
            stats.candidate_count += candidates;
        }
        stats
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
    pub name: LuaSymbol,
    pub kind: crate::summary::TypeDefinitionKind,
    source_uri_id: UriId,
    pub range: ByteRange,
    /// Range of just the type name token (e.g. `Foo` within
    /// `---@class Foo`). Falls back to `range` when absent. Used by
    /// `find_references` so a type-name declaration highlights the name
    /// itself rather than the whole anchor statement (which would
    /// collide with the matching `global_shard` entry on the same line).
    pub name_range: Option<ByteRange>,
}

impl TypeCandidate {
    pub(crate) fn source_uri_id(&self) -> UriId {
        self.source_uri_id
    }
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
            module_index: HashMap::new(),
            require_aliases: HashMap::new(),
        }
    }

    pub fn summary_by_id(&self, uri_id: UriId) -> Option<&DocumentSummary> {
        self.summaries.get(&uri_id)
    }

    pub fn summaries_iter_id(&self) -> impl Iterator<Item = (UriId, &DocumentSummary)> {
        self.summaries
            .iter()
            .map(|(uri_id, summary)| (*uri_id, summary))
    }

    pub fn summaries_values(&self) -> impl Iterator<Item = &DocumentSummary> {
        self.summaries.values()
    }

    pub fn summary_count(&self) -> usize {
        self.summaries.len()
    }

    pub fn stats(&self) -> WorkspaceAggregationStats {
        let global_stats = self.global_shard.stats();
        let mut stats = WorkspaceAggregationStats {
            summary_count: self.summaries.len(),
            global_root_count: global_stats.root_count,
            global_node_count: global_stats.node_count,
            global_candidate_count: global_stats.candidate_count,
            global_reverse_path_count: global_stats.reverse_path_count,
            type_name_count: self.type_shard.len(),
            type_candidate_count: self.type_shard.values().map(Vec::len).sum(),
            module_last_segment_count: self.module_index.len(),
            module_entry_count: self.module_index.values().map(Vec::len).sum(),
            require_alias_count: self.require_aliases.len(),
            ..Default::default()
        };

        for summary in self.summaries.values() {
            stats.global_contribution_count += summary.global_contributions.len();
            stats.function_summary_count += summary.function_summaries.len();
            stats.function_name_index_count += summary.function_name_index.len();
            stats.type_definition_count += summary.type_definitions.len();
            stats.type_field_count += summary
                .type_definitions
                .iter()
                .map(|definition| definition.fields.len())
                .sum::<usize>();
            stats.table_shape_count += summary.table_shapes.len();
            stats.table_field_count += summary
                .table_shapes
                .values()
                .map(|shape| shape.fields.len())
                .sum::<usize>();
            stats.call_site_count += summary.call_sites.len();
        }

        stats
    }

    pub fn type_candidates(&self, name: &str) -> Option<&Vec<TypeCandidate>> {
        let name = get_lua_symbol(name)?;
        self.type_shard.get(&name)
    }

    pub fn contains_type(&self, name: &str) -> bool {
        self.type_candidates(name).is_some()
    }

    /// Build the initial global index atomically from a complete set of
    /// file summaries. This is the cold-start path: all summaries are
    /// available at once, so we skip `remove_contributions` (nothing to
    /// remove) and `resolve_module_to_id` benefits from a fully
    /// populated `require_map` + `summaries` — eliminating the
    /// batch-ordering bug where early files couldn't resolve modules
    /// defined in later batches.
    ///
    /// After this call, `upsert_summary` handles incremental updates.
    pub fn build_initial(&mut self, summaries: Vec<(UriId, DocumentSummary)>) {
        // Clear all shards — build_initial is a full rebuild. Any
        // contributions from did_open's upsert_summary during the
        // cold-start window are included in `summaries` (the caller
        // collects them via `idx.summary_by_id(...)` for open UriIds), so we
        // must wipe the shards to avoid duplicates.
        self.summaries.clear();
        self.global_shard.clear();
        self.type_shard.clear();

        // 1. Insert all summaries first (consuming the owned Vec to avoid
        //    deep-cloning every DocumentSummary) so later shard builds can
        //    see every file's summary in a single consistent snapshot.
        for (uri_id, summary) in summaries {
            self.summaries.insert(uri_id, summary);
        }

        {
            let summaries = &self.summaries;
            let global_shard = &mut self.global_shard;
            let type_shard = &mut self.type_shard;

            // 2. Build all shards in a single pass.
            for (uri_id, summary) in summaries {
                for gc in &summary.global_contributions {
                    global_shard.push_candidate(
                        gc.name.as_str(),
                        GlobalCandidate {
                            name: gc.name,
                            kind: gc.kind.clone(),
                            type_fact: gc.type_fact.clone(),
                            range: gc.range,
                            selection_range: gc.selection_range,
                            source_uri_id: *uri_id,
                        },
                    );
                }

                for td in &summary.type_definitions {
                    let candidates = type_shard.entry(td.name).or_default();
                    candidates.push(TypeCandidate {
                        name: td.name,
                        kind: td.kind.clone(),
                        source_uri_id: *uri_id,
                        range: td.range,
                        name_range: td.name_range,
                    });
                }
            }
        }

        // 3. Sort candidate lists once (not per-insert like upsert_summary).
        //    Pre-compute URI priority so each URI is evaluated only once
        //    (avoids repeated String allocations inside sort comparisons).
        let id_priority: HashMap<UriId, UriPriority> = self
            .summaries
            .iter()
            .map(|(id, _)| (*id, uri_priority(*id)))
            .collect();
        let default_priority = UriPriority::worst();

        self.global_shard.sort_all(|c| {
            *id_priority
                .get(&c.source_uri_id())
                .unwrap_or(&default_priority)
        });
        for candidates in self.type_shard.values_mut() {
            candidates.sort_by_cached_key(|c| {
                *id_priority
                    .get(&c.source_uri_id())
                    .unwrap_or(&default_priority)
            });
        }
    }

    /// Integrate a new or updated file summary into the aggregation layer.
    ///
    /// Performs a name-level diff: removes old contributions from this URI
    /// and inserts new ones.
    pub fn upsert_summary(&mut self, uri_id: UriId, summary: DocumentSummary) {
        self.remove_contributions(uri_id);
        let summary_priorities: HashMap<UriId, UriPriority> = self
            .summaries
            .iter()
            .map(|(id, _)| (*id, uri_priority(*id)))
            .collect();
        let current_priority = uri_priority(uri_id);

        for gc in &summary.global_contributions {
            self.global_shard.push_candidate(
                gc.name.as_str(),
                GlobalCandidate {
                    name: gc.name,
                    kind: gc.kind.clone(),
                    type_fact: gc.type_fact.clone(),
                    range: gc.range,
                    selection_range: gc.selection_range,
                    source_uri_id: uri_id,
                },
            );
            self.global_shard.sort_at(gc.name.as_str(), |c| {
                summary_priorities
                    .get(&c.source_uri_id())
                    .copied()
                    .unwrap_or(current_priority)
            });
        }

        for td in &summary.type_definitions {
            let candidates = self.type_shard.entry(td.name).or_default();
            candidates.push(TypeCandidate {
                name: td.name,
                kind: td.kind.clone(),
                source_uri_id: uri_id,
                range: td.range,
                name_range: td.name_range,
            });
            candidates.sort_by_cached_key(|c| {
                summary_priorities
                    .get(&c.source_uri_id())
                    .copied()
                    .unwrap_or(current_priority)
            });
        }

        self.summaries.insert(uri_id, summary);
    }

    /// Remove a file from the aggregation layer entirely.
    pub fn remove_file(&mut self, uri_id: UriId) {
        self.remove_contributions(uri_id);
        // Remove this URI from the module_index.
        self.module_index.retain(|_, entries| {
            entries.retain(|(_, id)| *id != uri_id);
            !entries.is_empty()
        });
        self.summaries.remove(&uri_id);
    }

    /// Register a module name → URI mapping in the module index.
    /// The module_name should already be normalized (lowercase, `.`-separated,
    /// init.lua handled).
    pub fn set_require_mapping(&mut self, module_name: String, uri_id: UriId) -> bool {
        use crate::workspace_scanner::module_last_segment;
        let last_seg = intern_lua_symbol(module_last_segment(&module_name));
        let module_name = intern_lua_symbol(&module_name);
        let entries = self.module_index.entry(last_seg).or_default();
        // Avoid duplicates: if this exact (module_name, uri_id) pair exists, skip.
        if entries
            .iter()
            .any(|(m, id)| *m == module_name && *id == uri_id)
        {
            return false;
        }
        entries.push((module_name, uri_id));
        true
    }

    /// Get all registered module names (for completion).
    pub fn all_module_names(&self) -> Vec<String> {
        let mut names: HashSet<LuaSymbol> = HashSet::new();
        for entries in self.module_index.values() {
            for (module_name, _) in entries {
                names.insert(*module_name);
            }
        }
        names.into_iter().map(|name| name.to_string()).collect()
    }

    fn remove_contributions(&mut self, uri_id: UriId) {
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
        if !self.summaries.contains_key(&uri_id) {
            return;
        }

        self.global_shard.remove_by_uri(uri_id);

        self.type_shard.retain(|_, candidates| {
            candidates.retain(|c| c.source_uri_id() != uri_id);
            !candidates.is_empty()
        });
    }

    /// Resolve a `require("module.path")` string to a file id.
    ///
    /// Algorithm:
    /// 1. Normalize the module path (lowercase, strip trailing `.init`).
    /// 2. Apply alias expansion (longest-prefix match).
    /// 3. Look up candidates by last segment (O(1) HashMap lookup).
    /// 4. Among candidates whose last segment matches, pick the one
    ///    with the longest matching suffix (by `.`-separated segments).
    pub fn resolve_module_to_id(&self, module_path: &str) -> Option<UriId> {
        use crate::workspace_scanner::normalize_require_path;

        let normalized = normalize_require_path(module_path);
        let resolved = self.apply_alias_expansion(&normalized);
        self.find_best_match_id(&resolved)
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
            if alias.as_str().is_empty() {
                continue;
            }
            if module_path.starts_with(alias.as_str()) && alias.as_str().len() > best_len {
                // Ensure the alias matches at a segment boundary:
                // either the alias covers the entire path, or the
                // next char after the alias is `.`
                let rest = &module_path[alias.as_str().len()..];
                if rest.is_empty() || rest.starts_with('.') {
                    best_alias = Some((alias.as_str(), replacement.as_str()));
                    best_len = alias.as_str().len();
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

    /// Find the best matching URI id for a normalized module path.
    /// Uses last-segment lookup + strict suffix matching.
    /// A candidate matches if it equals the query or ends with `.{query}`.
    fn find_best_match_id(&self, module_path: &str) -> Option<UriId> {
        use crate::workspace_scanner::module_last_segment;

        let query_last = module_last_segment(module_path);
        let query_last = get_lua_symbol(query_last)?;
        let candidates = self.module_index.get(&query_last)?;
        let dot_query = format!(".{}", module_path);

        for (candidate_name, candidate_uri_id) in candidates {
            if candidate_name.as_str() == module_path
                || candidate_name.as_str().ends_with(dot_query.as_str())
            {
                return Some(*candidate_uri_id);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lua_symbol::{intern_lua_symbol, LuaSymbol};
    use crate::summary::TypeDefinitionKind;
    use crate::uri_id::intern_uri;

    fn byte_range() -> ByteRange {
        ByteRange {
            start_byte: 0,
            end_byte: 1,
            start_row: 0,
            start_col: 0,
            end_row: 0,
            end_col: 1,
        }
    }

    fn assert_symbol(_: LuaSymbol) {}

    fn make_summary_with_global(
        uri: tower_lsp_server::ls_types::Uri,
        global_names: &[&str],
    ) -> DocumentSummary {
        use crate::summary::GlobalContribution;
        let contributions: Vec<GlobalContribution> = global_names
            .iter()
            .map(|name| GlobalContribution {
                name: intern_lua_symbol(name),
                kind: GlobalContributionKind::Variable,
                type_fact: TypeFact::Unknown,
                range: byte_range(),
                selection_range: byte_range(),
            })
            .collect();
        DocumentSummary {
            uri,
            global_contributions: contributions,
            function_summaries: HashMap::new(),
            function_name_index: HashMap::new(),
            type_definitions: vec![],
            table_shapes: HashMap::new(),
            module_return_type: None,
            module_return_range: None,
            signature_fingerprint: 0,
            call_sites: vec![],
            is_meta: false,
            meta_name: None,
        }
    }

    #[test]
    fn annotation_path_ranks_first_after_build_initial() {
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();
        let n2_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal2.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);
        let n2_id = intern_uri(&n2_uri);

        let mut agg = WorkspaceAggregation::new();
        // Insert in arbitrary order — sort must be independent of insert order.
        agg.build_initial(vec![
            (n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"])),
            (ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"])),
            (n2_id, make_summary_with_global(n2_uri.clone(), &["Foo"])),
        ]);

        let candidates = agg.global_shard.get("Foo").expect("Foo candidates");
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation candidate should be first after build_initial"
        );
    }

    #[test]
    fn annotation_path_ranks_first_after_upsert_normal() {
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();
        let n2_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal2.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);
        let n2_id = intern_uri(&n2_uri);

        let mut agg = WorkspaceAggregation::new();
        agg.build_initial(vec![
            (n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"])),
            (ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"])),
            (n2_id, make_summary_with_global(n2_uri.clone(), &["Foo"])),
        ]);

        // Simulate editing a normal file.
        agg.upsert_summary(n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"]));
        let candidates = agg.global_shard.get("Foo").expect("Foo candidates");
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation candidate should be first after upserting normal file"
        );
    }

    #[test]
    fn annotation_path_ranks_first_after_upsert_annotation() {
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();
        let n2_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal2.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);
        let n2_id = intern_uri(&n2_uri);

        let mut agg = WorkspaceAggregation::new();
        agg.build_initial(vec![
            (n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"])),
            (ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"])),
            (n2_id, make_summary_with_global(n2_uri.clone(), &["Foo"])),
        ]);

        // Simulate editing the annotation file itself.
        agg.upsert_summary(ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"]));
        let candidates = agg.global_shard.get("Foo").expect("Foo candidates");
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation candidate should be first after upserting annotation file"
        );
    }

    #[test]
    fn annotation_path_ranks_first_after_upsert_new_file() {
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();
        let n3_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal3.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);
        let n3_id = intern_uri(&n3_uri);

        let mut agg = WorkspaceAggregation::new();
        agg.build_initial(vec![
            (n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"])),
            (ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"])),
        ]);

        // Simulate a brand-new file (first upsert, not in summaries yet).
        agg.upsert_summary(n3_id, make_summary_with_global(n3_uri.clone(), &["Foo"]));
        let candidates = agg.global_shard.get("Foo").expect("Foo candidates");
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation candidate should be first after upserting new file"
        );
    }

    #[test]
    fn annotation_path_ranks_first_during_cold_start_upserts() {
        // Simulate the cold-start window: files upserted one-by-one
        // (no build_initial). Each upsert must keep annotation first.
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();
        let n2_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal2.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);
        let n2_id = intern_uri(&n2_uri);

        let mut agg = WorkspaceAggregation::new();
        // Insert normal first, then annotation, then another normal.
        agg.upsert_summary(n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"]));
        agg.upsert_summary(ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"]));
        agg.upsert_summary(n2_id, make_summary_with_global(n2_uri.clone(), &["Foo"]));

        let candidates = agg.global_shard.get("Foo").expect("Foo candidates");
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation should be first during cold-start upserts"
        );
    }

    #[test]
    fn annotation_path_ranks_first_in_type_shard() {
        use crate::summary::{TypeDefinition, TypeDefinitionKind};

        fn make_summary_with_type(
            uri: tower_lsp_server::ls_types::Uri,
            type_name: &str,
        ) -> DocumentSummary {
            DocumentSummary {
                uri,
                global_contributions: vec![],
                function_summaries: HashMap::new(),
                function_name_index: HashMap::new(),
                type_definitions: vec![TypeDefinition {
                    name: intern_lua_symbol(type_name),
                    kind: TypeDefinitionKind::Class,
                    parents: vec![],
                    fields: vec![],
                    alias_type: None,
                    generic_params: vec![],
                    range: byte_range(),
                    name_range: None,
                    anchor_shape_id: None,
                }],
                table_shapes: HashMap::new(),
                module_return_type: None,
                module_return_range: None,
                signature_fingerprint: 0,
                call_sites: vec![],
                is_meta: false,
                meta_name: None,
            }
        }

        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);

        let mut agg = WorkspaceAggregation::new();
        agg.build_initial(vec![
            (n1_id, make_summary_with_type(n1_uri.clone(), "MyType")),
            (ann_id, make_summary_with_type(ann_uri.clone(), "MyType")),
        ]);

        let candidates = agg
            .type_candidates("MyType")
            .expect("MyType candidates");
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation type candidate should be first"
        );

        // Upsert normal file — annotation should stay first.
        agg.upsert_summary(n1_id, make_summary_with_type(n1_uri.clone(), "MyType"));
        let candidates = agg.type_candidates("MyType").expect("MyType candidates");
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation type candidate should stay first after upsert normal"
        );
    }

    #[test]
    fn ue4_annotation_ranks_first_with_real_paths() {
        // Reproduce the user's exact scenario: UE4 defined in 5 files,
        // one under UEAnnotation/, four under Content/.
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///D%3A/NCDevelop/NC_Shell/UEAnnotation/LuaComment/UE4.lua"
                .parse()
                .unwrap();
        let l1_uri: tower_lsp_server::ls_types::Uri =
            "file:///D%3A/NCDevelop/NC_Shell/Content/LetsGo/Script/StartUp/UnLua.lua"
                .parse()
                .unwrap();
        let l2_uri: tower_lsp_server::ls_types::Uri =
            "file:///D%3A/NCDevelop/NC_Shell/Content/LetsGo/Script/UnLua.lua"
                .parse()
                .unwrap();
        let ls1_uri: tower_lsp_server::ls_types::Uri =
            "file:///D%3A/NCDevelop/NC_Shell/Content/LetsGoSDK/Script/Boot/UnLua.lua"
                .parse()
                .unwrap();
        let ls2_uri: tower_lsp_server::ls_types::Uri =
            "file:///D%3A/NCDevelop/NC_Shell/Content/LetsGoSDK/Script/StartUp/UnLua.lua"
                .parse()
                .unwrap();

        let ann_id = intern_uri(&ann_uri);
        let l1_id = intern_uri(&l1_uri);
        let l2_id = intern_uri(&l2_uri);
        let ls1_id = intern_uri(&ls1_uri);
        let ls2_id = intern_uri(&ls2_uri);

        let mut agg = WorkspaceAggregation::new();
        // Insert in the order a workspace scan might produce (Content first).
        agg.build_initial(vec![
            (l1_id, make_summary_with_global(l1_uri.clone(), &["UE4"])),
            (l2_id, make_summary_with_global(l2_uri.clone(), &["UE4"])),
            (ls1_id, make_summary_with_global(ls1_uri.clone(), &["UE4"])),
            (ls2_id, make_summary_with_global(ls2_uri.clone(), &["UE4"])),
            (ann_id, make_summary_with_global(ann_uri.clone(), &["UE4"])),
        ]);

        let candidates = agg.global_shard.get("UE4").expect("UE4 candidates");
        assert_eq!(candidates.len(), 5);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "UEAnnotation should be first after build_initial"
        );

        // Now simulate editing one of the Content files (upsert).
        agg.upsert_summary(l2_id, make_summary_with_global(l2_uri.clone(), &["UE4"]));
        let candidates = agg.global_shard.get("UE4").expect("UE4 candidates");
        assert_eq!(candidates.len(), 5);
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "UEAnnotation should still be first after upserting Content file"
        );
    }

    #[test]
    fn build_initial_after_cold_start_upsert_deduplicates_and_sorts() {
        // Cold-start window: file upserted via did_open before the
        // workspace scan's build_initial runs. build_initial must clear
        // shards (no duplicate candidates) and still sort correctly.
        let ann_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/annotation/test.lua".parse().unwrap();
        let n1_uri: tower_lsp_server::ls_types::Uri =
            "file:///proj/normal1.lua".parse().unwrap();

        let ann_id = intern_uri(&ann_uri);
        let n1_id = intern_uri(&n1_uri);

        let mut agg = WorkspaceAggregation::new();
        // Simulate did_open during cold-start: upsert before build_initial.
        agg.upsert_summary(n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"]));
        // Workspace scan completes — build_initial includes the upserted file.
        agg.build_initial(vec![
            (n1_id, make_summary_with_global(n1_uri.clone(), &["Foo"])),
            (ann_id, make_summary_with_global(ann_uri.clone(), &["Foo"])),
        ]);

        let candidates = agg.global_shard.get("Foo").expect("Foo candidates");
        assert_eq!(
            candidates.len(),
            2,
            "no duplicate candidates after build_initial"
        );
        assert_eq!(
            candidates[0].source_uri_id(),
            ann_id,
            "annotation should be first after build_initial following cold-start upsert"
        );
    }

    #[test]
    fn long_lived_aggregation_names_use_symbols_but_queries_stay_string_facing() {
        let uri = "file:///aggregation_symbols.lua".parse().unwrap();
        let uri_id = intern_uri(&uri);
        let candidate = GlobalCandidate {
            name: intern_lua_symbol("Game.Player.new"),
            kind: GlobalContributionKind::Function,
            type_fact: TypeFact::Unknown,
            range: byte_range(),
            selection_range: byte_range(),
            source_uri_id: uri_id,
        };
        assert_symbol(candidate.name);

        let mut shard = GlobalShard::new();
        shard.push_candidate("Game.Player.new", candidate);

        assert!(shard.contains_key("Game.Player.new"));
        assert_eq!(shard.iter_all_entries()[0].0, "Game.Player.new");
        assert_symbol(*shard.uri_to_paths.get(&uri_id).unwrap().first().unwrap());

        let type_candidate = TypeCandidate {
            name: intern_lua_symbol("Player"),
            kind: TypeDefinitionKind::Class,
            source_uri_id: uri_id,
            range: byte_range(),
            name_range: None,
        };
        assert_symbol(type_candidate.name);

        let mut agg = WorkspaceAggregation::new();
        agg.type_shard
            .insert(intern_lua_symbol("Player"), vec![type_candidate]);
        agg.require_aliases
            .insert(intern_lua_symbol("@game"), intern_lua_symbol("src.game"));
        agg.set_require_mapping("src.game.player".to_string(), uri_id);

        assert!(agg.type_shard.contains_key(&intern_lua_symbol("Player")));
        assert_eq!(agg.all_module_names(), vec!["src.game.player".to_string()]);
        assert_eq!(agg.resolve_module_to_id("@game.player"), Some(uri_id));
    }

    #[test]
    fn reports_aggregation_stats() {
        let uri = "file:///aggregation_stats.lua".parse().unwrap();
        let uri_id = intern_uri(&uri);
        let mut agg = WorkspaceAggregation::new();
        agg.set_require_mapping("src.game.player".to_string(), uri_id);
        agg.type_shard.insert(
            intern_lua_symbol("Player"),
            vec![TypeCandidate {
                name: intern_lua_symbol("Player"),
                kind: TypeDefinitionKind::Class,
                source_uri_id: uri_id,
                range: byte_range(),
                name_range: None,
            }],
        );
        agg.global_shard.push_candidate(
            "Game.Player",
            GlobalCandidate {
                name: intern_lua_symbol("Game.Player"),
                kind: GlobalContributionKind::Variable,
                type_fact: TypeFact::Unknown,
                range: byte_range(),
                selection_range: byte_range(),
                source_uri_id: uri_id,
            },
        );

        let stats = agg.stats();
        assert_eq!(stats.global_candidate_count, 1);
        assert_eq!(stats.type_candidate_count, 1);
        assert_eq!(stats.module_entry_count, 1);
    }
}
