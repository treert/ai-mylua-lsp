# Visual Reference: The Three Fields

## Quick Reference Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                      DocumentSummary                            │
│                    (per-file index)                             │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  1️⃣  global_contributions: Vec<GlobalContribution>            │
│      ├─ Function declarations (global only)                   │
│      ├─ Variable assignments (top-level)                      │
│      └─ Table extensions (e.g., Mgr.health = 100)            │
│      ↓                                                         │
│      → aggregation.rs: GlobalShard                           │
│      → goto.rs: module export fallback                       │
│      → fingerprint.rs: cascade invalidation                  │
│                                                                 │
│  2️⃣  global_ref_tree: GlobalRefTree (trie)                    │
│      └─ roots → children → children ...                       │
│      ├─ "print" (leaf)                                        │
│      ├─ "ModuleA"                                             │
│      │   └─ "utils"                                           │
│      │       ├─ "func" (leaf)                                 │
│      │       └─ "abc" (leaf)                                  │
│      ↓                                                         │
│      → Tests only (currently)                                 │
│      → Future: cascade-invalidation infrastructure           │
│                                                                 │
│  3️⃣  function_name_index: HashMap<String, FunctionSummaryId>  │
│      ├─ "add" → FunctionSummaryId(1)                          │
│      ├─ "Player.new" → FunctionSummaryId(2)                   │
│      └─ "Util.add" → FunctionSummaryId(3)                     │
│      ↓                                                         │
│      → type_inference.rs: resolve global function calls      │
│      → signature_help.rs: LSP hover info                      │
│      → call_hierarchy.rs: incoming/outgoing calls             │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## Lifecycle: From Source to LSP Response

```
File Edit
  ↓
[tree-sitter parse]
  ↓
build_file_analysis (single-file inference, ZERO cross-file deps)
  │
  ├─→ visit_top_level (AST traversal)
  │   │
  │   ├─→ visit_function_declaration("function Player:new() ... end")
  │   │   ├─ Create FunctionSummary (params, returns, signature)
  │   │ ╔═╪═══════════════════════════════════════════════════════╗
  │   │ ║ 3️⃣  function_name_index["Player.new"] = FunctionSummaryId(X)
  │   │ ║ 1️⃣  global_contributions.push(GlobalContribution {
  │   │ ║     name: "Player.new",
  │   │ ║     kind: Function,
  │   │ ║     type_fact: FunctionRef(X),
  │   │ ║     ...
  │   │ ║ })
  │   │ ╚═══════════════════════════════════════════════════════╝
  │   │
  │   ├─→ visit_assignment("Mgr = {}")
  │   │   │
  │   │   ├─ If base is NOT local:
  │   │   │ ╔═══════════════════════════════════════════════════════╗
  │   │   │ ║ 1️⃣  global_contributions.push(GlobalContribution {
  │   │   │ ║     name: "Mgr",
  │   │   │ ║     kind: Variable,
  │   │   │ ║     type_fact: Table(shape_id),
  │   │   │ ║     ...
  │   │   │ ║ })
  │   │   │ ╚═══════════════════════════════════════════════════════╝
  │   │
  │   └─→ visit_anonymous_functions
  │
  └─→ collect_external_refs (walk all TypeFacts)
      │
      ├─ Scope declarations (local type facts)
      ├─ Type definitions (@class fields)
      ├─ Function summaries (params, returns)
      ├─ Module return type
      ├─ Global contributions
      │
      └─ Extract GlobalRef + FieldOf chains
        ╔═══════════════════════════════════════════════════════╗
        ║ 2️⃣  global_ref_tree.insert(["ModuleA", "utils", "func"])
        ║     global_ref_tree.insert(["print"])
        ╚═══════════════════════════════════════════════════════╝

DocumentSummary created (serializable snapshot)
  ↓
WorkspaceAggregation::upsert_summary(uri, summary)
  │
  └─→ For each gc in summary.global_contributions:
      │
      └─→ GlobalShard.push_candidate(gc.name, GlobalCandidate {
          source_uri: uri,
          ...
          })
  ↓
Later: User hovers over "Player.new" call
  │
  ├─→ type_inference::infer_call_return_type
  │   │
  │   └─→ summary.get_function_by_name("Player.new")
  │       └─→ 3️⃣  function_name_index["Player.new"] → FunctionSummaryId(X)
  │           function_summaries[X] → FunctionSummary
  │           extract signature.returns
  │   
  └─→ LSP response: HoverInfo { type: "..." }
```

---

## Example: A Single File

**File**: `modules/player.lua`

```lua
---@class Player
Player = {}

---@param name string
---@return Player
function Player:new(name)
    return { name = name, health = 100 }
end

function add(x, y)
    return x + y
end

local function private_helper()
    return 42
end

---@type ModuleA
ModuleA = nil
```

**Resulting DocumentSummary**:

```rust
global_contributions: [
    GlobalContribution {
        name: "Player",
        kind: Variable,
        type_fact: TypeFact::Known(KnownType::EmmyType("Player")),
        range: ...,
        selection_range: ...,
    },
    GlobalContribution {
        name: "Player.new",
        kind: Function,
        type_fact: TypeFact::Known(KnownType::FunctionRef(FunctionSummaryId(0))),
        range: ...,
        selection_range: ...,
    },
    GlobalContribution {
        name: "add",
        kind: Function,
        type_fact: TypeFact::Known(KnownType::FunctionRef(FunctionSummaryId(1))),
        range: ...,
        selection_range: ...,
    },
    GlobalContribution {
        name: "ModuleA",
        kind: Variable,
        type_fact: TypeFact::Stub(SymbolicStub::TypeRef("ModuleA")),
        range: ...,
        selection_range: ...,
    },
    // Note: private_helper is NOT in global_contributions (it's local)
]

function_name_index: {
    "Player.new" → FunctionSummaryId(0),
    "add" → FunctionSummaryId(1),
    // Note: "private_helper" is NOT indexed (it's local)
}

global_ref_tree: {
    roots: {
        "ModuleA" → GlobalRefNode { children: {} },  // leaf - referenced in type annotation
    }
}

function_summaries: {
    FunctionSummaryId(0) → FunctionSummary {
        name: "Player.new",
        signature: FunctionSignature {
            params: [ParamInfo { name: "self", type_fact: ... },
                     ParamInfo { name: "name", type_fact: String }],
            returns: [TypeFact::Known(KnownType::EmmyType("Player"))],
        },
        emmy_annotated: true,
        ...
    },
    FunctionSummaryId(1) → FunctionSummary {
        name: "add",
        signature: FunctionSignature {
            params: [ParamInfo { name: "x", type_fact: Unknown },
                     ParamInfo { name: "y", type_fact: Unknown }],
            returns: [TypeFact::Stub(SymbolicStub::BinaryOp(...))],
        },
        emmy_annotated: false,
        ...
    },
}
```

---

## Consumption Scenarios

### Scenario 1: User hovers over `Player.new` call in another file

```
File: another_module.lua
  p = Player.new("Hero")
          ↑ hover here
  
  1. Query summaries["modules/player.lua"]
  2. Call summary.get_function_by_name("Player.new")
  3. 3️⃣  Look up function_name_index["Player.new"] → FunctionSummaryId(0)
  4. Return function_summaries[FunctionSummaryId(0)]
  5. Extract signature → params = [self, name: string]
  6. LSP: Show hover with signature
```

### Scenario 2: File `A` references `ModuleA`, file `B` defines `ModuleA`

```
File A (references):
  x = ModuleA.config
  
collect_external_refs(A) →
  2️⃣  global_ref_tree.insert(["ModuleA"])
  global_ref_tree.roots["ModuleA"] = leaf

File B (defines):
  ModuleA = { config = 123 }
  
global_contributions = [
    GlobalContribution { name: "ModuleA", ... }
]
  
Aggregation:
  1️⃣  GlobalShard.push_candidate("ModuleA", ...)

When File B is modified:
  → Signature fingerprint changes
  → Type dependants are marked dirty
  → (Future) Use 2️⃣  global_ref_tree to find all files that reference ModuleA
  → Revalidate those files
```

### Scenario 3: Signature Help on `add(1, 2)` call

```
File C:
  result = add(1, 2)
              ↑ inside function call

signature_help::collect_signatures_at
  1. Find "add" at this position
  2. Query summary = summaries["modules/player.lua"]
  3. 3️⃣  summary.get_function_by_name("add")
  4. Look up function_name_index["add"] → FunctionSummaryId(1)
  5. Get function_summaries[FunctionSummaryId(1)]
  6. Extract params: [(x, Unknown), (y, Unknown)]
  7. Extract returns: [Unknown]
  8. LSP: Show signature popup "add(x, y) → ?"
```

