# Scoped Type System — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merge ScopeTree and summary_builder into a single AST traversal, moving `local_type_facts` from `DocumentSummary` into scoped `ScopeDecl` with proper Lua scoping semantics.

**Architecture:** BuildContext gains a scope stack. All `local_type_facts.insert(...)` become `add_scoped_decl(...)`. All `local_type_facts.get(name)` become `resolve_in_build_scopes(name)`. Query-side consumers migrate from `summary.local_type_facts.get(name)` to `scope_tree.resolve_type(byte_offset, name)`. Two cross-file uses of `local_type_facts` get replaced by `TypeDefinition.anchor_shape_id`. After migration, `local_type_facts`, `LocalTypeFact`, and `TypeFactSource` are deleted, and `build_scope_tree` is removed.

**Tech Stack:** Rust, tower-lsp-server, tree-sitter

**Spec:** `docs/superpowers/specs/2026-04-26-scoped-type-system-design.md` — Phase 1 (§3)

---

### Task 1: Extend ScopeDecl with type_fact and bound_class fields

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/scope.rs:24-35` (ScopeDecl struct)

- [ ] **Step 1: Add `type_fact` and `bound_class` fields to `ScopeDecl`**

In `lsp/crates/mylua-lsp/src/scope.rs`, add two fields to `ScopeDecl` after `selection_range` (line 34):

```rust
#[derive(Debug, Clone)]
pub struct ScopeDecl {
    pub name: String,
    pub kind: DefKind,
    pub decl_byte: usize,
    /// Byte offset after which this declaration becomes visible.
    /// For parameters/for-variables this equals `decl_byte`.
    /// For `local` declarations this equals the statement's end byte,
    /// matching Lua semantics where `local x = x + 1` RHS sees the outer `x`.
    pub visible_after_byte: usize,
    pub range: ByteRange,
    pub selection_range: ByteRange,
    /// Inferred type of this declaration. `None` when the type is unknown
    /// (e.g. for-loop variables, some parameters).
    pub type_fact: Option<crate::type_system::TypeFact>,
    /// If this variable is the anchor for an `---@class` annotation,
    /// stores the class name. Used by Phase 2 class anchor binding.
    pub bound_class: Option<String>,
}
```

- [ ] **Step 2: Fix all existing ScopeDecl construction sites in `scope.rs`**

Every place in `scope.rs` that creates a `ScopeDecl` needs `type_fact: None, bound_class: None`. There are 5 sites in the `TreeBuilder` impl:

1. `collect_local_decl` → `collect_identifiers_as_decl` (line 334)
2. `collect_local_func_decl` (line 255)
3. `collect_parameters` — two branches (lines 273 and 286)
4. `collect_implicit_self` (line 309)
5. `for_numeric_statement` handler (line 193)
6. `for_generic_statement` handler (line 216)

For each, add `type_fact: None, bound_class: None` to the ScopeDecl literal.

Example for `collect_identifiers_as_decl` (line 334):
```rust
self.add_decl(scope_id, ScopeDecl {
    name: node_text(child, self.source).to_string(),
    kind: kind.clone(),
    decl_byte: child.start_byte(),
    visible_after_byte: visible_after,
    range: self.line_index.ts_node_to_byte_range(stmt_node, self.source),
    selection_range: self.line_index.ts_node_to_byte_range(child, self.source),
    type_fact: None,
    bound_class: None,
});
```

Apply the same pattern to all 6 sites.

- [ ] **Step 3: Add `resolve_type` and `resolve_bound_class` to ScopeTree**

After the existing `scope_byte_range_for_def` method (line 420), add:

```rust
    /// Resolve the type of a local variable at the given byte offset.
    /// Returns `None` if the name doesn't resolve or has no type.
    pub fn resolve_type(&self, byte_offset: usize, name: &str) -> Option<&crate::type_system::TypeFact> {
        self.resolve_decl(byte_offset, name)?
            .type_fact.as_ref()
    }

    /// Resolve the bound class of a local variable at the given byte offset.
    /// Returns `None` if the name doesn't resolve or has no class binding.
    pub fn resolve_bound_class(&self, byte_offset: usize, name: &str) -> Option<&str> {
        self.resolve_decl(byte_offset, name)?
            .bound_class.as_deref()
    }
```

- [ ] **Step 4: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All 461 tests pass. The new fields default to `None` so nothing changes yet.

- [ ] **Step 5: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/scope.rs && git commit -m "feat: add type_fact and bound_class fields to ScopeDecl"
```

---

### Task 2: Add scope stack to BuildContext with encapsulated methods

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/scope.rs:37-50` (make Scope fields pub, add ScopeTree::from_scopes)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs:127-174` (BuildContext struct + build_summary)

- [ ] **Step 1: Make Scope struct fields pub and add `ScopeTree::from_scopes`**

In `scope.rs`, the `Scope` struct (line 38) already has `pub` fields. The `ScopeTree` struct wraps `scopes: Vec<Scope>` as private. Add a constructor so BuildContext can produce a ScopeTree:

```rust
impl ScopeTree {
    /// Construct a ScopeTree from a pre-built scope vector.
    /// Used by `build_file_analysis` which builds scopes during summary construction.
    pub fn from_scopes(scopes: Vec<Scope>) -> Self {
        ScopeTree { scopes }
    }
}
```

Add this right before the existing `impl ScopeTree` block at line 352.

- [ ] **Step 2: Add scope stack fields and methods to BuildContext**

In `lsp/crates/mylua-lsp/src/summary_builder/mod.rs`, add `use crate::scope::{Scope, ScopeKind, ScopeDecl, ScopeTree};` to the imports (after line 16).

Add fields to `BuildContext` (after `module_return_range`, line 155):

```rust
    /// Scope stack for building the ScopeTree alongside the summary.
    pub(crate) scopes: Vec<Scope>,
    /// Stack of scope indices — top is the current innermost scope.
    pub(crate) scope_stack: Vec<usize>,
```

Initialize them in `build_summary` (after `module_return_range: None`, line 48):

```rust
        scopes: Vec::new(),
        scope_stack: Vec::new(),
```

Add scope management methods to the `impl BuildContext` block (after `take_pending_type`, line 173):

```rust
    /// Push a new scope onto the stack. Returns the scope id.
    pub(crate) fn push_scope(&mut self, kind: ScopeKind, start: usize, end: usize) -> usize {
        let id = self.scopes.len();
        let parent = self.scope_stack.last().copied();
        self.scopes.push(Scope {
            kind,
            byte_start: start,
            byte_end: end,
            parent,
            children: Vec::new(),
            declarations: Vec::new(),
        });
        if let Some(pid) = parent {
            self.scopes[pid].children.push(id);
        }
        self.scope_stack.push(id);
        id
    }

    /// Pop the current scope from the stack.
    pub(crate) fn pop_scope(&mut self) {
        self.scope_stack.pop();
    }

    /// Add a declaration to the current scope.
    pub(crate) fn add_scoped_decl(&mut self, decl: ScopeDecl) {
        if let Some(&scope_id) = self.scope_stack.last() {
            self.scopes[scope_id].declarations.push(decl);
        }
    }

    /// Resolve a name by walking the scope stack from innermost to outermost.
    /// This is the build-time equivalent of `ScopeTree::resolve_decl`.
    pub(crate) fn resolve_in_build_scopes(&self, name: &str) -> Option<&ScopeDecl> {
        for &scope_id in self.scope_stack.iter().rev() {
            let scope = &self.scopes[scope_id];
            // Find the latest matching declaration visible at "now"
            // (during build, all decls in the current scope that have been
            // added so far are visible — we're processing sequentially).
            let mut best: Option<&ScopeDecl> = None;
            for decl in &scope.declarations {
                if decl.name == name {
                    best = Some(decl);
                }
            }
            if best.is_some() {
                return best;
            }
        }
        None
    }

    /// Extract the built scopes into a ScopeTree, consuming the scope data.
    pub(crate) fn take_scope_tree(&mut self) -> ScopeTree {
        ScopeTree::from_scopes(std::mem::take(&mut self.scopes))
    }
```

- [ ] **Step 3: Build and verify**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build 2>&1`
Expected: Compiles. The new fields/methods exist but aren't used yet.

- [ ] **Step 4: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/scope.rs lsp/crates/mylua-lsp/src/summary_builder/mod.rs && git commit -m "feat: add scope stack to BuildContext with push/pop/resolve methods"
```

---

### Task 3: Add scope push/pop to visitors.rs + register declarations into scopes

This is the core merge task. The visitors already traverse all the right AST nodes — we add scope push/pop calls and dual-write declarations to both `local_type_facts` (for now) and the new scope stack.

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:16-63` (visit_top_level)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:93-125` (visit_nested_block)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:131-229` (visit_local_declaration)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:360-381` (visit_local_function)

- [ ] **Step 1: Add scope imports**

At the top of `visitors.rs`, add to the existing imports:

```rust
use crate::scope::{ScopeKind, ScopeDecl};
use crate::types::DefKind;
```

- [ ] **Step 2: Push/pop File scope in `visit_top_level`**

Wrap the body of `visit_top_level` (line 16) with File scope push/pop:

```rust
pub(super) fn visit_top_level(ctx: &mut BuildContext, root: tree_sitter::Node) {
    ctx.push_scope(ScopeKind::File, root.start_byte(), root.end_byte());

    let mut cursor = root.walk();
    if !cursor.goto_first_child() {
        ctx.pop_scope();
        return;
    }
    loop {
        // ... existing match arms unchanged ...
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    ctx.pop_scope();
}
```

- [ ] **Step 3: Push/pop block scopes in `visit_nested_block`**

Add scope push/pop based on node kind at the entry/exit of `visit_nested_block`:

```rust
fn visit_nested_block(ctx: &mut BuildContext, node: tree_sitter::Node) {
    // Push scope for block-level nodes
    let scope_kind = match node.kind() {
        "do_statement" => Some(ScopeKind::DoBlock),
        "while_statement" => Some(ScopeKind::WhileBlock),
        "repeat_statement" => Some(ScopeKind::RepeatBlock),
        "if_statement" | "if_clause" => Some(ScopeKind::IfThenBlock),
        "elseif_clause" => Some(ScopeKind::ElseIfBlock),
        "else_clause" => Some(ScopeKind::ElseBlock),
        "for_numeric_statement" => Some(ScopeKind::ForNumeric),
        "for_generic_statement" => Some(ScopeKind::ForGeneric),
        _ => None,
    };
    if let Some(kind) = scope_kind {
        ctx.push_scope(kind, node.start_byte(), node.end_byte());
    }

    // Register for-loop variables into scope
    match node.kind() {
        "for_numeric_statement" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let db = name_node.start_byte();
                ctx.add_scoped_decl(ScopeDecl {
                    name: node_text(name_node, ctx.source).to_string(),
                    kind: DefKind::ForVariable,
                    decl_byte: db,
                    visible_after_byte: db,
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
                    type_fact: None,
                    bound_class: None,
                });
            }
        }
        "for_generic_statement" => {
            if let Some(names_node) = node.child_by_field_name("names") {
                for i in 0..names_node.named_child_count() {
                    if let Some(id_node) = names_node.named_child(i as u32) {
                        if id_node.kind() == "identifier" {
                            let db = id_node.start_byte();
                            ctx.add_scoped_decl(ScopeDecl {
                                name: node_text(id_node, ctx.source).to_string(),
                                kind: DefKind::ForVariable,
                                decl_byte: db,
                                visible_after_byte: db,
                                range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                                selection_range: ctx.line_index.ts_node_to_byte_range(id_node, ctx.source),
                                type_fact: None,
                                bound_class: None,
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        if scope_kind.is_some() { ctx.pop_scope(); }
        return;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "block" | "if_clause" | "elseif_clause" | "else_clause"
            | "if_statement" | "do_statement" | "while_statement" | "repeat_statement"
            | "for_numeric_statement" | "for_generic_statement" => {
                visit_nested_block(ctx, child);
            }
            "function_declaration" => {
                visit_function_declaration(ctx, child);
            }
            "assignment_statement" => {
                visit_assignment(ctx, child);
            }
            "local_declaration" => {
                visit_local_declaration(ctx, child);
            }
            "local_function_declaration" => {
                visit_local_function(ctx, child);
            }
            "emmy_comment" => visit_emmy_comment(ctx, child),
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }

    if scope_kind.is_some() { ctx.pop_scope(); }
}
```

- [ ] **Step 4: Dual-write local declarations into scope**

In `visit_local_declaration`, at each place where `ctx.local_type_facts.insert(...)` is called (lines 170, 185, 200, 221), add a parallel `ctx.add_scoped_decl(...)` call **immediately after** the `local_type_facts.insert`.

Example for the first site (after line 175):

```rust
                ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
                    name: name.clone(),
                    type_fact: type_fact.clone(),
                    source: TypeFactSource::EmmyAnnotation,
                    range,
                });
                ctx.add_scoped_decl(ScopeDecl {
                    name: name.clone(),
                    kind: DefKind::LocalVariable,
                    decl_byte: name_node.start_byte(),
                    visible_after_byte: node.end_byte(),
                    range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
                    selection_range: range,
                    type_fact: Some(type_fact),
                    bound_class: None,
                });
                continue;
```

Note: `type_fact` needs to be cloned before inserting into `local_type_facts` since we also pass it to `add_scoped_decl`. Add `.clone()` to the type_fact in the `LocalTypeFact` construction where needed.

Apply the same dual-write pattern at all 4 `local_type_facts.insert` sites within `visit_local_declaration`. Each ScopeDecl should have:
- `kind: DefKind::LocalVariable`
- `decl_byte: name_node.start_byte()`
- `visible_after_byte: node.end_byte()` (the statement end, matching Lua semantics)
- `type_fact: Some(...)` with the same type_fact written to local_type_facts

- [ ] **Step 5: Dual-write local function into scope**

In `visit_local_function` (line 360), after the `ctx.local_type_facts.insert(...)` at line 375:

```rust
    ctx.add_scoped_decl(ScopeDecl {
        name: name.clone(),
        kind: DefKind::LocalFunction,
        decl_byte: name_node.start_byte(),
        visible_after_byte: name_node.start_byte(), // local functions visible immediately
        range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
        selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
        type_fact: Some(TypeFact::Known(KnownType::FunctionRef(func_id))),
        bound_class: None,
    });
```

- [ ] **Step 6: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. We're dual-writing — old path still works, new path builds scope data.

- [ ] **Step 7: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary_builder/visitors.rs && git commit -m "feat: dual-write local declarations into scope stack alongside local_type_facts"
```

---

### Task 4: Produce ScopeTree from build_file_analysis and wire up callers

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs:29-75` (build_summary → build_file_analysis)
- Modify: `lsp/crates/mylua-lsp/src/lib.rs:283,295,355,421,435`
- Modify: `lsp/crates/mylua-lsp/src/indexing.rs:288,307,311`

- [ ] **Step 1: Change `build_summary` to return `(DocumentSummary, ScopeTree)`**

In `lsp/crates/mylua-lsp/src/summary_builder/mod.rs`, rename `build_summary` to `build_file_analysis` and change the return type:

```rust
pub fn build_file_analysis(
    uri: &Uri,
    tree: &tree_sitter::Tree,
    source: &[u8],
    line_index: &LineIndex,
) -> (DocumentSummary, ScopeTree) {
```

At the end of the function, before the `DocumentSummary` construction, extract the scope tree:

```rust
    let scope_tree = ctx.take_scope_tree();

    let summary = DocumentSummary {
        // ... existing fields unchanged ...
    };

    (summary, scope_tree)
}
```

Keep the old name as a deprecated wrapper for now to reduce breakage:
```rust
/// Deprecated: use `build_file_analysis` which also returns a ScopeTree.
pub fn build_summary(uri: &Uri, tree: &tree_sitter::Tree, source: &[u8], line_index: &LineIndex) -> DocumentSummary {
    build_file_analysis(uri, tree, source, line_index).0
}
```

- [ ] **Step 2: Update `lib.rs` callers**

In `lib.rs`, replace paired `build_summary` + `build_scope_tree` calls with single `build_file_analysis` calls.

At the `did_change` main path (around line 295 + 355):
```rust
let (mut summary, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
```
Remove the separate `let scope_tree = scope::build_scope_tree(...)` at line 355.

At the fallback path (line 283) where only `build_scope_tree` is called (parsing failed, reuse old tree):
```rust
let (_, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
```
(Discard the summary since we're keeping the old document state.)

At `index_file_from_disk` (around lines 421 + 435): same pattern — replace the pair with one call.

- [ ] **Step 3: Update `indexing.rs` callers**

In `indexing.rs`, replace the two paths:

Fresh-parse path (around lines 307 + 311):
```rust
let (mut summary, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
```

Cache-hit path (around line 288): currently only calls `build_scope_tree`. Replace with:
```rust
let (_, scope_tree) = summary_builder::build_file_analysis(&uri, &tree, lua_source.source(), lua_source.line_index());
```

- [ ] **Step 4: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. The ScopeTree now comes from `build_file_analysis` but still has the same structure as before (the scope data from BuildContext mirrors what `build_scope_tree` produced for the top-level + nested blocks).

- [ ] **Step 5: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary_builder/mod.rs lsp/crates/mylua-lsp/src/lib.rs lsp/crates/mylua-lsp/src/indexing.rs && git commit -m "feat: build_file_analysis returns (DocumentSummary, ScopeTree), wire up all callers"
```

---

### Task 5: Switch build-time variable lookups from `local_type_facts` to `resolve_in_build_scopes`

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs:101,152,225,292,396`
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:420,797,812,912`

- [ ] **Step 1: Switch `infer_expression_type` (type_infer.rs:101)**

Replace:
```rust
            if let Some(fact) = ctx.local_type_facts.get(text) {
                return fact.type_fact.clone();
            }
```
With:
```rust
            if let Some(decl) = ctx.resolve_in_build_scopes(text) {
                if let Some(ref tf) = decl.type_fact {
                    return tf.clone();
                }
            }
```

- [ ] **Step 2: Switch all other `ctx.local_type_facts.get(...)` in type_infer.rs**

Apply the same pattern at lines 152, 225, 292, and 396. Each `ctx.local_type_facts.get(text)` → `ctx.resolve_in_build_scopes(text)`, then read `.type_fact` from the returned `ScopeDecl`.

- [ ] **Step 3: Switch `ctx.local_type_facts.get(...)` in visitors.rs**

At line 420 (`visit_function_declaration`, table shape lookup):
```rust
        if let Some(ltf) = ctx.local_type_facts.get(base_name) {
```
→
```rust
        if let Some(decl) = ctx.resolve_in_build_scopes(base_name) {
            if let Some(ref ltf_type) = decl.type_fact {
                if let TypeFact::Known(KnownType::Table(shape_id)) = ltf_type {
```
(Adjust the surrounding logic to match the new structure.)

At line 797 (`visit_assignment`, local check):
```rust
                if ctx.local_type_facts.contains_key(&chain.base_name) {
```
→
```rust
                if ctx.resolve_in_build_scopes(&chain.base_name).is_some() {
```

At line 812 (subscript_expression base lookup):
```rust
                    if let Some(ltf) = ctx.local_type_facts.get(base_text) {
                        if let TypeFact::Known(KnownType::Table(shape_id)) = &ltf.type_fact {
```
→
```rust
                    if let Some(decl) = ctx.resolve_in_build_scopes(base_text) {
                        if let Some(TypeFact::Known(KnownType::Table(shape_id))) = &decl.type_fact {
```

At line 912 (`register_nested_field_write`):
```rust
    let base_shape_id = match ctx.local_type_facts.get(base_name) {
        Some(ltf) => match &ltf.type_fact {
```
→
```rust
    let base_shape_id = match ctx.resolve_in_build_scopes(base_name) {
        Some(decl) => match &decl.type_fact {
            Some(TypeFact::Known(KnownType::Table(sid))) => *sid,
```

- [ ] **Step 4: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. Build-time lookups now use scope-aware resolution.

- [ ] **Step 5: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs lsp/crates/mylua-lsp/src/summary_builder/visitors.rs && git commit -m "refactor: switch build-time variable lookups to resolve_in_build_scopes"
```

---

### Task 6: Add `anchor_shape_id` to TypeDefinition and migrate cross-file consumers

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary.rs:134-158` (TypeDefinition struct)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/emmy_visitors.rs:13-26` (flush_pending_class)
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs:819-835,973-989`

- [ ] **Step 1: Add `anchor_shape_id` to TypeDefinition**

In `summary.rs`, add after `name_range` (line 157):

```rust
    /// When the `@class` anchors a local table (`local M = {}`), stores the
    /// shape ID so cross-file consumers can look up fields without going
    /// through `local_type_facts`. Populated by `flush_pending_class`.
    #[serde(default)]
    pub anchor_shape_id: Option<crate::table_shape::TableShapeId>,
```

Add `anchor_shape_id: None,` to every `TypeDefinition` literal in `emmy_visitors.rs` (3 sites: `flush_pending_class` line 15, `emit_pending_class_as_typedef` line 33, and the alias/enum push sites).

- [ ] **Step 2: Populate `anchor_shape_id` in `flush_pending_class`**

In `emmy_visitors.rs::flush_pending_class`, after pushing the TypeDefinition, check the `local_type_facts` (still available at this point) for a Table shape:

```rust
pub(super) fn flush_pending_class(ctx: &mut BuildContext, node: tree_sitter::Node) {
    if let Some((cname, parents, fields, generic_params, name_range)) = ctx.pending_class.take() {
        // Check if the anchor statement creates a local with a table shape.
        // This will be used by cross-file field resolution.
        let anchor_shape_id = detect_anchor_shape(ctx, node);

        ctx.type_definitions.push(TypeDefinition {
            name: cname,
            kind: TypeDefinitionKind::Class,
            parents,
            fields,
            alias_type: None,
            generic_params,
            range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
            name_range: Some(name_range),
            anchor_shape_id,
        });
    }
}
```

Add the helper function:
```rust
/// When the anchor node is a `local_declaration` whose first variable has
/// a Table shape type, return that shape ID. Cross-file consumers use this
/// to find methods written via `function Class:method()`.
fn detect_anchor_shape(ctx: &BuildContext, anchor_node: tree_sitter::Node) -> Option<crate::table_shape::TableShapeId> {
    if anchor_node.kind() != "local_declaration" && anchor_node.kind() != "assignment_statement" {
        return None;
    }
    // For local_declaration: get first name, look up in scope/local_type_facts
    let names = anchor_node.child_by_field_name("names")?;
    let first_name_node = names.named_child(0)?;
    if first_name_node.kind() != "identifier" {
        return None;
    }
    let name = crate::util::node_text(first_name_node, ctx.source);
    // During flush_pending_class, the local hasn't been visited yet (it's the
    // current node being flushed against). But the values side may already have
    // been partly processed. Check if the RHS is a table constructor by peeking
    // at values. For existing `local M = {}` patterns, the shape will be
    // allocated later by visit_local_declaration. So we record None for now and
    // backfill in a post-pass. (See Step 3.)
    None
}
```

Actually, the timing is tricky — `flush_pending_class` runs **before** `visit_local_declaration` processes the values. The shape doesn't exist yet. We need a post-pass: after `visit_top_level` completes, scan `type_definitions` and match class names against `local_type_facts` to backfill `anchor_shape_id`.

Add to `build_file_analysis`, after `visit_top_level` returns (in `mod.rs`):

```rust
    // Backfill anchor_shape_id: for each class whose anchor is a local with
    // a Table shape, record the shape ID on the TypeDefinition.
    backfill_anchor_shape_ids(&mut ctx);
```

With the helper:
```rust
fn backfill_anchor_shape_ids(ctx: &mut BuildContext) {
    // Build a name→shape_id map from local_type_facts (still available)
    let local_shapes: HashMap<String, TableShapeId> = ctx.local_type_facts.iter()
        .filter_map(|(name, ltf)| {
            if let TypeFact::Known(KnownType::Table(sid)) = &ltf.type_fact {
                Some((name.clone(), *sid))
            } else {
                None
            }
        })
        .collect();

    // Also check scope-registered decls for Table shapes
    let scope_shapes: HashMap<String, TableShapeId> = ctx.scopes.iter()
        .flat_map(|s| s.declarations.iter())
        .filter_map(|decl| {
            if let Some(TypeFact::Known(KnownType::Table(sid))) = &decl.type_fact {
                Some((decl.name.clone(), *sid))
            } else {
                None
            }
        })
        .collect();

    for td in &mut ctx.type_definitions {
        if td.kind != TypeDefinitionKind::Class || td.anchor_shape_id.is_some() {
            continue;
        }
        // Check if any local variable shares this class name or was declared
        // at the same anchor position
        if let Some(&sid) = local_shapes.get(&td.name).or_else(|| scope_shapes.get(&td.name)) {
            td.anchor_shape_id = Some(sid);
        }
    }
}
```

- [ ] **Step 3: Migrate resolver.rs cross-file consumers**

At line 819 (`resolve_emmy_field_with_visited`), replace the `local_type_facts` fallback:

```rust
                        // Fallback: when the class anchor is a local table,
                        // use anchor_shape_id to find the shape.
                        if let Some(shape_id) = td.anchor_shape_id {
                            if let Some(shape) = summary.table_shapes.get(&shape_id) {
                                if let Some(fi) = shape.fields.get(field) {
                                    lsp_log!(
                                        "[resolve_emmy_field] found '{}.{}' via anchor_shape_id fallback",
                                        type_name, field
                                    );
                                    return ResolvedType {
                                        type_fact: fi.type_fact.clone(),
                                        def_uri: Some(candidate.source_uri.clone()),
                                        def_range: fi.def_range,
                                    };
                                }
                            }
                        }
```

At line 973 (`collect_emmy_fields_recursive`), same pattern:

```rust
                        // Also collect fields from the anchor table shape.
                        if let Some(shape_id) = td.anchor_shape_id {
                            if let Some(shape) = summary.table_shapes.get(&shape_id) {
                                for (fname, fi) in &shape.fields {
                                    if !fields.iter().any(|f| f.name == *fname) {
                                        fields.push(FieldCompletion {
                                            name: fname.clone(),
                                            type_display: format!("{}", fi.type_fact),
                                            is_function: is_function_type(&fi.type_fact),
                                            def_uri: Some(candidate.source_uri.clone()),
                                            def_range: fi.def_range,
                                        });
                                    }
                                }
                            }
                        }
```

- [ ] **Step 4: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. Cross-file field resolution now uses `anchor_shape_id` instead of `local_type_facts`.

- [ ] **Step 5: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary.rs lsp/crates/mylua-lsp/src/summary_builder/emmy_visitors.rs lsp/crates/mylua-lsp/src/summary_builder/mod.rs lsp/crates/mylua-lsp/src/resolver.rs && git commit -m "feat: add anchor_shape_id to TypeDefinition, migrate cross-file consumers"
```

---

### Task 7: Migrate query-time consumers from `local_type_facts` to `scope_tree`

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/hover.rs:428-440`
- Modify: `lsp/crates/mylua-lsp/src/goto.rs:167-195`
- Modify: `lsp/crates/mylua-lsp/src/type_inference.rs:76,105`
- Modify: `lsp/crates/mylua-lsp/src/completion.rs:464-479`
- Modify: `lsp/crates/mylua-lsp/src/inlay_hint.rs:194`
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs:165-181`
- Modify: `lsp/crates/mylua-lsp/src/diagnostics/type_compat.rs:20,107`
- Modify: `lsp/crates/mylua-lsp/src/diagnostics/type_mismatch.rs:22,147`
- Modify: `lsp/crates/mylua-lsp/src/aggregation.rs:843`

This is a large but mechanical task. Each consumer switches from `summary.local_type_facts.get(name)` to `scope_tree.resolve_type(byte_offset, name)`. The tricky part is threading `scope_tree` (from `Document`) and `byte_offset` (from the AST node being inspected) to each call site.

- [ ] **Step 1: Migrate `resolver.rs::resolve_local_in_file` (line 175)**

This function is called by `hover.rs` and `goto.rs`. It needs to accept a `scope_tree` + `byte_offset` parameter instead of reading from the summary.

Change signature from:
```rust
pub fn resolve_local_in_file(uri: &Uri, local_name: &str, agg: &mut WorkspaceAggregation) -> ResolvedType
```
To:
```rust
pub fn resolve_local_in_file(uri: &Uri, local_name: &str, byte_offset: usize, scope_tree: &crate::scope::ScopeTree, agg: &mut WorkspaceAggregation) -> ResolvedType
```

Replace the body:
```rust
    let fact = match scope_tree.resolve_type(byte_offset, local_name) {
        Some(tf) => tf.clone(),
        None => return ResolvedType::unknown(),
    };
    resolve_type(&fact, agg)
```

Update all callers of `resolve_local_in_file` to pass `byte_offset` and `scope_tree`.

- [ ] **Step 2: Migrate `hover.rs::resolve_local_type_info` (line 428)**

Update to accept and pass `byte_offset` and `scope_tree`:

```rust
fn resolve_local_type_info(
    uri: &Uri,
    name: &str,
    byte_offset: usize,
    scope_tree: &crate::scope::ScopeTree,
    index: &mut WorkspaceAggregation,
) -> Option<String> {
    // FunctionRef hover fix: resolve to readable signature
    if let Some(type_fact) = scope_tree.resolve_type(byte_offset, name) {
        if let crate::type_system::TypeFact::Known(crate::type_system::KnownType::FunctionRef(id)) = type_fact {
            if let Some(summary) = index.summaries.get(uri) {
                if let Some(fs) = summary.function_summaries.get(id) {
                    return Some(format_signature(&fs.signature));
                }
            }
        }
    }

    let resolved = resolver::resolve_local_in_file(uri, name, byte_offset, scope_tree, index);
    let display = format_resolved_type(&resolved.type_fact);
    if display == "unknown" { None } else { Some(display) }
}
```

Update the caller at line 117 to pass `byte_offset` and `&doc.scope_tree`.

- [ ] **Step 3: Migrate `goto.rs::type_definition_for_local` (line 177)**

Replace `index.summaries.get(def_uri).and_then(|s| s.local_type_facts.get(local_name))` with a scope_tree lookup. Thread `scope_tree` through the call chain.

- [ ] **Step 4: Migrate `type_inference.rs` (lines 76, 105)**

These need `scope_tree` added to the `infer_node_type` function signature. Since `infer_node_type` is called from many places (hover, completion, signature_help, diagnostics), this requires threading `scope_tree: &ScopeTree` through the function signature and all callers.

Replace both `summary.local_type_facts.get(text)` sites with `scope_tree.resolve_type(node.start_byte(), text)`.

- [ ] **Step 5: Migrate `completion.rs` (line 473)**

Replace `summary.local_type_facts.get(name)` with a scope_tree lookup. Thread `scope_tree` to the `resolve_local_item` function.

- [ ] **Step 6: Migrate `inlay_hint.rs` (line 194)**

Replace `summary.local_type_facts.get(name)` with `scope_tree.resolve_type(byte_offset, name)`.

- [ ] **Step 7: Migrate `diagnostics/type_compat.rs` (lines 20, 107)**

Replace both `summary.local_type_facts.get(text)` with scope_tree lookups. Thread `scope_tree` to the functions.

- [ ] **Step 8: Migrate `diagnostics/type_mismatch.rs` (lines 22, 147)**

Line 22 iterates `summary.local_type_facts.values()` — replace with `scope_tree.all_declarations()` filtered to those with `type_fact.is_some()`.

Line 147 does `.get(name)` — replace with `scope_tree.resolve_type(byte_offset, name)`.

- [ ] **Step 9: Migrate `aggregation.rs` (line 843)**

Add `referenced_type_names: HashSet<String>` to `DocumentSummary`. Populate it in `build_file_analysis` after `visit_top_level`. Replace the `local_type_facts.values()` iteration in `collect_referenced_type_names` with `summary.referenced_type_names`.

- [ ] **Step 10: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. No consumer reads `local_type_facts` anymore.

- [ ] **Step 11: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add -A && git commit -m "refactor: migrate all query-time consumers from local_type_facts to scope_tree"
```

---

### Task 8: Extend function body traversal into scope tree

Currently `build_function_summary` calls `collect_return_types` for shallow return-type scanning. We need the scope tree to also cover function body internals (local declarations, nested blocks, parameters). **This must happen before Task 9 (deletion) because `collect_return_types` reads variables from the scope stack.**

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:457-556` (build_function_summary)

- [ ] **Step 1: Register parameters into the FunctionBody scope**

After `build_function_summary` builds the `FunctionSummary` and before it returns, if a `body` node is present, push a `FunctionBody` scope, register parameters, register implicit self (for colon methods), and then do a full `visit_nested_block` pass on the body.

Add a new function `visit_function_body`:

```rust
/// Push a FunctionBody scope, register parameters + implicit self, then
/// recursively visit the body's statements. This populates the scope tree
/// with function-internal declarations so query-time `resolve_type` works
/// for locals inside functions.
fn visit_function_body(
    ctx: &mut BuildContext,
    func_body: tree_sitter::Node,
    params: &[ParamInfo],
    is_method: bool,
    class_prefix: &str,
) {
    ctx.push_scope(ScopeKind::FunctionBody, func_body.start_byte(), func_body.end_byte());

    // Register parameters
    if let Some(param_list) = func_body.child_by_field_name("parameters") {
        register_params_into_scope(ctx, param_list, params);
    }

    // Register implicit self for colon methods
    if is_method {
        let db = func_body.start_byte();
        let self_type = if !class_prefix.is_empty() {
            // Look up the class prefix in scope to find its type
            ctx.resolve_in_build_scopes(class_prefix)
                .and_then(|decl| decl.type_fact.clone())
        } else {
            None
        };
        ctx.add_scoped_decl(ScopeDecl {
            name: "self".to_string(),
            kind: DefKind::Parameter,
            decl_byte: db,
            visible_after_byte: db,
            range: ctx.line_index.ts_node_to_byte_range(func_body, ctx.source),
            selection_range: ctx.line_index.ts_node_to_byte_range(func_body, ctx.source),
            type_fact: self_type,
            bound_class: None,
        });
    }

    // Recursively visit the body's statements
    visit_nested_block(ctx, func_body);

    ctx.pop_scope();
}

fn register_params_into_scope(ctx: &mut BuildContext, param_list: tree_sitter::Node, emmy_params: &[ParamInfo]) {
    for i in 0..param_list.child_count() {
        let Some(child) = param_list.child(i as u32) else { continue };
        match child.kind() {
            "identifier" => {
                let name = node_text(child, ctx.source).to_string();
                let type_fact = emmy_params.iter()
                    .find(|p| p.name == name)
                    .map(|p| p.type_fact.clone())
                    .filter(|tf| *tf != TypeFact::Unknown);
                let db = child.start_byte();
                ctx.add_scoped_decl(ScopeDecl {
                    name,
                    kind: DefKind::Parameter,
                    decl_byte: db,
                    visible_after_byte: db,
                    range: ctx.line_index.ts_node_to_byte_range(child, ctx.source),
                    selection_range: ctx.line_index.ts_node_to_byte_range(child, ctx.source),
                    type_fact,
                    bound_class: None,
                });
            }
            "name_list" => {
                for j in 0..child.named_child_count() {
                    if let Some(id) = child.named_child(j as u32) {
                        if id.kind() == "identifier" {
                            let name = node_text(id, ctx.source).to_string();
                            let type_fact = emmy_params.iter()
                                .find(|p| p.name == name)
                                .map(|p| p.type_fact.clone())
                                .filter(|tf| *tf != TypeFact::Unknown);
                            let db = id.start_byte();
                            ctx.add_scoped_decl(ScopeDecl {
                                name,
                                kind: DefKind::Parameter,
                                decl_byte: db,
                                visible_after_byte: db,
                                range: ctx.line_index.ts_node_to_byte_range(id, ctx.source),
                                selection_range: ctx.line_index.ts_node_to_byte_range(id, ctx.source),
                                type_fact,
                                bound_class: None,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
```

- [ ] **Step 2: Call `visit_function_body` from `visit_local_function` and `visit_function_declaration`**

In `visit_local_function`, after building the function summary, add:

```rust
    if let Some(body) = body {
        visit_function_body(ctx, body, &fs.signature.params, false, "");
    }
```

(Place this before the function summary is moved into `ctx.function_summaries`.)

In `visit_function_declaration`, similarly call `visit_function_body` with the appropriate `is_method` and `class_prefix` parameters derived from the function name.

- [ ] **Step 3: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. The scope tree now includes function body internals.

- [ ] **Step 4: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary_builder/visitors.rs && git commit -m "feat: extend scope tree traversal into function bodies with parameter registration"
```

---

### Task 9: Remove `local_type_facts` and delete `build_scope_tree`

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary.rs` (remove `local_type_facts` field, `LocalTypeFact`, `TypeFactSource`)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs` (remove `local_type_facts` from BuildContext)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs` (remove all `local_type_facts.insert` calls)
- Modify: `lsp/crates/mylua-lsp/src/scope.rs` (remove `TreeBuilder`, `build_scope_tree`)

- [ ] **Step 1: Remove `local_type_facts` from DocumentSummary**

In `summary.rs`, remove:
- The `local_type_facts` field from `DocumentSummary` (line 35)
- The `LocalTypeFact` struct (lines 183-189)
- The `TypeFactSource` enum (lines 192-199)

- [ ] **Step 2: Remove `local_type_facts` from BuildContext**

In `summary_builder/mod.rs`:
- Remove the `local_type_facts` field from `BuildContext` (line 141)
- Remove `local_type_facts: HashMap::new()` from initialization (line 39)
- Remove `local_type_facts: ctx.local_type_facts` from DocumentSummary construction (line 66)
- Remove the deprecated `build_summary` wrapper function

- [ ] **Step 3: Remove `local_type_facts.insert(...)` calls from visitors.rs**

Remove all `ctx.local_type_facts.insert(...)` calls that were dual-writing alongside `ctx.add_scoped_decl(...)`. There should be 5 sites:
- `visit_local_declaration` (4 sites: lines 170, 185, 200, 221)
- `visit_local_function` (1 site: line 375)

Also remove the `backfill_anchor_shape_ids` function's dependency on `local_type_facts` — it should use `ctx.scopes` only.

- [ ] **Step 4: Remove `build_scope_tree` and `TreeBuilder` from scope.rs**

Remove:
- The `TreeBuilder` struct (lines 68-72) and all its `impl` methods (lines 74-345)
- The `build_scope_tree` function (lines 56-66)

Keep: all data structures (`ScopeKind`, `ScopeDecl`, `Scope`, `ScopeTree`) and all query methods (`resolve`, `resolve_decl`, `visible_locals`, `all_declarations`, `scope_byte_range_for_def`, `resolve_type`, `resolve_bound_class`).

- [ ] **Step 5: Remove any remaining imports of deleted items**

Search for `build_scope_tree`, `LocalTypeFact`, `TypeFactSource`, `local_type_facts` across all files and remove stale references.

- [ ] **Step 6: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. `local_type_facts` is fully removed.

- [ ] **Step 7: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add -A && git commit -m "refactor: remove local_type_facts from DocumentSummary and delete build_scope_tree"
```

---

### Task 10: Final verification and cleanup

**Files:**
- All modified files

- [ ] **Step 1: Full build**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build 2>&1`
Expected: Zero errors, minimal warnings.

- [ ] **Step 2: Full test suite**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests 2>&1`
Expected: All 461+ tests pass.

- [ ] **Step 3: Verify no `local_type_facts` references remain**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && grep -rn 'local_type_facts' crates/mylua-lsp/src/ --include='*.rs' 2>&1`
Expected: Zero hits.

- [ ] **Step 4: Verify no `build_scope_tree` references remain**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && grep -rn 'build_scope_tree' crates/mylua-lsp/src/ --include='*.rs' 2>&1`
Expected: Zero hits (except possibly doc comments).

- [ ] **Step 5: Commit any cleanup fixes**

Only if issues found. Otherwise skip.

- [ ] **Step 6: Update docs**

Update `docs/index-architecture.md` to reflect:
- `DocumentSummary` no longer contains `local_type_facts`
- `build_file_analysis` replaces `build_summary` + `build_scope_tree`
- ScopeTree now carries type information

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add docs/ && git commit -m "docs: update architecture docs for scoped type system Phase 1"
```
