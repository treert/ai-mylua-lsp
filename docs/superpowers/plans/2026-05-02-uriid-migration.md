# UriId Migration Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the remaining internal `Uri` to `UriId` migration without changing LSP-facing `Uri` output behavior.

**Architecture:** Keep LSP protocol boundaries URI-facing, but move resolver locations, field-reference identity, diagnostics scheduling, and aggregation candidates toward compact internal IDs. Use aggregation-local `UriId` only inside `WorkspaceAggregation`/resolver data, and session-local `UriId` only for `Backend.documents`/scheduler/document store paths; do not mix the two domains.

**Tech Stack:** Rust, tower-lsp-server, tree-sitter, Cargo integration tests

**Current Baseline:** Latest completed commits include `a6d71d4 refactor: avoid interning read-only document lookups` and `fe52c0e refactor: schedule diagnostics with UriId keys`. `cd lsp && cargo test --tests && cargo build` passed after those commits. Rust formatting commands are forbidden; edit locally and preserve existing formatting.

---

## Execution Rules

- Execute tasks serially. Do not run implementation subagents in parallel because these tasks touch the same resolver/consumer data flow.
- Each task must be implemented by a fresh subagent or directly by the main agent, then reviewed before continuing.
- Each task must end with:
  - `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test <targeted tests>`
  - `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build`
  - `ReadLints` for changed Rust files
  - code-reviewer subagent
  - one git commit if verification and review pass
- Do not run `cargo fmt`, `rustfmt`, IDE format, or bulk formatting scripts.
- Do not remove URI values required for LSP output until every consumer has an explicit way to resolve an id back to a URI.

---

### Task 1: Add Aggregation Location Accessors

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/aggregation.rs`
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_goto.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_hover.rs`

- [ ] **Step 1: Add read-only summary id accessors**

In `lsp/crates/mylua-lsp/src/aggregation.rs`, keep the existing private mutating `summary_uri_id(&mut self, uri: &Uri) -> UriId` allocator. Add read-only accessors near `summary()`:

```rust
    pub(crate) fn summary_id(&self, uri: &Uri) -> Option<UriId> {
        self.summary_uri_ids.get(uri).copied()
    }

    pub(crate) fn summary_by_id(&self, uri_id: UriId) -> Option<&DocumentSummary> {
        self.summaries.get(&uri_id)
    }

    pub(crate) fn summary_uri(&self, uri_id: UriId) -> Option<&Uri> {
        self.summaries.get(&uri_id).map(|summary| &summary.uri)
    }
```

Do not make the mutating allocator public. The read-only accessor must return `None` for unknown URIs instead of allocating.

- [ ] **Step 2: Expose candidate source ids within the crate**

In `GlobalCandidate` and `TypeCandidate`, change `fn source_uri_id(&self) -> UriId` to:

```rust
    pub(crate) fn source_uri_id(&self) -> UriId {
        self.source_uri_id
    }
```

Keep `source_uri(&self) -> &Uri` unchanged in this task. It remains the compatibility boundary while consumers migrate.

- [ ] **Step 3: Add resolver location type without removing existing fields**

In `lsp/crates/mylua-lsp/src/resolver.rs`, add `UriId` import:

```rust
use crate::uri_id::UriId;
```

Add this type above `ResolvedType`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedLocation {
    pub uri_id: UriId,
    pub range: ByteRange,
}
```

Extend `ResolvedType`:

```rust
pub struct ResolvedType {
    pub type_fact: TypeFact,
    pub def_uri: Option<Uri>,
    pub def_range: Option<ByteRange>,
    pub def_location: Option<ResolvedLocation>,
}
```

Update constructors:

```rust
fn unknown() -> Self {
    Self { type_fact: TypeFact::Unknown, def_uri: None, def_range: None, def_location: None }
}

fn from_fact(fact: TypeFact) -> Self {
    Self { type_fact: fact, def_uri: None, def_range: None, def_location: None }
}

fn with_location(fact: TypeFact, uri: Uri, range: ByteRange, uri_id: Option<UriId>) -> Self {
    Self {
        type_fact: fact,
        def_uri: Some(uri),
        def_range: Some(range),
        def_location: uri_id.map(|id| ResolvedLocation { uri_id: id, range }),
    }
}
```

Update all `ResolvedType { ... }` literals in `resolver.rs` to set `def_location`. When a location is seeded from a URI hint, use `agg.summary_id(uri)` to populate it. When a location is seeded from a candidate, use `candidate.source_uri_id()`.

- [ ] **Step 4: Update `with_location` call sites**

Replace existing calls:

```rust
ResolvedType::with_location(fact, uri, range)
```

with:

```rust
ResolvedType::with_location(fact, uri, range, uri_id)
```

Use the following source for `uri_id`:
- Global candidate: `Some(c.source_uri_id())`
- Type candidate: `Some(candidate.source_uri_id())`
- Table field from URI hint: `agg.summary_id(uri)`
- Union best location: preserve `(Uri, ByteRange, Option<UriId>)`, not just `(Uri, ByteRange)`

- [ ] **Step 5: Add resolver regression assertions**

Add focused assertions to existing tests instead of broad new fixtures:

In `test_goto.rs`, add a case near existing require/table goto tests that resolves a field from a required table and asserts the public goto result remains the same URI/range. The test should exercise the resolver location path indirectly through existing public API.

In `test_hover.rs`, add a hover case for a field declared in a different file and asserted through `hover::hover(...)`; expected content should remain unchanged.

- [ ] **Step 6: Verify Task 1**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --test test_goto --test test_hover
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build
```

Expected: all selected tests pass and build exits 0.

- [ ] **Step 7: Review and commit Task 1**

Request code review with context:

```text
Task 1 added aggregation summary id accessors and resolver ResolvedLocation while preserving def_uri/def_range compatibility.
```

If review passes:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
git add lsp/crates/mylua-lsp/src/aggregation.rs lsp/crates/mylua-lsp/src/resolver.rs lsp/crates/mylua-lsp/tests/test_goto.rs lsp/crates/mylua-lsp/tests/test_hover.rs
git commit -m "refactor: attach UriId to resolver locations"
```

---

### Task 2: Use Resolver Locations in Field References

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/references.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_references.rs`

- [ ] **Step 1: Change field identity to store `ResolvedLocation`**

In `references.rs`, import the type:

```rust
use crate::resolver::ResolvedLocation;
```

Change `Identity::Field` from:

```rust
Field {
    field_name: String,
    def_uri: Uri,
    def_range: ByteRange,
},
```

to:

```rust
Field {
    field_name: String,
    location: ResolvedLocation,
},
```

- [ ] **Step 2: Convert identity creation to require `def_location`**

In `try_identify_field`, replace:

```rust
let def_uri = resolved.def_uri?;
let def_range = resolved.def_range?;

Some(Identity::Field {
    field_name,
    def_uri,
    def_range,
})
```

with:

```rust
let location = resolved.def_location?;

Some(Identity::Field {
    field_name,
    location,
})
```

In `resolve_segments_to_field`, change the return type from:

```rust
Option<(Uri, ByteRange)>
```

to:

```rust
Option<ResolvedLocation>
```

Return `resolved.def_location` instead of `(resolved.def_uri?, resolved.def_range?)`.

- [ ] **Step 3: Resolve URI only at LSP output boundary**

In `find_references`, for `Identity::Field`, resolve the declaration URI through the index:

```rust
let Some(identity_def_uri) = index.summary_uri(location.uri_id) else {
    return None;
};
let identity_def_range = location.range;
```

Use `identity_def_uri.clone()` only when creating `Location`.

For range conversion, keep using:

```rust
range_from_byte_range(identity_def_uri, identity_def_range, all_docs)
```

This keeps LSP output URI-facing while identity comparison is id-based.

- [ ] **Step 4: Compare fields by id and range**

Change `verify_field` signature from:

```rust
fn verify_field(
    node: tree_sitter::Node,
    target_def_uri: &Uri,
    target_def_range: &ByteRange,
    ...
) -> bool
```

to:

```rust
fn verify_field(
    node: tree_sitter::Node,
    target_location: ResolvedLocation,
    ...
) -> bool
```

Inside the dotted-chain branch, compare:

```rust
return location == target_location;
```

Inside the regular field branch, compare:

```rust
resolved.def_location == Some(target_location)
```

Keep `doc_uri: &Uri` parameters where needed for type inference and scanning; those are current-file context, not stored identity.

- [ ] **Step 5: Add or adjust references test**

In `test_references.rs`, ensure there is a field-reference test where declaration is in one file and usage is in another. If one already exists, extend it to assert:

```rust
assert!(locations.iter().any(|loc| loc.uri == def_uri));
assert!(locations.iter().any(|loc| loc.uri == use_uri));
```

The purpose is to prove id-based identity still resolves back to correct LSP URIs.

- [ ] **Step 6: Verify Task 2**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --test test_references --test test_goto --test test_hover
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build
```

Expected: all selected tests pass and build exits 0.

- [ ] **Step 7: Review and commit Task 2**

Request code review with context:

```text
Task 2 migrated field reference identity from Uri+range to resolver ResolvedLocation (UriId+range), resolving Uri only for LSP Location output.
```

If review passes:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
git add lsp/crates/mylua-lsp/src/references.rs lsp/crates/mylua-lsp/tests/test_references.rs
git commit -m "refactor: identify field references by UriId location"
```

---

### Task 3: Migrate Resolver Consumers to `ResolvedLocation`

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/hover.rs`
- Modify: `lsp/crates/mylua-lsp/src/signature_help.rs`
- Modify: `lsp/crates/mylua-lsp/src/type_inference.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_hover.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_signature_help.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_diagnostics.rs`

- [ ] **Step 1: Add small URI resolution helpers at each consumer boundary**

Do not add a global abstraction unless it removes duplication in at least two files. In each consumer file, use the same local pattern:

```rust
let Some(location) = resolved.def_location else {
    return None;
};
let Some(def_uri) = index.summary_uri(location.uri_id) else {
    return None;
};
let def_range = location.range;
```

Use this pattern only where the code currently consumes `resolved.def_uri` and `resolved.def_range` together.

- [ ] **Step 2: Update `hover.rs` field hover path**

In `hover.rs`, replace the field-hover block that currently checks:

```rust
if let (Some(def_uri), Some(def_range)) = (&resolved.def_uri, &resolved.def_range) {
```

with `def_location` resolution through `index.summary_uri(location.uri_id)`.

Keep `types::Definition { uri: def_uri.clone(), ... }` because hover rendering and doc lookup are URI-facing.

- [ ] **Step 3: Update `signature_help.rs` resolved function paths**

In `signature_help.rs`, replace `resolved.def_uri` reads for `FunctionRef(fid)` lookup with `resolved.def_location` where a location exists:

```rust
if let (KnownType::FunctionRef(fid), Some(location)) = (&resolved.type_fact, resolved.def_location) {
    if let Some(summary) = index.summary_by_id(location.uri_id) {
        if let Some(fs) = summary.function_summaries.get(fid) {
            return primary_plus_overloads(fs);
        }
    }
}
```

Where code needs a URI for comparing implementation file with declaration file, resolve it once:

```rust
let def_uri = resolved
    .def_location
    .and_then(|location| index.summary_uri(location.uri_id));
```

Keep existing `candidate.source_uri()` paths for global-shard direct lookups until Task 4.

- [ ] **Step 4: Update `type_inference.rs` FunctionRef summary lookup**

Replace:

```rust
if let Some(ref uri) = field_result.def_uri {
    if let Some(summary) = index.summary(uri) {
```

with:

```rust
if let Some(location) = field_result.def_location {
    if let Some(summary) = index.summary_by_id(location.uri_id) {
```

This removes a resolver consumer from the URI-based lookup path without changing inferred types.

- [ ] **Step 5: Verify Task 3**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --test test_hover --test test_signature_help --test test_diagnostics
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build
```

Expected: all selected tests pass and build exits 0.

- [ ] **Step 6: Review and commit Task 3**

Request code review with context:

```text
Task 3 migrated hover/signature/type inference resolver consumers to ResolvedLocation where both URI and range were consumed together.
```

If review passes:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
git add lsp/crates/mylua-lsp/src/hover.rs lsp/crates/mylua-lsp/src/signature_help.rs lsp/crates/mylua-lsp/src/type_inference.rs lsp/crates/mylua-lsp/tests/test_hover.rs lsp/crates/mylua-lsp/tests/test_signature_help.rs lsp/crates/mylua-lsp/tests/test_diagnostics.rs
git commit -m "refactor: consume resolver locations by UriId"
```

---

### Task 4: Remove URI Storage from Aggregation Candidates

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/aggregation.rs`
- Modify: `lsp/crates/mylua-lsp/src/workspace_symbol.rs`
- Modify: `lsp/crates/mylua-lsp/src/hover.rs`
- Modify: `lsp/crates/mylua-lsp/src/signature_help.rs`
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs`
- Modify: `lsp/crates/mylua-lsp/src/references.rs`
- Modify: `lsp/crates/mylua-lsp/src/completion.rs`
- Modify: `lsp/crates/mylua-lsp/src/goto.rs`
- Modify: `lsp/crates/mylua-lsp/src/call_hierarchy.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_workspace_symbol.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_hover.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_signature_help.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_type_definition.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_completion.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_goto.rs`
- Test: `lsp/crates/mylua-lsp/tests/test_call_hierarchy.rs`

- [ ] **Step 1: Add explicit candidate URI resolver methods**

In `aggregation.rs`, add methods on `WorkspaceAggregation`:

```rust
    pub(crate) fn candidate_uri(&self, candidate: &GlobalCandidate) -> Option<&Uri> {
        self.summary_uri(candidate.source_uri_id())
    }

    pub(crate) fn type_candidate_uri(&self, candidate: &TypeCandidate) -> Option<&Uri> {
        self.summary_uri(candidate.source_uri_id())
    }
```

These are temporary compatibility helpers. They make all URI resolution explicit at aggregation boundaries.

- [ ] **Step 2: Replace external `candidate.source_uri()` calls**

Search:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
rg "source_uri\\(\\)" lsp/crates/mylua-lsp/src
```

For call sites outside `aggregation.rs`, replace:

```rust
candidate.source_uri()
```

with one of:

```rust
index.candidate_uri(candidate)?
index.type_candidate_uri(candidate)?
```

or, in functions that cannot use `?`, use:

```rust
let Some(uri) = index.candidate_uri(candidate) else {
    continue;
};
```

Expected files include `workspace_symbol.rs`, `hover.rs`, `signature_help.rs`, `resolver.rs`, `references.rs`, `completion.rs`, `goto.rs`, and `call_hierarchy.rs`.

- [ ] **Step 3: Remove `source_uri: Uri` from candidates**

In `aggregation.rs`, remove the `source_uri: Uri` field from both structs:

```rust
pub struct GlobalCandidate {
    pub name: String,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: ByteRange,
    pub selection_range: ByteRange,
    source_uri_id: UriId,
}

pub struct TypeCandidate {
    pub name: String,
    pub kind: crate::summary::TypeDefinitionKind,
    source_uri_id: UriId,
    pub range: ByteRange,
}
```

Remove candidate construction lines:

```rust
source_uri: uri.clone(),
```

Remove `source_uri(&self) -> &Uri` methods from both candidate impl blocks.

- [ ] **Step 4: Update sorting to use id-resolved URI**

In `build_initial`, keep the existing `uri_priority` map from summary URI to priority, but sort candidates through `summary_uri`:

```rust
self.global_shard.sort_all(|c| {
    self.summary_uri(c.source_uri_id())
        .and_then(|uri| uri_priority.get(uri).copied())
        .unwrap_or(default_priority)
});
```

If this creates borrow conflicts because `self.global_shard` is mutably borrowed, precompute:

```rust
let id_priority: HashMap<UriId, (usize, usize, usize)> = self
    .summaries
    .iter()
    .map(|(id, summary)| (*id, uri_priority_key(&summary.uri)))
    .collect();
```

Then sort with:

```rust
*id_priority.get(&c.source_uri_id()).unwrap_or(&default_priority)
```

Use the same `id_priority` approach for `type_shard`.

In `upsert_summary`, replace:

```rust
self.global_shard.sort_at(&gc.name, |c| uri_priority_key(&c.source_uri));
candidates.sort_by_cached_key(|c| uri_priority_key(&c.source_uri));
```

with:

```rust
let summary_priorities: HashMap<UriId, (usize, usize, usize)> = self
    .summaries
    .iter()
    .map(|(id, summary)| (*id, uri_priority_key(&summary.uri)))
    .collect();
let current_priority = uri_priority_key(&uri);

self.global_shard.sort_at(&gc.name, |c| {
    summary_priorities
        .get(&c.source_uri_id())
        .copied()
        .unwrap_or(current_priority)
});
```

For `type_shard`, use the same key closure.

- [ ] **Step 5: Verify no candidate URI accessor remains**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
rg "source_uri\\(\\)|source_uri: Uri" lsp/crates/mylua-lsp/src
```

Expected: no matches outside comments that explicitly describe removed legacy behavior. If comments mention `source_uri()` as current API, update them.

- [ ] **Step 6: Verify Task 4**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --test test_workspace_symbol --test test_hover --test test_signature_help --test test_type_definition --test test_references --test test_completion --test test_goto --test test_call_hierarchy
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build
```

Expected: all selected tests pass and build exits 0.

- [ ] **Step 7: Review and commit Task 4**

Request code review with context:

```text
Task 4 removed stored Uri fields from aggregation candidates; consumers now resolve candidate Uri from source_uri_id through WorkspaceAggregation at LSP output boundaries.
```

If review passes:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
git add lsp/crates/mylua-lsp/src/aggregation.rs lsp/crates/mylua-lsp/src/workspace_symbol.rs lsp/crates/mylua-lsp/src/hover.rs lsp/crates/mylua-lsp/src/signature_help.rs lsp/crates/mylua-lsp/src/resolver.rs lsp/crates/mylua-lsp/src/references.rs lsp/crates/mylua-lsp/src/completion.rs lsp/crates/mylua-lsp/src/goto.rs lsp/crates/mylua-lsp/src/call_hierarchy.rs lsp/crates/mylua-lsp/tests/test_workspace_symbol.rs lsp/crates/mylua-lsp/tests/test_hover.rs lsp/crates/mylua-lsp/tests/test_signature_help.rs lsp/crates/mylua-lsp/tests/test_type_definition.rs lsp/crates/mylua-lsp/tests/test_references.rs lsp/crates/mylua-lsp/tests/test_completion.rs lsp/crates/mylua-lsp/tests/test_goto.rs lsp/crates/mylua-lsp/tests/test_call_hierarchy.rs
git commit -m "refactor: store aggregation candidate sources as UriId"
```

---

### Task 5: Final URI Surface Audit and Documentation

**Files:**
- Modify: `docs/index-architecture.md` if Task 4 removes `source_uri: Uri` from aggregation candidates.
- Modify: `docs/architecture.md` if Task 1 introduces `ResolvedLocation`.
- Leave unchanged: `docs/performance-analysis.md` unless the audit finds a documented URI-keyed hot path that no longer exists.
- Read-only audit: `lsp/crates/mylua-lsp/src/**/*.rs`

- [ ] **Step 1: Run URI surface audit**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
rg "HashMap<Uri|HashSet<Uri|Vec<\\(String, Uri\\)>|source_uri: Uri|def_uri: Option<Uri>|source_uri\\(\\)|document_id\\(" lsp/crates/mylua-lsp/src
```

Classify remaining matches into one of these categories:

- LSP output boundary: `WorkspaceEdit`, `Location`, `DocumentLink`, publish diagnostics, workspace scanner path-to-URI conversion.
- Interner/internal mapping: `uri_id.rs`, aggregation `summary_uri_ids`, aggregation `module_uri_ids`.
- Compatibility field still intentionally present: `ResolvedType.def_uri` / `FieldCompletion.def_uri`, if Tasks 1-4 did not remove them yet.
- Migration miss: any hot internal store, queue, cache, reverse index, or candidate field still keyed by/storing full `Uri` without a boundary reason.

- [ ] **Step 2: Decide whether to remove `ResolvedType.def_uri` compatibility**

If the audit shows no external consumer reads `ResolvedType.def_uri` except fallback compatibility paths, remove `def_uri` and `def_range` from `ResolvedType` and make `def_location` the single resolver location field.

If removing them would force broad churn in one step, keep them and add this comment above `ResolvedType`:

```rust
// `def_location` is the internal identity used by migrated consumers.
// `def_uri`/`def_range` remain as URI-facing compatibility fields for
// consumers that directly construct LSP responses; remove them only after
// those boundaries resolve through `WorkspaceAggregation`.
```

Do not preserve both indefinitely without the comment.

- [ ] **Step 3: Update docs only if behavior or architecture changed**

If Task 4 removed candidate URI storage, update `docs/index-architecture.md` to say aggregation candidates store source `UriId` and resolve back to `Uri` through `WorkspaceAggregation` at LSP output boundaries.

If Task 1 introduced `ResolvedLocation`, update `docs/architecture.md` or `docs/index-architecture.md` with one short paragraph:

```markdown
Resolver definition locations now carry aggregation-local `UriId` plus `ByteRange` internally. LSP handlers and feature modules resolve the id back to `Uri` only when constructing protocol-facing `Location`, hover, completion, or signature-help responses.
```

Do not update `ai-readme.md` unless an LSP capability list changes. This migration should not add or remove capabilities.

- [ ] **Step 4: Run full verification**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests && cargo build
```

Expected: all tests pass and build exits 0.

- [ ] **Step 5: Final code review**

Request code review with context:

```text
Final UriId migration audit: resolver locations and aggregation candidates now use internal UriId for migrated internal identity paths; LSP output boundaries still resolve to Uri. Full cargo test --tests && cargo build passed.
```

Fix all Critical and Important issues before committing.

- [ ] **Step 6: Commit final audit/docs**

If docs changed:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
git add docs/architecture.md docs/index-architecture.md docs/performance-analysis.md lsp/crates/mylua-lsp/src
git commit -m "docs: update UriId migration architecture notes"
```

If only code cleanup changed:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp
git add lsp/crates/mylua-lsp/src
git commit -m "refactor: complete UriId migration audit"
```

---

## Handoff Notes for Subagents

Use these constraints in every implementation prompt:

- Read `ai-readme.md` and `docs/README.md` first.
- Do not run Rust formatting commands.
- Keep the task scope exactly to the current task.
- Do not rewrite unrelated resolver/type inference logic.
- Do not remove `Uri` at LSP boundaries; protocol output still requires `Uri`.
- Prefer adding small accessors over exposing internal maps.
- Preserve existing tests unless the test expectation is explicitly about an internal representation that the task changes.
- Return status as one of:
  - `DONE`
  - `DONE_WITH_CONCERNS`
  - `NEEDS_CONTEXT`
  - `BLOCKED`

## Recommended Execution Choice

Use **Subagent-Driven Development**, serially:

1. Main agent dispatches one implementer subagent for Task 1 with the full task text.
2. Implementer runs tests, self-reviews, and commits only if verification passes.
3. Main agent runs a spec-compliance review and a code-quality review.
4. Main agent proceeds to the next task only after review issues are fixed.

Inline execution in this session is acceptable if a task proves too coupled for delegation, but do not mix manual edits into a subagent-owned task unless the subagent reports `BLOCKED`.
