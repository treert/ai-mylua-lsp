# Scoped Type System — Phase 1 & 2 进度记录

**日期**: 2026-04-27
**状态**: ✅ Phase 1 完成（Task 1-10） ✅ Phase 2 完成

## Phase 2 变更

### 新增字段

- `BuildContext.pending_class_name: Option<String>` — `flush_pending_class` 暂存类名，由紧邻的 local/assignment 消费
- `BuildContext.global_class_bindings: HashMap<String, String>` — 全局变量 → class 名绑定

### 新增方法

- `BuildContext::resolve_bound_class_for(name) -> Option<&str>` — 先查 scope stack 的 `bound_class`，再查 `global_class_bindings`
- `add_field_to_class(ctx, class_name, field_name, type_fact, def_range)` — 向 @class 追加字段（Emmy @field 同名则跳过）

### 改动文件

| 文件 | 改动内容 |
|------|----------|
| `summary_builder/mod.rs` | 新增 `pending_class_name`, `global_class_bindings` 字段 + `resolve_bound_class_for` 方法 |
| `summary_builder/emmy_visitors.rs` | `flush_pending_class` 暂存 `pending_class_name` |
| `summary_builder/visitors.rs` | `visit_local_declaration` 消费 `pending_class_name` → `bound_class`; `visit_assignment` 消费 → `global_class_bindings`; `visit_function_declaration` 写方法到 @class; `visit_function_body` 注册 self 的 `bound_class`; `visit_assignment` 处理 `self.field = expr` 写回 @class; 非 local/assignment 语句清除 `pending_class_name` |

### Phase 2 功能

1. `---@class Foo` + `local Foo = ...` → `ScopeDecl.bound_class = Some("Foo")`
2. `---@class Foo` + `Foo = ...` (global) → `global_class_bindings["Foo"] = "Foo"`
3. `function Foo:method()` → 写方法字段到 @class Foo TypeDefinition
4. `self.field = expr` (冒号方法内) → 写字段到 @class TypeDefinition
5. 去重: Emmy `@field` 声明优先; 已追加的同名字段跳过
6. 变量查找严格分层: scope 找到即为 local，不回退查全局绑定

---

## Phase 1 Commit 链

```
200fca2 refactor: remove local_type_facts and delete build_scope_tree  ← Task 9
c04f7a2 feat: extend scope tree traversal into function bodies        ← Task 8
2c55331 refactor: migrate query-time consumers from local_type_facts to scope_tree  ← Task 7
b6bb86e feat: add anchor_shape_id to TypeDefinition, migrate cross-file consumers   ← Task 6
d4865a2 refactor: switch build-time variable lookups to resolve_in_build_scopes     ← Task 5
dc97eec feat: build_file_analysis returns (DocumentSummary, ScopeTree), wire up all callers ← Task 4
3b4fbe2 feat: dual-write local declarations into scope stack alongside local_type_facts ← Task 3
8695428 feat: add scope stack to BuildContext with push/pop/resolve methods    ← Task 2
f6824a7 feat: add type_fact and bound_class fields to ScopeDecl               ← Task 1
```

## Phase 1 最终架构状态

- `build_file_analysis` 单次 AST 遍历同时产出 `DocumentSummary` + `ScopeTree`
- `ScopeDecl` 携带 `type_fact`, `bound_class`, `is_emmy_annotated`
- `ScopeTree` 查询 API: `resolve_type(byte_offset, name)`, `resolve_decl(byte_offset, name)`, `all_declarations()`
- 函数体完整遍历: 参数（含 Emmy 类型）、隐式 self（冒号方法）、匿名函数体均入 scope
- **`local_type_facts` 已完全删除**: `LocalTypeFact`, `TypeFactSource`, `DocumentSummary.local_type_facts` 均已移除
- **`build_scope_tree` + `TreeBuilder` 已删除**: scope.rs 仅保留数据结构和查询方法
- 所有消费方（hover, goto, completion, inlay_hint, type_inference, diagnostics, aggregation）已切换到 scope_tree

## 关键文件路径

- Plan: `docs/superpowers/plans/2026-04-26-scoped-type-system-phase1.md`
- Spec: `docs/superpowers/specs/2026-04-26-scoped-type-system-design.md`
- scope.rs: `lsp/crates/mylua-lsp/src/scope.rs`
- summary_builder/mod.rs: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs`
- visitors.rs: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`

## 测试命令

```bash
cd lsp
cargo build && cargo test --tests  # 461+ tests 全过
```
