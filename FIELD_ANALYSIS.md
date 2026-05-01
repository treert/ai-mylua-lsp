# Analysis: `global_contributions`, `global_ref_tree`, and `function_name_index`

## Overview

These three fields are part of the **`DocumentSummary`** struct defined in `summary.rs` (lines 58-123). They represent different aspects of a single file's type information extracted during single-file inference, ready to be merged into the workspace aggregation layer.

---

## 1. `global_contributions: Vec<GlobalContribution>`

### Type Definition
**File**: `lsp/crates/mylua-lsp/src/summary.rs` (lines 155-163)

```rust
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

### Purpose
- Records **all global names defined/extended by this file** (assignments, function declarations, table field assignments)
- Each entry is a candidate definition that participates in the **GlobalShard** (workspace-level global name index)
- Excludes local variables, which are tracked separately in `ScopeTree`

### Populating `global_contributions`

**Location**: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`

Three main paths populate this field:

#### 1. **Global Function Declarations** (lines 698-710)
```rust
// function Player.new() ... end
// function add(x) ... end  -- only global functions, not local
ctx.function_name_index.insert(normalized.clone(), func_id);
ctx.global_contributions.push(GlobalContribution {
    name: normalized,
    kind: GlobalContributionKind::Function,
    type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
});
```

**Conditions**:
- The function is not a local (no `local function` keyword)
- The base is not a local variable (e.g., in `function Foo.bar()`, `Foo` must not be local)
- Colon-qualified names normalized: `Player:new` → `Player.new`

#### 2. **Global Variable Assignments** (lines 1056-1062)
```rust
// G = expr  or  local G = expr (at top level)
ctx.global_contributions.push(GlobalContribution {
    name,
    kind: GlobalContributionKind::Variable,
    type_fact,
    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    selection_range: ctx.line_index.ts_node_to_byte_range(var_node, ctx.source),
});
```

**Conditions**:
- Variable is bare name (not inside a scope)
- Assignment is at top level
- Type inferred from RHS or from pending `---@type` annotation

#### 3. **Global Table Extensions** (lines 1147-1153)
```rust
// ModuleA.utils.func = value
// Player.health = 100
ctx.global_contributions.push(GlobalContribution {
    name,  // e.g., "ModuleA.utils.func"
    kind: GlobalContributionKind::TableExtension,
    type_fact,
    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    selection_range: ctx.line_index.ts_node_to_byte_range(var_node, ctx.source),
});
```

**Conditions**:
- Dotted path (`a.b.c = expr`)
- Base is **not** a local variable
- Pure dotted chain (no function calls, subscripts, or parentheses)

### Consuming `global_contributions`

#### 1. **Aggregation** (`lsp/crates/mylua-lsp/src/aggregation.rs`)
- Lines 436-445, 514-523: Each `GlobalContribution` is converted to a `GlobalCandidate` and inserted into the **GlobalShard** (tree-structured global name index)
- URI is attached so later queries know which file provided each candidate

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

#### 2. **Fingerprinting** (`lsp/crates/mylua-lsp/src/summary_builder/fingerprint.rs`)
- Lines 171-177: Global contributions are sorted by name and hashed to compute the **signature fingerprint**
- Used for cascade invalidation: if fingerprint doesn't change, dependant files don't need revalidation

```rust
let mut globals: Vec<_> = ctx.global_contributions.iter().collect();
globals.sort_by(|a, b| {
    a.name.cmp(&b.name)
        .then_with(|| a.selection_range.start_byte.cmp(&b.selection_range.start_byte))
});
// ... hash the globals
```

#### 3. **Goto Definition** (`lsp/crates/mylua-lsp/src/goto.rs`)
- Line 359: When jumping to a module's exports, falls back to the first global contribution's selection range
- Used as a fallback when `module_return_range` is not present

```rust
let target_range = index.summaries.get(&target_uri)
    .and_then(|s| {
        s.module_return_range
            .or_else(|| s.global_contributions.first().map(|gc| gc.selection_range))
    })
```

---

## 2. `global_ref_tree: GlobalRefTree`

### Type Definition
**File**: `lsp/crates/mylua-lsp/src/summary.rs` (lines 9-52)

```rust
/// External global names referenced by a file, stored as a trie.
///
/// "ModuleA.utils.func" + "ModuleA.utils.abc" + "print" →
/// roots: {
///   "ModuleA": { children: { "utils": { children: { "func": {}, "abc": {} } } } },
///   "print": {},
/// }
///
/// Leaf nodes (children.is_empty()) indicate "referenced at this level";
/// they also implicitly mean "anything below may affect me" when the deeper
/// path couldn't be statically resolved.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalRefTree {
    pub roots: HashMap<String, GlobalRefNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalRefNode {
    pub children: HashMap<String, GlobalRefNode>,
}
```

### Purpose
- Stores a **trie (prefix tree) of external global references** made by this file
- Maps dotted paths (`ModuleA.utils.func`) to a hierarchical structure
- Used for **dependency tracking**: which globals does this file depend on?
- Self-contributions (names defined in this file) are **included**—they show this file references itself
- Local variables are **excluded**—only globals

### Tree Structure

The trie is a **rooted forest**:
- **Root level** (`roots` HashMap): Top-level names (`print`, `ModuleA`, `Foo`)
- **Child levels** (`children` HashMap): Nested fields (`ModuleA → utils → func`)
- **Leaf nodes**: Empty `children` map indicates "referenced here"

Example:
```
File references: ModuleA.utils.func, ModuleA.utils.abc, print

global_ref_tree.roots = {
    "ModuleA" → GlobalRefNode {
        children: {
            "utils" → GlobalRefNode {
                children: {
                    "func" → GlobalRefNode { children: {} },  // leaf
                    "abc" → GlobalRefNode { children: {} }    // leaf
                }
            }
        }
    },
    "print" → GlobalRefNode { children: {} }  // leaf
}
```

### Populating `global_ref_tree`

**Location**: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs` (lines 101-104, 143-299)

**Main Process** (`collect_external_refs` function):

1. Walk all `TypeFact` instances in the file's:
   - Scope declarations (local variable type facts)
   - Type definitions (`@class` fields, `@alias` types)
   - Function summaries (param & return types)
   - Module return type
   - Global contributions

2. Extract `GlobalRef` and `FieldOf` chains:
   ```rust
   // TypeFact::Stub(SymbolicStub::GlobalRef { name: "ModuleA.utils.func" })
   // → segments = ["ModuleA", "utils", "func"]
   global_out.insert(&segments);
   ```

3. For each extracted path, insert into trie using `GlobalRefTree::insert`:
   ```rust
   pub fn insert(&mut self, segments: &[String]) {
       if segments.is_empty() { return; }
       let mut node = self.roots.entry(segments[0].clone()).or_default();
       for seg in &segments[1..] {
           node = node.children.entry(seg.clone()).or_default();
       }
   }
   ```

### Consuming `global_ref_tree`

**Current Status**: Primarily used in **tests only** (lines 555-596 in `summary_builder/mod.rs`)

```rust
#[test]
fn global_ref_tree_simple_global() {
    let s = build_file_analysis(/* ... */).0;
    assert!(s.global_ref_tree.roots.contains_key("print"),
        "expected 'print' in global_ref_tree");
}

#[test]
fn global_ref_tree_dotted_path() {
    let module_a = s.global_ref_tree.roots.get("ModuleA")
        .and_then(|n| n.children.get("utils"));
}
```

**Planned Use** (per architecture docs `index-architecture.md`):
- Dependency tracking and **cascade invalidation**
- When a global name (e.g., `Mgr`) changes, identify which files reference it
- **Reverse dependency index**: `type_name → [URIs that reference it]`
- Similar to `TypeDependants` in `WorkspaceAggregation` (lines 29-30 of `aggregation.rs`)

**Current Production Use**: None identified in non-test code. The field is **pre-computed and serialized** but not actively queried. It appears to be infrastructure for future dependency-tracking features.

---

## 3. `function_name_index: HashMap<String, FunctionSummaryId>`

### Type Definition
**File**: `lsp/crates/mylua-lsp/src/summary.rs` (lines 71-75)

```rust
/// Reverse index: function name → FunctionSummaryId.
/// Only contains **global** functions. Colon-separated names are normalized
/// to dot (e.g. `"Player:new"` → `"Player.new"`).
/// Local functions are accessed via scope_tree → `FunctionRef(id)` instead.
pub function_name_index: HashMap<String, FunctionSummaryId>,
```

### Purpose
- **O(1) reverse lookup**: function name → `FunctionSummaryId`
- Only indexes **global functions** (not local functions, which use `ScopeTree` + `FunctionRef`)
- Enables fast signature resolution for cross-file function calls
- Names are **normalized**: colon-qualified names (`Player:new`) converted to dot form (`Player.new`)

### Key Design Points

1. **Global-only**: Local functions bypass this index
   ```
   local function foo() end  -- NOT in function_name_index
   function bar() end        -- IS in function_name_index
   ```

2. **Colon normalization**: Both methods and functions use the same index
   ```
   function Player:new() end  → "Player.new" → FunctionSummaryId(42)
   function Util.add(a, b) end → "Util.add" → FunctionSummaryId(43)
   ```

3. **Referencing**: Maps to `FunctionSummaryId`, which keys into `function_summaries: HashMap<FunctionSummaryId, FunctionSummary>`

### Populating `function_name_index`

**Location**: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs` (line 700)

```rust
// When a global function is encountered:
let normalized = name.replace(':', ".");
ctx.function_name_index.insert(normalized.clone(), func_id);
```

**BuildContext struct** (`summary_builder/mod.rs` lines 379-419):
```rust
pub(crate) struct BuildContext<'a> {
    pub(crate) function_name_to_id: HashMap<String, FunctionSummaryId>,
    /// Local mapping: function name → FunctionSummaryId (all functions).
    /// Used during type inference to resolve local function calls.
    
    pub(crate) function_name_index: HashMap<String, FunctionSummaryId>,
    /// Exported reverse index: global function name (colon→dot normalized) → FunctionSummaryId.
    /// Populated by visit_function_declaration for global functions only.
    /// Transferred to DocumentSummary::function_name_index at build completion.
}
```

Transfer to `DocumentSummary` (lines 86):
```rust
let mut summary = DocumentSummary {
    // ...
    function_name_index: ctx.function_name_index,  // transferred here
    // ...
};
```

### Consuming `function_name_index`

#### 1. **Lookup Method** (`DocumentSummary::get_function_by_name`)
**File**: `lsp/crates/mylua-lsp/src/summary.rs` (lines 246-250)

```rust
pub fn get_function_by_name(&self, name: &str) -> Option<&FunctionSummary> {
    let normalized = name.replace(':', ".");
    self.function_name_index.get(&normalized)
        .and_then(|id| self.function_summaries.get(id))
}
```

#### 2. **Type Inference** (`lsp/crates/mylua-lsp/src/type_inference.rs`)
Lines 303-323: When resolving function call return types:
```rust
// Try scope tree first for local functions, then function_name_index for globals.
let fs_data = index.summaries.get(uri).and_then(|summary| {
    // Local function via scope tree → FunctionRef(id)
    if let Some(KnownType::FunctionRef(fid)) = scope_tree.resolve_type(...) {
        // use local function
    }
    // Global function via function_name_index
    summary.get_function_by_name(callee_text).map(|fs| {
        (
            fs.generic_params.clone(),
            fs.signature.params.clone(),
            fs.signature.returns.clone(),
        )
    })
});
```

#### 3. **Signature Help** (`lsp/crates/mylua-lsp/src/signature_help.rs`)
Lines 120-126: When user hovers over function call:
```rust
// Global function via function_name_index (O(1)).
if let Some(fs) = summary.get_function_by_name(&name) {
    let sigs = primary_plus_overloads(fs);
    return Some((sigs, false, name));
}
```

#### 4. **Call Hierarchy** (`lsp/crates/mylua-lsp/src/call_hierarchy.rs`)
Lines 66, 82-86: When computing incoming/outgoing calls:
```rust
// Try scope tree first (handles local functions via FunctionRef(id)),
// then global function_name_index, then global_shard.
if let Some(fs) = summary.get_function_by_name(&name) {
    return vec![build_item(
        fs.name.clone(),
        SymbolKind::FUNCTION,
        uri.clone(),
        // ...
    )];
}
```

---

## Summary Table

| Field | Type | Size | Populated By | Consumed By | Purpose |
|-------|------|------|--------------|-------------|---------|
| `global_contributions` | `Vec<GlobalContribution>` | O(global names in file) | `visit_function_declaration`, `visit_assignment` | GlobalShard aggregation, Fingerprinting, Goto definition | Export all global names defined/extended by file to workspace |
| `global_ref_tree` | `GlobalRefTree` (trie) | O(external refs) | `collect_external_refs` (walks all TypeFacts) | Tests, Future cascade-invalidation | Track which external globals this file depends on |
| `function_name_index` | `HashMap<String, FunctionSummaryId>` | O(global functions) | `visit_function_declaration` | Type inference, Signature help, Call hierarchy, Goto | Fast global function lookup for cross-file queries |

---

## Data Flow Integration

```
Source File
    ↓
parse (tree-sitter)
    ↓
build_file_analysis
    ├─ visit_top_level
    │   ├─ visit_function_declaration → global_contributions + function_name_index
    │   ├─ visit_assignment → global_contributions
    │   └─ build_function_summary
    │       └─ function_summaries HashMap
    │
    └─ collect_external_refs (walks TypeFacts)
        └─ global_ref_tree (trie of external refs)

DocumentSummary
    ├─ global_contributions → (aggregation.rs) → GlobalShard
    ├─ global_ref_tree → (tests + future dependency tracking)
    └─ function_name_index → (type_inference, signature_help, call_hierarchy)
        ├─ resolve "Mgr.create" call at hover → FunctionSummaryId(X)
        ├─ lookup function_summaries[X] → FunctionSummary
        └─ extract signature + params → return to LSP client
```

---

## Architecture Alignment

Per `index-architecture.md` §2.1:

| Component | Status |
|-----------|--------|
| `GlobalContributions` | **Core**: feeds GlobalShard immediately during aggregation |
| `function_name_index` | **Core**: enables O(1) global function lookup for cross-file queries |
| `global_ref_tree` | **Infrastructure**: pre-computed for future cascade-invalidation; currently tests-only |

The three fields represent the **export interface** of a single file: what does it contribute to the workspace (`global_contributions`), what does it depend on (`global_ref_tree`), and how can other files look up its functions (`function_name_index`).

