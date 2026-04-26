# Function Summary ID 化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the FunctionSummaryId refactoring — add `function_name_index` to DocumentSummary, emit `FunctionRef(id)` from summary_builder, handle `FunctionRef` in all consumers, and redirect local function lookups through `local_type_facts`.

**Architecture:** Phase 1 infrastructure is already in place (FunctionSummaryId type, KnownType::FunctionRef variant, ID-keyed function_summaries in DocumentSummary, BuildContext ID allocation). This plan completes Phase 2: exporting name→ID index, emitting FunctionRef from builders, handling FunctionRef in all consumer match arms, and removing the linear-scan `get_function_by_name`.

**Tech Stack:** Rust, tower-lsp-server, tree-sitter, serde

**Spec:** `docs/superpowers/specs/2026-04-26-function-summary-id-design.md`

---

### Task 1: Add `function_name_index` to DocumentSummary and export from builder

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary.rs:14-64` (DocumentSummary struct)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs:29-73` (build_summary function)
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs:125-150` (BuildContext struct)

- [ ] **Step 1: Add `function_name_index` field to `DocumentSummary`**

In `lsp/crates/mylua-lsp/src/summary.rs`, add after line 25 (`function_summaries`):

```rust
    /// Reverse index: function name → FunctionSummaryId.
    /// Only contains **global** functions. Colon-separated names are normalized
    /// to dot (e.g. `"Player:new"` → `"Player.new"`).
    /// Local functions are accessed via `local_type_facts` → `FunctionRef(id)` instead.
    #[serde(default)]
    pub function_name_index: HashMap<String, FunctionSummaryId>,
```

Add `FunctionSummaryId` to the existing import from `crate::type_system` on line 6:
```rust
use crate::type_system::{FunctionSignature, FunctionSummaryId, TypeFact};
```
(This import already exists — no change needed.)

- [ ] **Step 2: Add `function_name_index` to BuildContext**

In `lsp/crates/mylua-lsp/src/summary_builder/mod.rs`, add a new field to `BuildContext` after `function_name_to_id` (line 133):

```rust
    /// Exported reverse index: global function name (colon→dot normalized) → FunctionSummaryId.
    /// Populated by `visit_function_declaration` for global functions only.
    /// Transferred to `DocumentSummary::function_name_index` at build completion.
    pub(crate) function_name_index: HashMap<String, FunctionSummaryId>,
```

Initialize it in `build_summary` (after line 36):
```rust
        function_name_index: HashMap::new(),
```

Export it in the `DocumentSummary` construction (after line 62):
```rust
        function_name_index: ctx.function_name_index,
```

- [ ] **Step 3: Build and verify**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build 2>&1`
Expected: Compiles with zero errors. Warnings about unused field are OK at this stage.

- [ ] **Step 4: Run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests 2>&1`
Expected: All tests pass (function_name_index is empty but `#[serde(default)]` and existing `get_function_by_name` keep everything working).

- [ ] **Step 5: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary.rs lsp/crates/mylua-lsp/src/summary_builder/mod.rs && git commit -m "feat: add function_name_index to DocumentSummary and BuildContext"
```

---

### Task 2: Populate `function_name_index` and emit `FunctionRef(id)` from summary_builder

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:360-443` (visit_local_function, visit_function_declaration)

- [ ] **Step 1: Modify `visit_local_function` to write `local_type_facts`**

In `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`, replace `visit_local_function` (lines 360-372):

```rust
fn visit_local_function(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, ctx.source).to_string();
    let body = node.child_by_field_name("body");

    let fs = build_function_summary(ctx, &name, node, body);
    let func_id = ctx.alloc_function_id();
    ctx.function_name_to_id.insert(name.clone(), func_id);
    ctx.function_summaries.insert(func_id, fs);

    // Register in local_type_facts so consumers can discover this function
    // via type_inference → local_type_facts → FunctionRef(id) path.
    ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
        name: name.clone(),
        type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
        source: TypeFactSource::Assignment,
        range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    });
}
```

- [ ] **Step 2: Modify `visit_function_declaration` to populate `function_name_index` and emit `FunctionRef`**

In `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`, replace `visit_function_declaration` (lines 374-443):

```rust
fn visit_function_declaration(ctx: &mut BuildContext, node: tree_sitter::Node) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = node_text(name_node, ctx.source).to_string();
    let body = node.child_by_field_name("body");

    let fs = build_function_summary(ctx, &name, node, body);
    let sig_for_global = fs.signature.clone();
    let func_id = ctx.alloc_function_id();
    ctx.function_name_to_id.insert(name.clone(), func_id);
    ctx.function_summaries.insert(func_id, fs);

    // `function M.add(a, b)` / `function M:method()` — when the base is a
    // local with a Table shape, register the function as a field on that
    // shape (so `return M` carries the field through `require()`), and
    // skip global_contributions (M is local, not global).
    //
    // Only when the base is NOT a known local do we fall through to the
    // global contribution path — mirroring `visit_assignment`'s
    // `register_nested_field_write` → `continue` pattern.
    let wrote_to_shape = 'shape: {
        let (base_name, field_name) = if let Some((b, f)) = name.rsplit_once(':') {
            (b, f)
        } else if let Some((b, f)) = name.rsplit_once('.') {
            (b, f)
        } else {
            break 'shape false; // bare name, nothing to register
        };

        // Only single-segment bases (e.g. `M` in `M.add`). Multi-segment
        // bases like `a.b.c` would need nested shape walking which is
        // already handled by `register_nested_field_write` for assignments.
        if base_name.contains('.') || base_name.contains(':') {
            break 'shape false;
        }

        if let Some(ltf) = ctx.local_type_facts.get(base_name) {
            if let TypeFact::Known(KnownType::Table(shape_id)) = &ltf.type_fact {
                let sid = *shape_id;
                if let Some(shape) = ctx.table_shapes.get_mut(&sid) {
                    shape.set_field(field_name.to_string(), FieldInfo {
                        name: field_name.to_string(),
                        type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
                        def_range: Some(ctx.line_index.ts_node_to_byte_range(name_node, ctx.source)),
                        assignment_count: 1,
                    });
                    break 'shape true;
                }
            }
        }
        false
    };

    // Base is a local table → field already written to shape, no global.
    if wrote_to_shape {
        return;
    }

    // Base is not a local (or bare name) → register as global contribution
    // (e.g. `function Player.new()` where Player is a global).
    //
    // Write to function_name_index with colon→dot normalization.
    let normalized = name.replace(':', ".");
    ctx.function_name_index.insert(normalized, func_id);

    ctx.global_contributions.push(GlobalContribution {
        name: name.clone(),
        kind: GlobalContributionKind::Function,
        type_fact: TypeFact::Known(KnownType::FunctionRef(func_id)),
        range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
        selection_range: ctx.line_index.ts_node_to_byte_range(name_node, ctx.source),
    });
}
```

- [ ] **Step 3: Build and check for errors**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build 2>&1`
Expected: Compiles. Some tests may fail now because consumers see `FunctionRef(id)` where they expected `Function(sig)`.

- [ ] **Step 4: Run tests to identify failures**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests 2>&1`
Expected: Some failures in hover/completion/signature_help/diagnostics tests where `FunctionRef` is now flowing through but not yet handled. Note which tests fail — these guide Tasks 3-6.

- [ ] **Step 5: Commit (even with test failures — infrastructure commit)**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary_builder/visitors.rs && git commit -m "feat: emit FunctionRef(id) from summary_builder, populate function_name_index"
```

---

### Task 3: Handle `FunctionRef(id)` in resolver.rs

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs` (5+ locations)

The resolver has several places that match `KnownType::Function(sig)` to extract return types or check function-ness. Each needs a `FunctionRef(id)` sibling branch that resolves `(source_uri, id) → FunctionSummary → signature`.

- [ ] **Step 1: Add `FunctionRef` handling to `resolve_table_field` return type extraction (line ~476)**

Current code at line 476:
```rust
                    .and_then(|fi| {
                        if let TypeFact::Known(KnownType::Function(ref sig)) = fi.type_fact {
                            sig.returns.first().cloned()
                        } else {
                            None
                        }
                    })
```

Replace with:
```rust
                    .and_then(|fi| {
                        match &fi.type_fact {
                            TypeFact::Known(KnownType::Function(ref sig)) => {
                                sig.returns.first().cloned()
                            }
                            TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                                summary.function_summaries.get(fid)
                                    .and_then(|fs| fs.signature.returns.first().cloned())
                            }
                            _ => None,
                        }
                    })
```

Note: `summary` is already in scope from line 470 (`let summary = match agg.summaries.get(uri)`).

- [ ] **Step 2: Add `FunctionRef` handling to global_shard qualified lookup (line ~546)**

Current code at line 546:
```rust
            if let TypeFact::Known(KnownType::Function(ref sig)) = resolved.type_fact {
```

Replace with helper pattern. The `resolved` here comes from `resolve_recursive` which may resolve a `FunctionRef` into a `Function` or leave it. Add a `FunctionRef` branch:

```rust
            let ret = match &resolved.type_fact {
                TypeFact::Known(KnownType::Function(ref sig)) => {
                    sig.returns.first().cloned()
                }
                TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                    resolved.def_uri.as_ref()
                        .and_then(|uri| agg.summaries.get(uri))
                        .and_then(|s| s.function_summaries.get(fid))
                        .and_then(|fs| fs.signature.returns.first().cloned())
                }
                _ => None,
            };
            if let Some(ret) = ret {
                let mut ret_resolved = resolve_recursive(&ret, agg, depth + 1, visited);
                if ret_resolved.def_uri.is_none() {
                    ret_resolved.def_uri = Some(c.source_uri.clone());
                    ret_resolved.def_range = Some(c.selection_range);
                }
                return ret_resolved;
            }
```

- [ ] **Step 3: Add `FunctionRef` to `is_function_type` (line ~1013)**

Current code:
```rust
fn is_function_type(fact: &TypeFact) -> bool {
    match fact {
        TypeFact::Known(KnownType::Function(_))
        | TypeFact::Stub(SymbolicStub::CallReturn { .. }) => true,
        TypeFact::Union(types) => types.iter().any(is_function_type),
        _ => false,
    }
}
```

Replace:
```rust
fn is_function_type(fact: &TypeFact) -> bool {
    match fact {
        TypeFact::Known(KnownType::Function(_))
        | TypeFact::Known(KnownType::FunctionRef(_))
        | TypeFact::Stub(SymbolicStub::CallReturn { .. }) => true,
        TypeFact::Union(types) => types.iter().any(is_function_type),
        _ => false,
    }
}
```

- [ ] **Step 4: Add `FunctionRef` to `resolve_method_return_with_generics` global_shard fallback (line ~1095)**

Current code at line 1095:
```rust
        if let TypeFact::Known(KnownType::Function(ref sig)) = resolved.type_fact {
            if let Some(ret) = sig.returns.first() {
                return substitute_generics(ret, type_name, actual_params, agg);
            }
        }
```

Replace with:
```rust
        let ret = match &resolved.type_fact {
            TypeFact::Known(KnownType::Function(ref sig)) => {
                sig.returns.first()
            }
            TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                resolved.def_uri.as_ref()
                    .and_then(|uri| agg.summaries.get(uri))
                    .and_then(|s| s.function_summaries.get(fid))
                    .and_then(|fs| fs.signature.returns.first())
            }
            _ => None,
        };
        if let Some(ret) = ret {
            return substitute_generics(ret, type_name, actual_params, agg);
        }
```

- [ ] **Step 5: Add `FunctionRef` to `substitute_in_fact` (line ~1157)**

Current code at line 1157:
```rust
        TypeFact::Known(KnownType::Function(sig)) => {
            ...
            TypeFact::Known(KnownType::Function(crate::type_system::FunctionSignature {
                params,
                returns,
            }))
        }
```

Add after this arm (before the `_ => fact.clone()` default):
```rust
        TypeFact::Known(KnownType::FunctionRef(_)) => {
            // FunctionRef points to a summary; generic substitution doesn't
            // modify the stored summary, so pass through unchanged.
            fact.clone()
        }
```

- [ ] **Step 6: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: Closer to all-pass. Some consumer tests may still fail.

- [ ] **Step 7: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/resolver.rs && git commit -m "feat: handle FunctionRef(id) in resolver match arms"
```

---

### Task 4: Handle `FunctionRef(id)` in remaining consumer files

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/hover.rs:442-451`
- Modify: `lsp/crates/mylua-lsp/src/signature_help.rs` (lines 123, 135, 178, 278)
- Modify: `lsp/crates/mylua-lsp/src/type_inference.rs:235`
- Modify: `lsp/crates/mylua-lsp/src/aggregation.rs:816`
- Modify: `lsp/crates/mylua-lsp/src/diagnostics/type_compat.rs` (lines 57, 78, 85, 86)
- Modify: `lsp/crates/mylua-lsp/src/workspace_symbol.rs:58`
- Modify: `lsp/crates/mylua-lsp/src/inlay_hint.rs:220`

- [ ] **Step 1: `hover.rs` — `format_resolved_type` (line ~447)**

Current:
```rust
    if let TypeFact::Known(crate::type_system::KnownType::Function(sig)) = fact {
        return format_signature(sig);
    }
```

Replace:
```rust
    match fact {
        TypeFact::Known(crate::type_system::KnownType::Function(sig)) => {
            return format_signature(sig);
        }
        TypeFact::Known(crate::type_system::KnownType::FunctionRef(_)) => {
            // FunctionRef display: the caller should have resolved this to a
            // full signature before reaching here. Fall through to Display.
        }
        _ => {}
    }
```

- [ ] **Step 2: `signature_help.rs` — add `FunctionRef` matching at lines 123, 135, 178, 278**

At line 123:
```rust
    if let TypeFact::Known(KnownType::Function(ref sig)) = resolved.type_fact {
        return Some((vec![sig.clone()], false, name));
    }
```

Replace:
```rust
    match &resolved.type_fact {
        TypeFact::Known(KnownType::Function(ref sig)) => {
            return Some((vec![sig.clone()], false, name));
        }
        TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
            if let Some(uri) = &resolved.def_uri {
                if let Some(summary) = index.summaries.get(uri) {
                    if let Some(fs) = summary.function_summaries.get(fid) {
                        let sigs = primary_plus_overloads(fs);
                        return Some((sigs, false, name));
                    }
                }
            }
        }
        _ => {}
    }
```

At line 135:
```rust
        if let TypeFact::Known(KnownType::Function(ref sig)) = c.type_fact {
            return Some((vec![sig.clone()], false, name));
        }
```

Replace:
```rust
        match &c.type_fact {
            TypeFact::Known(KnownType::Function(ref sig)) => {
                return Some((vec![sig.clone()], false, name));
            }
            TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                if let Some(summary) = index.summaries.get(&c.source_uri) {
                    if let Some(fs) = summary.function_summaries.get(fid) {
                        let sigs = primary_plus_overloads(fs);
                        return Some((sigs, false, name));
                    }
                }
            }
            _ => {}
        }
```

At line 178 (`lookup_function_signatures_by_field`):
```rust
    if let TypeFact::Known(KnownType::Function(sig)) = &resolved.type_fact {
```

Replace:
```rust
    match &resolved.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => {
```
Then wrap the existing `if let Some(def_uri)` block inside this arm and add a `FunctionRef` arm:
```rust
        TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
            if let Some(uri) = &resolved.def_uri {
                if let Some(summary) = index.summaries.get(uri) {
                    if let Some(fs) = summary.function_summaries.get(fid) {
                        return primary_plus_overloads(fs);
                    }
                }
            }
            // Fall through to single-sig from inline
        }
        _ => {}
    }
```

At line 278:
```rust
        if let TypeFact::Known(KnownType::Function(sig)) = candidate.type_fact {
            return Some((candidate.source_uri.clone(), vec![sig]));
        }
```

Replace:
```rust
        match candidate.type_fact {
            TypeFact::Known(KnownType::Function(sig)) => {
                return Some((candidate.source_uri.clone(), vec![sig]));
            }
            TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                if let Some(summary) = index.summaries.get(&candidate.source_uri) {
                    if let Some(fs) = summary.function_summaries.get(fid) {
                        return Some((candidate.source_uri.clone(), primary_plus_overloads(fs)));
                    }
                }
            }
            _ => {}
        }
```

- [ ] **Step 3: `type_inference.rs` — add `FunctionRef` at line 235**

Current:
```rust
            if let TypeFact::Known(KnownType::Function(ref sig)) = field_result.type_fact {
                if let Some(ret) = sig.returns.first() {
                    return ret.clone();
                }
            }
```

Replace:
```rust
            match &field_result.type_fact {
                TypeFact::Known(KnownType::Function(ref sig)) => {
                    if let Some(ret) = sig.returns.first() {
                        return ret.clone();
                    }
                }
                TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                    if let Some(def_uri) = &field_result.def_uri {
                        if let Some(summary) = index.summaries.get(def_uri) {
                            if let Some(fs) = summary.function_summaries.get(fid) {
                                if let Some(ret) = fs.signature.returns.first() {
                                    return ret.clone();
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
```

- [ ] **Step 4: `aggregation.rs` — add `FunctionRef` to `walk` in `collect_type_names` (line ~816)**

Current:
```rust
            TypeFact::Known(KnownType::Function(sig)) => {
                for p in &sig.params {
                    walk(&p.type_fact, out);
                }
                for r in &sig.returns {
                    walk(r, out);
                }
            }
```

Add after this arm:
```rust
            TypeFact::Known(KnownType::FunctionRef(_)) => {
                // FunctionRef is an opaque ID; the signature's type names are
                // already collected when iterating function_summaries directly.
            }
```

- [ ] **Step 5: `diagnostics/type_compat.rs` — add `FunctionRef` to compatibility checks**

At line 57, add:
```rust
        ("function", KnownType::FunctionRef(_)) => true,
```

At line 78, replace:
```rust
        (KnownType::Function(_), KnownType::Function(_)) => true,
```
with:
```rust
        (KnownType::Function(_) | KnownType::FunctionRef(_), KnownType::Function(_) | KnownType::FunctionRef(_)) => true,
```

At line 85, replace:
```rust
        (KnownType::Table(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Function(_)) => false,
        (KnownType::Function(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Table(_)) => false,
```
with:
```rust
        (KnownType::Table(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Function(_) | KnownType::FunctionRef(_)) => false,
        (KnownType::Function(_) | KnownType::FunctionRef(_), KnownType::String | KnownType::Number | KnownType::Boolean | KnownType::Table(_)) => false,
```

- [ ] **Step 6: `workspace_symbol.rs` — add `FunctionRef` at line 58**

Current:
```rust
                && matches!(candidate.type_fact, TypeFact::Known(KnownType::Function(_)))
```

Replace:
```rust
                && matches!(candidate.type_fact, TypeFact::Known(KnownType::Function(_) | KnownType::FunctionRef(_)))
```

- [ ] **Step 7: `inlay_hint.rs` — add `FunctionRef` at line 220**

Current:
```rust
        TypeFact::Known(KnownType::Function(_)) => false,
```

Add after:
```rust
        TypeFact::Known(KnownType::FunctionRef(_)) => false,
```

- [ ] **Step 8: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: Closer to all-pass. Inspect remaining failures.

- [ ] **Step 9: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/hover.rs lsp/crates/mylua-lsp/src/signature_help.rs lsp/crates/mylua-lsp/src/type_inference.rs lsp/crates/mylua-lsp/src/aggregation.rs lsp/crates/mylua-lsp/src/diagnostics/type_compat.rs lsp/crates/mylua-lsp/src/workspace_symbol.rs lsp/crates/mylua-lsp/src/inlay_hint.rs && git commit -m "feat: handle FunctionRef(id) in all consumer match arms"
```

---

### Task 5: Replace `get_function_by_name` linear scan with `function_name_index` O(1) lookup

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary.rs:195-225` (remove/refactor helper methods)
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs` (lines 515, 522, 1063-1064, 1072)
- Modify: `lsp/crates/mylua-lsp/src/hover.rs:224`
- Modify: `lsp/crates/mylua-lsp/src/completion.rs:440`
- Modify: `lsp/crates/mylua-lsp/src/signature_help.rs` (lines 112, 130, 187, 274)
- Modify: `lsp/crates/mylua-lsp/src/call_hierarchy.rs` (lines 70, 251, 334, 364)
- Modify: `lsp/crates/mylua-lsp/src/type_inference.rs:283`

- [ ] **Step 1: Replace `get_function_by_name` with `get_function_by_name_indexed` on `DocumentSummary`**

In `lsp/crates/mylua-lsp/src/summary.rs`, replace the `get_function_by_name` method (lines 202-206):

```rust
    /// Look up a function summary by name using `function_name_index` (O(1)).
    /// Falls back to linear scan when the name isn't in the index (e.g. the
    /// caller passes a colon-qualified name that wasn't normalized).
    pub fn get_function_by_name(&self, name: &str) -> Option<&FunctionSummary> {
        // O(1) path: try the normalized (dot) form in the index first.
        let normalized = name.replace(':', ".");
        if let Some(id) = self.function_name_index.get(&normalized) {
            return self.function_summaries.get(id);
        }
        // Fallback: linear scan for non-indexed entries (local functions
        // during query time when called from call_hierarchy etc.).
        self.function_summaries
            .values()
            .find(|fs| fs.name == name)
    }
```

- [ ] **Step 2: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass. The `get_function_by_name` interface is preserved but now uses O(1) lookup for global functions.

- [ ] **Step 3: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary.rs && git commit -m "perf: use function_name_index for O(1) function lookup by name"
```

---

### Task 6: Handle `FunctionRef` in summary_builder/type_infer.rs table shape field lookups

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs` (lines 239, 298)

These are locations inside `infer_call_return_type` where table shape fields that store `Function(sig)` are pattern-matched. Now that `visit_function_declaration` writes `FunctionRef(id)` to shape fields, these need updating.

- [ ] **Step 1: Update method call shape field lookup (line ~239)**

Current:
```rust
                            if let TypeFact::Known(KnownType::Function(ref sig)) = fi.type_fact {
                                if let Some(ret) = sig.returns.first() {
                                    return ret.clone();
                                }
                            }
```

Replace:
```rust
                            match &fi.type_fact {
                                TypeFact::Known(KnownType::Function(ref sig)) => {
                                    if let Some(ret) = sig.returns.first() {
                                        return ret.clone();
                                    }
                                }
                                TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                                    if let Some(fs) = ctx.function_summaries.get(fid) {
                                        if let Some(ret) = fs.signature.returns.first() {
                                            return ret.clone();
                                        }
                                    }
                                }
                                _ => {}
                            }
```

- [ ] **Step 2: Update dot-call shape field lookup (line ~298)**

Current:
```rust
                                    if let TypeFact::Known(KnownType::Function(ref sig)) = fi.type_fact {
                                        if let Some(ret) = sig.returns.first() {
                                            return ret.clone();
                                        }
                                    }
```

Replace:
```rust
                                    match &fi.type_fact {
                                        TypeFact::Known(KnownType::Function(ref sig)) => {
                                            if let Some(ret) = sig.returns.first() {
                                                return ret.clone();
                                            }
                                        }
                                        TypeFact::Known(KnownType::FunctionRef(ref fid)) => {
                                            if let Some(fs) = ctx.function_summaries.get(fid) {
                                                if let Some(ret) = fs.signature.returns.first() {
                                                    return ret.clone();
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
```

- [ ] **Step 3: Build and run tests**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build && cargo test --tests 2>&1`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs && git commit -m "feat: handle FunctionRef in summary_builder type_infer shape field lookups"
```

---

### Task 7: Final verification and cleanup

**Files:**
- All modified files

- [ ] **Step 1: Full build**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build 2>&1`
Expected: Zero errors, zero new warnings.

- [ ] **Step 2: Full test suite**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests 2>&1`
Expected: All 460+ tests pass.

- [ ] **Step 3: Verify no direct `function_summaries.get(string_name)` remains in consumer code**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && grep -rn 'function_summaries\.get(' crates/mylua-lsp/src/ --include='*.rs' | grep -v 'function_summaries\.get(&func_id)\|function_summaries\.get(id)\|function_summaries\.get(fid)\|function_summaries\.get(&id)' 2>&1`

Expected: Only hits in `summary.rs` (the `get_function_by_name` fallback linear scan), `summary_builder/` (build-phase internal lookups using `function_name_to_id`-derived IDs), and `summary_builder/fingerprint.rs`.

- [ ] **Step 4: Verify `FunctionRef` is matched everywhere `Function` is**

Run: `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && grep -rn 'KnownType::Function(' crates/mylua-lsp/src/ --include='*.rs' | grep -v 'FunctionRef\|emmy\.rs\|type_infer\.rs:87\|type_compat\.rs:15\|type_compat\.rs:125\|type_system\.rs' 2>&1`

Expected: Every remaining `KnownType::Function(` either has a corresponding `FunctionRef` handler nearby or is in a context where only inline Emmy function types appear (emmy.rs, type_compat literal inference).

- [ ] **Step 5: Commit any fixes from Steps 3-4**

Only if issues found. Otherwise skip.

- [ ] **Step 6: Update future-work.md**

In `docs/future-work.md`, add after the §1.7 section header:

```markdown
**✅ 已完成于 2026-04-26**
```

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp && git add docs/future-work.md && git commit -m "docs: mark future-work §1.7 as completed"
```
