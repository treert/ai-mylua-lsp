# Quick Reference: Three Fields in DocumentSummary

## 1️⃣ `global_contributions: Vec<GlobalContribution>`

| Property | Value |
|----------|-------|
| **Type** | `Vec<GlobalContribution>` |
| **What** | All global names exported by this file |
| **Contains** | Function declarations, variable assignments, table extensions |
| **Excludes** | Local variables |
| **Population** | `summary_builder/visitors.rs:700, 1056, 1147` |
| **Consumption** | aggregation.rs (GlobalShard), fingerprint.rs, goto.rs |
| **Status** | ✅ **Core component** — actively used |

### Quick Facts
- 3 kinds: `Variable`, `Function`, `TableExtension`
- Global functions get **normalized** names: `Player:new` → `Player.new`
- Each entry flows to `GlobalShard` for workspace-wide lookup
- Used for cascade invalidation fingerprinting

### Code Snippet
```rust
ctx.global_contributions.push(GlobalContribution {
    name: "Player.new",
    kind: GlobalContributionKind::Function,
    type_fact: TypeFact::Known(KnownType::FunctionRef(id)),
    range: ...,
    selection_range: ...,
});
```

---

## 2️⃣ `global_ref_tree: GlobalRefTree`

| Property | Value |
|----------|-------|
| **Type** | `GlobalRefTree` (trie structure) |
| **What** | External global names **referenced** by this file |
| **Structure** | `HashMap<String, GlobalRefNode>` → `children` → ... |
| **Example** | `ModuleA.utils.func` → trie path |
| **Population** | `summary_builder/mod.rs:101-104` via `collect_external_refs` |
| **Consumption** | Tests only (currently) |
| **Status** | ⏳ **Infrastructure** — pre-computed, future-ready |

### Quick Facts
- **Trie (prefix tree)** of dotted paths
- Leaf nodes have empty `children`
- Extracted from all `TypeFact` instances (`GlobalRef`, `FieldOf` chains)
- Self-contributions are **included** (file references itself)
- Local variables are **excluded**
- Planned for reverse-dependency cascade invalidation

### Code Snippet
```rust
pub struct GlobalRefTree {
    pub roots: HashMap<String, GlobalRefNode>,
}

// Insert: ["ModuleA", "utils", "func"]
// Result: roots["ModuleA"].children["utils"].children["func"]
tree.insert(&["ModuleA".into(), "utils".into(), "func".into()]);
```

---

## 3️⃣ `function_name_index: HashMap<String, FunctionSummaryId>`

| Property | Value |
|----------|-------|
| **Type** | `HashMap<String, FunctionSummaryId>` |
| **What** | Global function name → its ID (for O(1) lookup) |
| **Contains** | Global functions only |
| **Excludes** | Local functions |
| **Population** | `summary_builder/visitors.rs:700` during `visit_function_declaration` |
| **Consumption** | type_inference.rs, signature_help.rs, call_hierarchy.rs |
| **Status** | ✅ **Core component** — actively used |

### Quick Facts
- **O(1) reverse lookup**: name → `FunctionSummaryId`
- Names normalized: `Player:new` → `Player.new`
- Complements `function_summaries: HashMap<FunctionSummaryId, FunctionSummary>`
- Accessed via `DocumentSummary::get_function_by_name(name)`
- Local functions use `ScopeTree` + `FunctionRef` instead

### Code Snippet
```rust
let normalized = name.replace(':', ".");
ctx.function_name_index.insert(normalized, func_id);

// Later:
pub fn get_function_by_name(&self, name: &str) -> Option<&FunctionSummary> {
    let normalized = name.replace(':', ".");
    self.function_name_index.get(&normalized)
        .and_then(|id| self.function_summaries.get(id))
}
```

---

## Comparison Matrix

| Aspect | global_contributions | global_ref_tree | function_name_index |
|--------|----------------------|-----------------|---------------------|
| **Direction** | **Outbound** (what I export) | **Inbound** (what I use) | **Outbound** (how to find me) |
| **Data Structure** | Vec (ordered list) | Trie (prefix tree) | HashMap (fast lookup) |
| **Cardinality** | O(globals in file) | O(external refs) | O(global functions) |
| **Lookup Speed** | O(n) iteration | O(depth) tree path | O(1) hash |
| **Use Case** | Aggregation, fingerprint | Dependency graph | Function resolution |
| **Active?** | ✅ Yes | ⏳ Partial (tests) | ✅ Yes |

---

## Real Code Path Examples

### Example 1: Hover on Function Call
```
User hovers: p = Player.new("Hero")
                        ↑
→ signature_help.rs:123
→ summary.get_function_by_name("Player.new")
→ 3️⃣  function_name_index.get("Player.new") → FunctionSummaryId(X)
→ function_summaries[X] → FunctionSummary { signature: ... }
→ LSP: "function Player.new(name: string) → Player"
```

### Example 2: Build GlobalShard
```
aggregation.rs:436
for gc in &summary.global_contributions {
    global_shard.push_candidate(gc.name, GlobalCandidate {
        name: gc.name.clone(),      // e.g., "Player.new"
        type_fact: gc.type_fact,    // FunctionRef(id)
        source_uri: uri.clone(),    // which file
        ...
    });
}
```

### Example 3: Cascade Invalidation (Future)
```
File B changes: ModuleA = { ... }
→ Signature fingerprint changes
→ (Future) Use 2️⃣  global_ref_tree to find files that reference ModuleA
→ Revalidate those files
→ Currently: TypeDependants used instead (similar concept)
```

---

## Key Insights

1. **Separation of Concerns**
   - 1️⃣ What I **define** (contributions) → GlobalShard
   - 2️⃣ What I **use** (references) → dependency graph
   - 3️⃣ How to **find me** (function index) → fast lookup

2. **Global vs Local**
   - All three fields are **global-only**
   - Local functions/variables use `ScopeTree` instead
   - This separates workspace-level queries from scope-local queries

3. **Colon Normalization**
   - OO methods: `Player:new` = `Player.new` (dotted form)
   - Both 1️⃣ and 3️⃣ normalize for consistency
   - 2️⃣ normalizes when extracting references

4. **Current vs Planned**
   - 1️⃣ and 3️⃣ are **actively used** in LSP queries
   - 2️⃣ is **pre-computed and serialized** but not yet queried
   - Infrastructure for future reverse-dependency tracking

