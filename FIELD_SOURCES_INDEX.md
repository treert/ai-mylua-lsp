# Complete Source Code Index: Three Fields

## Quick Navigation

| Field | Definition | Populated | Consumed |
|-------|-----------|-----------|----------|
| `global_contributions` | `summary.rs:155-163` | `visitors.rs:700,1056,1147` | `aggregation.rs:436,514` |
| `global_ref_tree` | `summary.rs:9-52` | `mod.rs:101-104` | `mod.rs:555-596 (tests)` |
| `function_name_index` | `summary.rs:71-75` | `visitors.rs:700` | `signature_help.rs:123` |

---

## 1. Type Definitions

### Location: `lsp/crates/mylua-lsp/src/summary.rs`

#### GlobalContribution (lines 155-163)
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalContribution {
    pub name: String,
    pub kind: GlobalContributionKind,
    pub type_fact: TypeFact,
    pub range: ByteRange,
    pub selection_range: ByteRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GlobalContributionKind {
    Variable,
    Function,
    TableExtension,
}
```

#### GlobalRefTree (lines 9-52)
```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalRefTree {
    pub roots: HashMap<String, GlobalRefNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalRefNode {
    pub children: HashMap<String, GlobalRefNode>,
}

impl GlobalRefTree {
    pub fn insert(&mut self, segments: &[String]) { ... }
    pub fn is_empty(&self) -> bool { ... }
}
```

#### function_name_index field (lines 71-75)
```rust
/// Reverse index: function name → FunctionSummaryId.
pub function_name_index: HashMap<String, FunctionSummaryId>,
```

#### Lookup helper (lines 246-250)
```rust
pub fn get_function_by_name(&self, name: &str) -> Option<&FunctionSummary> {
    let normalized = name.replace(':', ".");
    self.function_name_index.get(&normalized)
        .and_then(|id| self.function_summaries.get(id))
}
```

---

## 2. Population / Building

### Location: `lsp/crates/mylua-lsp/src/summary_builder/`

#### Entry Point: `mod.rs:30-107` `build_file_analysis`
- Initializes empty `BuildContext` with all three fields
- Calls `visit_top_level` to populate via AST traversal
- Calls `collect_external_refs` to build `global_ref_tree`
- Returns `DocumentSummary` with all three fields populated

**Relevant lines**:
- Lines 36-58: `BuildContext` initialization
  - `global_contributions: Vec::new()`
  - `function_name_index: HashMap::new()`
  - All initialized empty
- Lines 80-98: `DocumentSummary` construction
- Lines 101-104: `collect_external_refs` call → populates `global_ref_tree`

#### Populating `global_contributions`

**File**: `visitors.rs`

**Path 1: Global Functions (lines 698-710)**
```rust
// In visit_function_declaration
let normalized = name.replace(':', ".");
ctx.function_name_index.insert(normalized.clone(), func_id);  // 3️⃣
ctx.global_contributions.push(GlobalContribution {            // 1️⃣
    name: normalized,
    kind: GlobalContributionKind::Function,
    type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
});
```

**Path 2: Global Variables (lines 1056-1062)**
```rust
// In visit_assignment, when LHS is bare global name
ctx.global_contributions.push(GlobalContribution {
    name,
    kind: GlobalContributionKind::Variable,
    type_fact,
    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    selection_range: ctx.line_index.ts_node_to_byte_range(var_node, ctx.source),
});
```

**Path 3: Table Extensions (lines 1147-1153)**
```rust
// In visit_assignment, when LHS is dotted path (e.g., Player.health)
let name = chain.joined();
ctx.global_contributions.push(GlobalContribution {
    name,
    kind: GlobalContributionKind::TableExtension,
    type_fact,
    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    selection_range: ctx.line_index.ts_node_to_byte_range(var_node, ctx.source),
});
```

#### Populating `function_name_index`

**File**: `visitors.rs:700`

```rust
let normalized = name.replace(':', ".");
ctx.function_name_index.insert(normalized.clone(), func_id);
```

**Context**: Same as Path 1 for `global_contributions` above.
- Only global functions (non-local)
- Names normalized: `Player:new` → `Player.new`
- Happens in `visit_function_declaration`

#### Populating `global_ref_tree`

**File**: `mod.rs:101-104`

```rust
let (global_ref_tree, referenced_type_names) =
    collect_external_refs(&summary, &scope_tree);
summary.global_ref_tree = global_ref_tree;
summary.referenced_type_names = referenced_type_names;
```

**Function**: `collect_external_refs` (lines 143-299)

Core algorithm:
1. Walk all TypeFact instances in file (scope declarations, type definitions, function signatures, etc.)
2. Extract `GlobalRef` and `FieldOf` chains
3. Split dotted paths into segments
4. Insert segments into trie via `GlobalRefTree::insert`

**Key code** (lines 204-210):
```rust
TypeFact::Stub(SymbolicStub::GlobalRef { name }) => {
    global_out.insert(&global_ref_segments(name));
}
TypeFact::Stub(SymbolicStub::FieldOf { base, .. }) => {
    if let Some(segs) = extract_global_path(fact) {
        global_out.insert(&segs);
    }
}
```

#### Fingerprinting `global_contributions`

**File**: `fingerprint.rs:169-177`

```rust
let mut globals: Vec<_> = ctx.global_contributions.iter().collect();
globals.sort_by(|a, b| {
    a.name.cmp(&b.name)
        .then_with(|| a.selection_range.start_byte.cmp(&b.selection_range.start_byte))
});
// ... hash the globals
```

---

## 3. Consumption / Queries

### Aggregation

**File**: `aggregation.rs`

**Lines 436-445** — inserting from new summary:
```rust
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
```

**Lines 514-523** — re-inserting with priority sorting:
```rust
for gc in &summary.global_contributions {
    self.global_shard.push_candidate(&gc.name, GlobalCandidate { ... });
    self.global_shard.sort_at(&gc.name, |c| uri_priority_key(&c.source_uri));
}
```

### Type Inference

**File**: `type_inference.rs:301-323`

```rust
let fs_data = index.summaries.get(uri).and_then(|summary| {
    // Local function via scope tree → FunctionRef(id)
    if let Some(KnownType::FunctionRef(fid)) = scope_tree.resolve_type(...) {
        // use local function
    }
    // Global function via function_name_index  ← 3️⃣
    summary.get_function_by_name(callee_text).map(|fs| {
        (
            fs.generic_params.clone(),
            fs.signature.params.clone(),
            fs.signature.returns.clone(),
        )
    })
});
```

### Signature Help

**File**: `signature_help.rs:120-126`

```rust
// Global function via function_name_index (O(1)).
if let Some(fs) = summary.get_function_by_name(&name) {
    let sigs = primary_plus_overloads(fs);
    return Some((sigs, false, name));
}
```

### Call Hierarchy

**File**: `call_hierarchy.rs:66, 82-86`

```rust
// Global function via function_name_index
if let Some(fs) = summary.get_function_by_name(&name) {
    return vec![build_item(
        fs.name.clone(),
        SymbolKind::FUNCTION,
        uri.clone(),
        // ...
    )];
}
```

### Goto Definition

**File**: `goto.rs:354-359`

```rust
let target_range = index.summaries.get(&target_uri)
    .and_then(|s| {
        s.module_return_range
            .or_else(|| s.global_contributions.first().map(|gc| gc.selection_range))
    })
```

### Tests

**File**: `summary_builder/mod.rs:555-596`

Tests for `global_ref_tree` (primary consumers currently):

- `global_ref_tree_simple_global()` — line 555
- `global_ref_tree_dotted_path()` — line 564
- `global_ref_tree_self_contributions_kept()` — line 576
- `global_ref_tree_locals_excluded()` — line 585
- `global_ref_tree_multiple_refs_merge()` — line 593

---

## 4. Integration Points

### Flow: From Source File to LSP Response

```
1. File edited
   ↓
2. tree-sitter parse
   ↓
3. build_file_analysis (summary_builder/mod.rs:30)
   ├─ visit_top_level (visitors.rs)
   │  ├─ visit_function_declaration → [1️⃣, 3️⃣]
   │  └─ visit_assignment → [1️⃣]
   └─ collect_external_refs (mod.rs:143) → [2️⃣]
   ↓
4. DocumentSummary with [1️⃣, 2️⃣, 3️⃣]
   ↓
5. WorkspaceAggregation::upsert_summary (aggregation.rs:436)
   └─ For each gc in [1️⃣]: global_shard.push_candidate
   ↓
6. User hover / goto / signature-help query
   ├─ signature_help.rs:123 → [3️⃣].get_function_by_name()
   └─ aggregation.rs → GlobalShard lookup
   ↓
7. LSP response
```

### BuildContext Lifecycle

**File**: `summary_builder/mod.rs:379-419`

```rust
pub(crate) struct BuildContext<'a> {
    pub(crate) global_contributions: Vec<GlobalContribution>,
    pub(crate) function_name_index: HashMap<String, FunctionSummaryId>,
    pub(crate) function_name_to_id: HashMap<String, FunctionSummaryId>,
    // ... other fields
}
```

Transfer to `DocumentSummary` (lines 80-98):
```rust
let mut summary = DocumentSummary {
    uri: uri.clone(),
    content_hash,
    require_bindings: ctx.require_bindings,
    global_contributions: ctx.global_contributions,  // Transfer [1️⃣]
    function_name_index: ctx.function_name_index,   // Transfer [3️⃣]
    type_definitions: ctx.type_definitions,
    // ...
    global_ref_tree: GlobalRefTree::default(),      // Placeholder
    referenced_type_names: std::collections::HashSet::new(),
};
```

---

## 5. Related Data Structures

### `GlobalCandidate` (aggregation.rs:44-52)
- Created from each `GlobalContribution` during aggregation
- Adds `source_uri` for cross-file tracking

### `GlobalShard` (aggregation.rs:85-91)
- Tree-structured index built from `global_contributions`
- Enables workspace-wide global name queries

### `FunctionSummary` (summary.rs:173-189)
- Keyed by `FunctionSummaryId`
- Referenced indirectly via `function_name_index`

### `ScopeTree` (companion to DocumentSummary)
- Local functions indexed here via `FunctionRef`
- Complementary to global `function_name_index`

---

## 6. Key Files at a Glance

| File | Purpose | Lines | Content |
|------|---------|-------|---------|
| `summary.rs` | Type definitions | 9-52, 155-170, 71-75, 246-250 | Structs + lookup helper |
| `summary_builder/mod.rs` | Entry point + tree building | 30-107, 143-299 | `build_file_analysis` + `collect_external_refs` |
| `summary_builder/visitors.rs` | AST traversal → contributions | 700, 1056, 1147 | 3 paths populating `global_contributions` + `function_name_index` |
| `summary_builder/fingerprint.rs` | Cascade invalidation | 171-177 | Hash `global_contributions` |
| `aggregation.rs` | Workspace merge | 436-445, 514-523 | Transfer `global_contributions` → GlobalShard |
| `type_inference.rs` | Cross-file queries | 303-323 | Lookup `function_name_index` for call resolution |
| `signature_help.rs` | LSP hover info | 120-126 | Query `function_name_index` |
| `call_hierarchy.rs` | Incoming/outgoing calls | 66, 82-86 | Query `function_name_index` |
| `goto.rs` | Definition jumping | 354-359 | Fallback to `global_contributions` |

