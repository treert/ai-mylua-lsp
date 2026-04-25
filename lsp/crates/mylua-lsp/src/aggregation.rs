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
    /// Emmy type name → URIs whose summaries reference that type
    /// (P1-7 reverse type-dependency graph). Enables cascade
    /// invalidation when a class definition changes — any file that
    /// `@type`s / `@param`s / inherits from / has fields typed as
    /// the changed class needs its semantic diagnostics recomputed,
    /// even if it doesn't `require()` the defining file.
    pub type_dependants: HashMap<String, Vec<Uri>>,
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
    CallReturn { base_key: Box<CacheKey>, func_name: String },
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
    pub def_range: Option<tower_lsp_server::ls_types::Range>,
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

impl Default for WorkspaceAggregation {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkspaceAggregation {
    pub fn new() -> Self {
        Self {
            summaries: HashMap::new(),
            global_shard: HashMap::new(),
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

        // 1. Insert all summaries first so `resolve_module_to_uri`
        //    fallback path (scanning `self.summaries.keys()`) works
        //    for every file.
        for s in &summaries {
            self.summaries.insert(s.uri.clone(), s.clone());
        }

        // 2. Build all shards in a single pass.
        for summary in &summaries {
            let uri = &summary.uri;

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

            let referenced_types = collect_referenced_type_names(summary);
            for type_name in referenced_types {
                let uris = self.type_dependants.entry(type_name).or_default();
                if !uris.iter().any(|u| u == uri) {
                    uris.push(uri.clone());
                }
            }
        }

        // 3. Sort candidate lists once (not per-insert like upsert_summary).
        for candidates in self.global_shard.values_mut() {
            candidates.sort_by_cached_key(|c| uri_priority_key(&c.source_uri));
        }
        for candidates in self.type_shard.values_mut() {
            candidates.sort_by_cached_key(|c| uri_priority_key(&c.source_uri));
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

        // Collect every Emmy type name this file references — in its
        // local type facts, class parents/fields, function sigs, etc.
        // — and register `this_file` as a dependant of each.
        let referenced_types = collect_referenced_type_names(&summary);
        for type_name in referenced_types {
            let uris = self.type_dependants.entry(type_name).or_default();
            if !uris.iter().any(|u| u == &uri) {
                uris.push(uri.clone());
            }
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

        // Prune this URI from every type_dependants bucket (file may
        // be re-indexed with a different set of referenced types).
        self.type_dependants.retain(|_, uris| {
            uris.retain(|u| u != uri);
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

/// Walk `summary` for every Emmy type name it references (a shape
/// table / primitive / function type without an Emmy name is
/// ignored). Returns a sorted + deduped `Vec<String>` — used by
/// `upsert_summary` to populate `type_dependants`.
fn collect_referenced_type_names(summary: &DocumentSummary) -> Vec<String> {
    use crate::type_system::{KnownType, SymbolicStub, TypeFact};
    let mut names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    fn walk(fact: &TypeFact, out: &mut std::collections::BTreeSet<String>) {
        match fact {
            TypeFact::Known(KnownType::EmmyType(n)) => {
                out.insert(n.clone());
            }
            TypeFact::Known(KnownType::EmmyGeneric(n, params)) => {
                out.insert(n.clone());
                for p in params {
                    walk(p, out);
                }
            }
            TypeFact::Known(KnownType::Function(sig)) => {
                for p in &sig.params {
                    walk(&p.type_fact, out);
                }
                for r in &sig.returns {
                    walk(r, out);
                }
            }
            TypeFact::Stub(SymbolicStub::TypeRef { name }) => {
                out.insert(name.clone());
            }
            TypeFact::Stub(SymbolicStub::FieldOf { base, .. }) => {
                walk(base, out);
            }
            TypeFact::Union(parts) => {
                for p in parts {
                    walk(p, out);
                }
            }
            _ => {}
        }
    }

    // 1. All `---@type X local y` annotations.
    for ltf in summary.local_type_facts.values() {
        walk(&ltf.type_fact, &mut names);
    }

    // 2. `@class Foo : Parent1, Parent2` — each parent.
    for td in &summary.type_definitions {
        for parent in &td.parents {
            names.insert(parent.clone());
        }
        // 3. Every `@field` type.
        for f in &td.fields {
            walk(&f.type_fact, &mut names);
        }
        // 4. `@alias Foo = Bar | Baz` target type.
        if let Some(alias_fact) = &td.alias_type {
            walk(alias_fact, &mut names);
        }
    }

    // 5. Function param & return types.
    for fs in summary.function_summaries.values() {
        for p in &fs.signature.params {
            walk(&p.type_fact, &mut names);
        }
        for r in &fs.signature.returns {
            walk(r, &mut names);
        }
        for overload in &fs.overloads {
            for p in &overload.params {
                walk(&p.type_fact, &mut names);
            }
            for r in &overload.returns {
                walk(r, &mut names);
            }
        }
    }

    // 6. Module return type (for files that `return X` at top level).
    if let Some(mrt) = &summary.module_return_type {
        walk(mrt, &mut names);
    }

    // 7. Global contributions — `---@type Foo G = ...` stores the
    //    typed annotation on a `GlobalContribution` rather than in
    //    `local_type_facts`, so we need an explicit pass here to
    //    not miss it.
    for gc in &summary.global_contributions {
        walk(&gc.type_fact, &mut names);
    }

    // Drop self-references and generic parameter names:
    // - Self-references: a file shouldn't list itself as a dependant
    //   of its own class.
    // - Generic params: `---@class Foo<T>` + `---@field x T` treats
    //   `T` as an EmmyType during walk; `T` is NOT a real type in
    //   the workspace (unless the user happened to name a class `T`,
    //   which we then falsely cross-wire). Filter them out.
    let self_defined: std::collections::HashSet<&str> = summary
        .type_definitions
        .iter()
        .map(|td| td.name.as_str())
        .collect();
    let generic_params: std::collections::HashSet<&str> = summary
        .type_definitions
        .iter()
        .flat_map(|td| td.generic_params.iter().map(|s| s.as_str()))
        .collect();
    names
        .into_iter()
        .filter(|n| !self_defined.contains(n.as_str()))
        .filter(|n| !generic_params.contains(n.as_str()))
        .collect()
}
