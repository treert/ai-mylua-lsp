# Field Analysis Documentation

This directory contains comprehensive analysis of three critical fields in the MyLua LSP codebase:

1. **`global_contributions`** — Global names exported by a file
2. **`global_ref_tree`** — External globals referenced by a file (trie structure)
3. **`function_name_index`** — Fast lookup for global functions

---

## 📚 Documentation Files

### 1. **FIELD_QUICK_REFERENCE.md** ⭐ START HERE
- **Purpose**: One-page quick lookup
- **Contents**: 
  - 3 tables comparing all three fields
  - Key facts and code snippets
  - Real code path examples
  - Insights and use cases
- **Best for**: Getting oriented quickly, understanding differences

### 2. **FIELD_ANALYSIS.md** 📖 DEEP DIVE
- **Purpose**: Comprehensive technical reference
- **Contents**:
  - Type definitions with full struct signatures
  - Detailed population/consumption explanation
  - 3 concrete examples of how data flows
  - Architecture alignment with index-architecture.md
  - Summary table with all facts
- **Best for**: Understanding internals, writing code that uses these fields

### 3. **FIELD_VISUAL_SUMMARY.md** 🎨 VISUAL GUIDE
- **Purpose**: Diagrams and flow charts
- **Contents**:
  - ASCII diagrams of data structures
  - Lifecycle from source file to LSP response
  - Real example with `player.lua`
  - 3 consumption scenarios with trace paths
- **Best for**: Visual learners, understanding relationships

### 4. **FIELD_SOURCES_INDEX.md** 🔍 SOURCE CODE INDEX
- **Purpose**: Complete line-by-line reference
- **Contents**:
  - Quick navigation table (file:line)
  - All type definitions with line numbers
  - Population code at exact line numbers
  - Consumption code at exact line numbers
  - Integration points and flow
  - Key files at a glance table
- **Best for**: Navigating codebase, finding exact locations

---

## 🎯 Quick Navigation by Task

**I want to...** → **Read this file:**

- Understand what these fields are → `FIELD_QUICK_REFERENCE.md`
- See the complete picture → `FIELD_ANALYSIS.md`
- Find code locations → `FIELD_SOURCES_INDEX.md`
- Visualize data structures → `FIELD_VISUAL_SUMMARY.md`
- Understand dependencies → `FIELD_ANALYSIS.md` § Architecture Alignment
- Write code that uses them → `FIELD_QUICK_REFERENCE.md` + `FIELD_SOURCES_INDEX.md`
- Debug a query → `FIELD_VISUAL_SUMMARY.md` § Consumption Scenarios

---

## 🔑 Key Insights (TL;DR)

### What Each Field Does

| Field | Type | Direction | Use |
|-------|------|-----------|-----|
| `global_contributions` | `Vec<GlobalContribution>` | ⬆️ Export | What I define for others to use |
| `global_ref_tree` | Trie structure | ⬇️ Import | What external globals I depend on |
| `function_name_index` | `HashMap<Name → ID>` | ⬆️ Export | How others can find my functions |

### Population

- **`global_contributions`**: Populated by `visit_function_declaration` + `visit_assignment` during AST traversal
  - 3 kinds: Variable, Function, TableExtension
  - Colon names normalized: `Player:new` → `Player.new`

- **`global_ref_tree`**: Populated by `collect_external_refs` which walks all TypeFacts
  - Trie-based prefix tree
  - Extracts GlobalRef + FieldOf chains

- **`function_name_index`**: Populated by `visit_function_declaration` (same place as global_contributions)
  - Maps normalized function names → FunctionSummaryId

### Consumption

- **`global_contributions`**: 
  - ✅ Active: Aggregation (GlobalShard), fingerprinting, goto definition
  
- **`global_ref_tree`**:
  - ⏳ Infrastructure: Currently tests only; future cascade-invalidation

- **`function_name_index`**:
  - ✅ Active: Type inference, signature help, call hierarchy

---

## 🗺️ Related Architecture

These fields are part of the **two-layer type inference** system:

1. **Single-file layer**: DocumentSummary (these three fields + others)
   - Zero cross-file dependencies
   - Generated in `build_file_analysis`

2. **Cross-file layer**: WorkspaceAggregation
   - GlobalShard built from `global_contributions`
   - Enables workspace-wide queries

See `docs/index-architecture.md` for the full system design.

---

## 📊 Size & Performance

| Field | Cardinality | Lookup | Population |
|-------|-------------|--------|------------|
| `global_contributions` | O(globals in file) | O(n) iteration | O(1) per global |
| `global_ref_tree` | O(external refs) | O(depth) tree walk | O(depth) per ref |
| `function_name_index` | O(global functions) | O(1) hash | O(1) per function |

Typical file: 50-200 globals, 5-20 global functions, 100+ external refs

---

## 🔗 Code Locations Quick Lookup

| What | Where |
|------|-------|
| Type defs | `summary.rs:9-52, 155-163` |
| `get_function_by_name()` | `summary.rs:246-250` |
| `build_file_analysis` | `summary_builder/mod.rs:30` |
| `visit_function_declaration` | `summary_builder/visitors.rs:~650` |
| `visit_assignment` | `summary_builder/visitors.rs:~1000` |
| `collect_external_refs` | `summary_builder/mod.rs:143` |
| Aggregation | `aggregation.rs:436, 514` |
| Type inference | `type_inference.rs:303` |
| Signature help | `signature_help.rs:123` |

---

## ✅ Understanding Checklist

After reading these docs, you should understand:

- [ ] What each field represents
- [ ] How to find where each is populated
- [ ] How to find where each is consumed
- [ ] The difference between global and local (why only globals here)
- [ ] Why `function_name_index` normalizes colon to dot
- [ ] What GlobalRefTree is and why it's a trie
- [ ] How `global_contributions` flows to GlobalShard
- [ ] Why `global_ref_tree` is infrastructure (pre-computed but not yet queried)
- [ ] Real code paths for hover/goto/signature-help queries
- [ ] The two-layer inference system

---

## 📝 Notes

- All three fields are part of **DocumentSummary** (per-file index)
- They represent **the export interface** of a single file
- Local functions/variables use **ScopeTree** instead (separate system)
- The three fields are **serializable** (Serialize/Deserialize traits)
- **Zero cross-file dependencies** during population (enables parallel parsing)

---

## 🆘 Need Help?

1. Start with: `FIELD_QUICK_REFERENCE.md`
2. For details: `FIELD_ANALYSIS.md`
3. For locations: `FIELD_SOURCES_INDEX.md`
4. For visualization: `FIELD_VISUAL_SUMMARY.md`
5. For codebase context: `docs/index-architecture.md` (project docs)

