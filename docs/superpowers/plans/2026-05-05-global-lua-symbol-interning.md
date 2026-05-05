# Global LuaSymbol Interning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce large-workspace RSS by replacing duplicated long-lived Lua/index strings with compact interned `LuaSymbol` keys.

**Architecture:** Add a `LuaSymbol(Spur)` newtype over a process-global `lasso::ThreadedRodeo`, following the existing `UriId` pattern: the rodeo stays private and all callers go through helper functions. Migrate only long-lived structures (`WorkspaceAggregation`, `DocumentSummary`, `ScopeTree`, `TypeFact`, `TableShape`) to `LuaSymbol`; keep request-local UI strings such as hover markdown, diagnostic messages, completion labels, and signature labels as `String`.

**Tech Stack:** Rust, lasso `ThreadedRodeo`, serde `Serialize`, Cargo integration tests, `lua-perf --summary`

---

## Execution Rules

- Execute tasks serially. These tasks touch the same data model and should not be implemented in parallel.
- Do not run `cargo fmt`, `rustfmt`, IDE format, or bulk formatting scripts.
- Keep LSP protocol boundaries string-facing. Convert `LuaSymbol` back to `&str` / `String` only when building LSP responses, logs, debug JSON, or user-visible text.
- Do not intern request-local assembled strings: hover markdown, diagnostic message bodies, completion `label`/`detail`, signature help labels, index status messages, CLI argument strings, and config strings.
- Each code task must end with:
  - `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests`
  - `cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build`
  - `ReadLints` for changed Rust files
  - code-reviewer subagent

---

## File Structure

- Create: `lsp/crates/mylua-lsp/src/lua_symbol.rs`
  - Owns `LuaSymbol`, the private global `ThreadedRodeo`, serde output, and conversion helpers.
- Modify: `lsp/crates/mylua-lsp/Cargo.toml`
  - Adds `lasso`.
- Modify: `lsp/crates/mylua-lsp/src/lib.rs`
  - Exposes `lua_symbol` internally like `uri_id`.
- Modify: `lsp/crates/mylua-lsp/src/type_system.rs`
  - Converts long-lived type names, field names, function names, module paths, and parameter names to `LuaSymbol`.
- Modify: `lsp/crates/mylua-lsp/src/table_shape.rs`
  - Converts table field keys, field names, and owner names to `LuaSymbol`.
- Modify: `lsp/crates/mylua-lsp/src/summary.rs`
  - Converts summary names and indexes to `LuaSymbol`; keeps JSON output readable through `LuaSymbol::Serialize`.
- Modify: `lsp/crates/mylua-lsp/src/scope.rs`
  - Converts declaration names and bound class names to `LuaSymbol`.
- Modify: `lsp/crates/mylua-lsp/src/aggregation.rs`
  - Converts global/type/module shards and reverse indexes to `LuaSymbol`.
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/**`, `resolver.rs`, `goto.rs`, `hover.rs`, `completion.rs`, `signature_help.rs`, `inlay_hint.rs`, `references.rs`, `rename.rs`, `workspace_symbol.rs`, `symbols.rs`, diagnostics modules as needed.
  - Converts build/query boundaries to intern or resolve symbols while preserving external behavior.
- Test: existing `lsp/crates/mylua-lsp/tests/*.rs`
  - Existing behavior tests are the primary regression suite.
- Test: `lsp/crates/mylua-lsp/src/lua_symbol.rs`
  - Unit tests for interning, equality, resolve, display/debug, and JSON string output.

---

### Task 1: Add LuaSymbol Infrastructure

**Files:**
- Modify: `lsp/crates/mylua-lsp/Cargo.toml`
- Create: `lsp/crates/mylua-lsp/src/lua_symbol.rs`
- Modify: `lsp/crates/mylua-lsp/src/lib.rs`

- [ ] **Step 1: Add dependency**

Run this from the crate directory so Cargo chooses the current latest compatible version:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp/crates/mylua-lsp && cargo add lasso
```

Expected: `Cargo.toml` gains a `lasso` dependency and `Cargo.lock` updates.

- [ ] **Step 2: Create the symbol module**

Create `lsp/crates/mylua-lsp/src/lua_symbol.rs`:

```rust
use std::fmt;
use std::sync::OnceLock;

use lasso::{Spur, ThreadedRodeo};
use serde::{Serialize, Serializer};

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LuaSymbol(Spur);

static LUA_SYMBOLS: OnceLock<ThreadedRodeo> = OnceLock::new();

pub fn intern_lua_symbol(text: &str) -> LuaSymbol {
    LuaSymbol(symbols().get_or_intern(text))
}

pub fn resolve_lua_symbol(symbol: LuaSymbol) -> &'static str {
    symbols().resolve(&symbol.0)
}

impl LuaSymbol {
    pub fn as_str(self) -> &'static str {
        resolve_lua_symbol(self)
    }
}

impl fmt::Debug for LuaSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("LuaSymbol").field(&self.as_str()).finish()
    }
}

impl fmt::Display for LuaSymbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<&str> for LuaSymbol {
    fn from(value: &str) -> Self {
        intern_lua_symbol(value)
    }
}

impl Serialize for LuaSymbol {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

fn symbols() -> &'static ThreadedRodeo {
    LUA_SYMBOLS.get_or_init(ThreadedRodeo::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interns_equal_text_to_equal_symbols() {
        let a = intern_lua_symbol("Player.name");
        let b = intern_lua_symbol("Player.name");
        let c = intern_lua_symbol("Player.level");

        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.as_str(), "Player.name");
    }

    #[test]
    fn serializes_as_string() {
        let symbol = intern_lua_symbol("Player.name");
        let json = serde_json::to_string(&symbol).unwrap();

        assert_eq!(json, "\"Player.name\"");
    }
}
```

If the current `lasso` API requires `resolve(&symbol.0)` to borrow differently, adjust only inside this module. Keep all call sites independent of `lasso`.

- [ ] **Step 3: Export the module**

In `lsp/crates/mylua-lsp/src/lib.rs`, add near `pub mod uri_id;`:

```rust
pub mod lua_symbol;
```

- [ ] **Step 4: Verify the infrastructure**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test lua_symbol
```

Expected: the new unit tests pass.

---

### Task 2: Migrate Core Summary Value Types

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/type_system.rs`
- Modify: `lsp/crates/mylua-lsp/src/table_shape.rs`
- Modify: `lsp/crates/mylua-lsp/src/summary.rs`
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/**`

- [ ] **Step 1: Convert type-system names**

In `type_system.rs`, import `LuaSymbol`:

```rust
use crate::lua_symbol::{intern_lua_symbol, LuaSymbol};
```

Replace long-lived `String` fields with `LuaSymbol`:

```rust
pub enum KnownType {
    Nil,
    Boolean,
    Number,
    Integer,
    String,
    Table(TableShapeId),
    Function(FunctionSignature),
    FunctionRef(FunctionSummaryId),
    EmmyType(LuaSymbol),
    EmmyGeneric(LuaSymbol, Vec<TypeFact>),
}

pub enum SymbolicStub {
    RequireRef { module_path: LuaSymbol },
    CallReturn {
        base: Box<TypeFact>,
        func_name: LuaSymbol,
        is_method_call: bool,
        call_arg_types: Vec<TypeFact>,
        generic_args: Vec<TypeFact>,
    },
    FunctionCallReturn {
        func_name: LuaSymbol,
        call_arg_types: Vec<TypeFact>,
    },
    GlobalRef { name: LuaSymbol },
    TypeRef { name: LuaSymbol },
    FieldOf {
        base: Box<TypeFact>,
        field: LuaSymbol,
    },
}

pub struct ParamInfo {
    pub name: LuaSymbol,
    pub type_fact: TypeFact,
    pub optional: bool,
}
```

Update display and comparison code to use `.as_str()` or `format!("{}", symbol)`. For example:

```rust
if skip_self && p.name.as_str() == "self" {
    continue;
}
```

When building facts from parsed text, intern at the construction boundary:

```rust
TypeFact::Known(KnownType::EmmyType(intern_lua_symbol(type_name)))
```

- [ ] **Step 2: Convert table shape names**

In `table_shape.rs`, replace:

```rust
pub fields: HashMap<String, FieldInfo>,
pub owner_name: Option<String>,
pub name: String,
```

with:

```rust
pub fields: HashMap<LuaSymbol, FieldInfo>,
pub owner_name: Option<LuaSymbol>,
pub name: LuaSymbol,
```

Change setters to intern once at the boundary:

```rust
pub fn set_owner(&mut self, name: &str) {
    if self.owner_name.is_none() && !name.is_empty() {
        self.owner_name = Some(intern_lua_symbol(name));
    }
}

pub fn set_field(&mut self, name: &str, info: FieldInfo) {
    self.fields.insert(intern_lua_symbol(name), info);
}
```

Adjust callers in `summary_builder/table_extract.rs` to pass `&str` or resolved symbol text, not owned `String` unless already needed for parsing.

- [ ] **Step 3: Convert summary names**

In `summary.rs`, replace long-lived names:

```rust
pub function_name_index: HashMap<LuaSymbol, FunctionSummaryId>,
pub meta_name: Option<LuaSymbol>,
pub callee_name: LuaSymbol,
pub caller_name: LuaSymbol,
pub name: LuaSymbol,
pub generic_params: Vec<LuaSymbol>,
pub parents: Vec<LuaSymbol>,
```

Keep `DocumentSummary.uri: Uri` unchanged because URI identity is already handled by `UriId` at aggregation/document-store boundaries and JSON output is useful.

Remove `Deserialize` derives from `DocumentSummary`, `CallSite`, `GlobalContribution`, `FunctionSummary`, `TypeDefinition`, `TypeFieldDef`, `TypeFact`, `KnownType`, `SymbolicStub`, `FunctionSignature`, `ParamInfo`, `TableShape`, and `FieldInfo` unless a compile error proves a real deserialization path still exists. Keep `Serialize` so `lua_perf --summary` works.

- [ ] **Step 4: Update summary builder construction**

In `summary_builder/**`, intern at the point parsed strings become summary/scope/type facts:

```rust
let name = intern_lua_symbol(name_text);
```

Keep parser helpers that extract raw source text returning `String` when they are request-local parser utilities. Convert to `LuaSymbol` only when storing into `DocumentSummary`, `ScopeDecl`, `TypeFact`, or `TableShape`.

- [ ] **Step 5: Verify summary JSON output remains readable**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo run --bin lua-perf -- --summary-stdout /Users/zhuguosen/MyGit/ai-mylua-lsp/tests/lua-root/main.lua
```

Expected: JSON name fields print strings such as `"Player"` or `"foo.bar"`, not numeric `Spur` values. If the exact fixture path differs, use any small Lua file from `tests/lua-root`.

---

### Task 3: Migrate ScopeTree and WorkspaceAggregation

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/scope.rs`
- Modify: `lsp/crates/mylua-lsp/src/aggregation.rs`
- Modify: `lsp/crates/mylua-lsp/src/indexing.rs`
- Modify: `lsp/crates/mylua-lsp/src/workspace_scanner.rs` only if module names are stored long-term after scanning

- [ ] **Step 1: Convert scope declarations**

In `scope.rs`, replace:

```rust
pub name: String,
pub bound_class: Option<String>,
```

with:

```rust
pub name: LuaSymbol,
pub bound_class: Option<LuaSymbol>,
```

Keep query APIs string-facing:

```rust
pub fn resolve_decl(&self, byte_offset: usize, name: &str) -> Option<&ScopeDecl> {
    let name = intern_lua_symbol(name);
    self.resolve_decl_symbol(byte_offset, name)
}
```

Use an internal helper for symbol comparisons:

```rust
fn find_decl_in_scope_symbol(
    &self,
    scope_id: usize,
    byte_offset: usize,
    name: LuaSymbol,
) -> Option<&ScopeDecl> {
    self.scopes[scope_id]
        .declarations
        .iter()
        .rev()
        .find(|decl| decl.name == name && decl.visible_after_byte <= byte_offset)
}
```

When returning LSP-facing `Definition`, resolve the symbol:

```rust
name: decl.name.as_str().to_string(),
```

- [ ] **Step 2: Convert aggregation shards**

In `aggregation.rs`, replace long-lived string keys and candidate names:

```rust
pub type_shard: HashMap<LuaSymbol, Vec<TypeCandidate>>,
module_index: HashMap<LuaSymbol, Vec<(LuaSymbol, UriId)>>,
pub require_aliases: HashMap<LuaSymbol, LuaSymbol>,
pub name: LuaSymbol,
pub children: HashMap<LuaSymbol, GlobalNode>,
roots: HashMap<LuaSymbol, GlobalNode>,
uri_to_paths: HashMap<UriId, Vec<LuaSymbol>>,
```

Keep public/query methods accepting `&str` unless all callers are already symbol-aware. Intern once at the boundary:

```rust
pub fn exact_candidates(&self, name: &str) -> Option<&Vec<GlobalCandidate>> {
    self.exact_candidates_symbol(intern_lua_symbol(name))
}
```

Use resolved text only when splitting a qualified global path:

```rust
let path = candidate.name.as_str();
let (root, segments) = split_global_path(path);
let root = intern_lua_symbol(root);
```

For `iter_all_entries`, `iter_roots_with_prefix`, and `all_module_names`, keep returning `String` if callers build LSP UI output. Build those strings at iteration/output time from `LuaSymbol::as_str()`.

- [ ] **Step 3: Convert module index inputs**

`indexing.rs` and `workspace_scanner.rs` may still produce module names as `String` while scanning. That is fine for transient scan results. Intern when inserting into `WorkspaceAggregation::set_require_mapping` or `module_index`:

```rust
pub fn set_require_mapping(&mut self, module_name: String, uri_id: UriId) {
    let module_name = intern_lua_symbol(&module_name);
    ...
}
```

Avoid changing config alias storage in `config.rs`; config strings are not the main long-lived duplication source. Convert aliases to `LuaSymbol` when stored on `WorkspaceAggregation`.

- [ ] **Step 4: Verify aggregation behavior**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test test_goto test_references test_diagnostics
```

Expected: goto, references, and diagnostics tests pass without behavior changes.

---

### Task 4: Update Consumers Without Interning UI Text

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs`
- Modify: `lsp/crates/mylua-lsp/src/goto.rs`
- Modify: `lsp/crates/mylua-lsp/src/hover.rs`
- Modify: `lsp/crates/mylua-lsp/src/completion.rs`
- Modify: `lsp/crates/mylua-lsp/src/signature_help.rs`
- Modify: `lsp/crates/mylua-lsp/src/inlay_hint.rs`
- Modify: `lsp/crates/mylua-lsp/src/references.rs`
- Modify: `lsp/crates/mylua-lsp/src/rename.rs`
- Modify: `lsp/crates/mylua-lsp/src/workspace_symbol.rs`
- Modify: diagnostics modules that inspect `TypeFact`, `TableShape`, `ScopeDecl`, or `DocumentSummary`

- [ ] **Step 1: Update resolver comparisons and visited sets**

Where resolver code currently uses `HashSet<String>` for visited type names or module names that come from `TypeFact`, use `HashSet<LuaSymbol>`:

```rust
let mut visited: HashSet<LuaSymbol> = HashSet::new();
```

When a resolver function still receives user/source text as `&str`, intern once at entry and pass the symbol through private helpers.

- [ ] **Step 2: Resolve symbols at LSP output boundaries**

For user-facing output, convert symbols back to strings at the final boundary:

```rust
CompletionItem {
    label: field.name.as_str().to_string(),
    ..
}
```

Keep assembled strings as regular `String`:

```rust
let markdown = format!("```lua\n{}\n```", signature.display_label(...));
```

Do not introduce `LuaSymbol` for markdown, diagnostics text, completion detail, signature help labels, index status messages, or logs.

- [ ] **Step 3: Update references/rename matching**

References and rename extract source text for the symbol under cursor. Keep extraction returning `String`; intern once for comparisons against stored symbols:

```rust
let target = intern_lua_symbol(&target_name);
if decl.name == target {
    ...
}
```

When building `WorkspaceEdit`, keep replacement text as the user-provided `String`; do not intern it unless storing into summary/scope during re-index.

- [ ] **Step 4: Update diagnostics**

Diagnostics should compare `LuaSymbol` values internally but format messages from `as_str()`:

```rust
format!("Undefined global '{}'", name.as_str())
```

Avoid changing diagnostic code/config identifiers such as `"undefinedGlobal"`; these are protocol/config strings, not indexed Lua symbols.

- [ ] **Step 5: Run full integration tests**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests
```

Expected: all integration tests pass.

---

### Task 5: Preserve Debug JSON and Measure Memory

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/bin/lua_perf.rs` only if output command help needs clarification
- Modify: `docs/future-work.md` only if implementation results change priority or scope
- Modify: `docs/performance-analysis.md` if measured RSS/indexing numbers are added

- [ ] **Step 1: Verify summary JSON**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo run --bin lua-perf -- --summary-stdout /Users/zhuguosen/MyGit/ai-mylua-lsp/tests/lua-root/main.lua
```

Expected:

```json
{
  "global_contributions": [
    {
      "name": "..."
    }
  ]
}
```

No summary field should show raw numeric `Spur` values.

- [ ] **Step 2: Compare large-workspace RSS**

On the 2w-file workspace, capture before/after RSS using the same startup flow and same extension/LSP settings. Record:

```text
file count
source bytes
peak RSS after initial index Ready
initial index wall time
```

Expected: RSS drops significantly from the ~6G baseline. Index build time should not regress; HashMap-heavy phases are expected to improve.

- [ ] **Step 3: Update performance docs if measured**

If real before/after numbers are collected, add them to `docs/performance-analysis.md`. Do not add speculative numbers.

---

### Task 6: Final Verification

**Files:**
- All changed Rust files
- `docs/future-work.md`
- Optional: `docs/performance-analysis.md`

- [ ] **Step 1: Run required verification**

Run:

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo test --tests
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp && cargo build
```

Expected: both commands finish with zero errors.

- [ ] **Step 2: Check lints**

Use `ReadLints` on changed Rust files. Fix any diagnostics introduced by this migration.

- [ ] **Step 3: Review**

Run the code-reviewer subagent with this checklist:

```text
Review the LuaSymbol interning migration. Focus on:
- any long-lived String fields missed in WorkspaceAggregation, DocumentSummary, ScopeTree, TypeFact, or TableShape
- accidental interning of request-local UI strings
- places where symbols are resolved too early and converted back into stored String
- serde JSON output still printing strings
- LSP behavior regressions around goto, references, rename, hover, completion, diagnostics
```

- [ ] **Step 4: Commit**

Only after tests, build, lints, and review pass:

```bash
git add lsp/crates/mylua-lsp/Cargo.toml lsp/Cargo.lock lsp/crates/mylua-lsp/src docs
git commit -m "$(cat <<'EOF'
refactor: intern long-lived Lua symbols

Replace duplicated long-lived Lua/index strings with compact symbols to reduce large-workspace memory use while preserving string output at LSP and debug JSON boundaries.

EOF
)"
```

