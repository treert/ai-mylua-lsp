# Scoped Type System — Phase 1 进度记录

**日期**: 2026-04-26
**状态**: Task 1-6 完成，Task 7 未开始（之前一次 subagent 尝试失败已回滚）

## 已完成的 Commit 链

```
b6bb86e feat: add anchor_shape_id to TypeDefinition, migrate cross-file consumers  ← 当前 HEAD
d4865a2 refactor: switch build-time variable lookups to resolve_in_build_scopes
dc97eec feat: build_file_analysis returns (DocumentSummary, ScopeTree), wire up all callers
3b4fbe2 feat: dual-write local declarations into scope stack alongside local_type_facts
8695428 feat: add scope stack to BuildContext with push/pop/resolve methods
f6824a7 feat: add type_fact and bound_class fields to ScopeDecl
```

## 当前架构状态

- `ScopeDecl` 已有 `type_fact: Option<TypeFact>` 和 `bound_class: Option<String>` 字段
- `BuildContext` 已有 scope stack（`push_scope`/`pop_scope`/`add_scoped_decl`/`resolve_in_build_scopes`/`take_scope_tree`）
- `visitors.rs` 已经**双写**：`local_type_facts.insert(...)` 和 `add_scoped_decl(...)` 并存
- `build_file_analysis` 返回 `(DocumentSummary, ScopeTree)`，所有调用方已切换
- build 时变量查找已切到 `resolve_in_build_scopes`（type_infer.rs + visitors.rs）
- `TypeDefinition.anchor_shape_id` 已加，resolver.rs 跨文件消费已迁移
- `ScopeTree` 查询 API：`resolve_type(byte_offset, name)`, `resolve_bound_class(byte_offset, name)`

## 剩余 Task

### Task 7: 迁移查询时消费方（最大最复杂）

需要把以下消费方从 `summary.local_type_facts.get(name)` 切到 `scope_tree.resolve_type(byte_offset, name)`。核心挑战是 threading `scope_tree` 参数到深层函数。

**消费方清单（按依赖顺序）：**

1. `resolver.rs::resolve_local_in_file` (line ~175) — 签名加 `byte_offset` + `scope_tree` 参数
2. `hover.rs::resolve_local_type_info` (line ~428) — 调用 resolve_local_in_file，同时做 FunctionRef hover fix
3. `goto.rs::type_definition_for_local` (line ~177) — 读 local_type_facts 改为 scope_tree
4. `type_inference.rs::infer_node_type` (lines ~76, ~105) — 签名加 `scope_tree`，**级联最大**：所有调用 infer_node_type 的地方都要改（hover, completion, signature_help, diagnostics）
5. `completion.rs::resolve_local_item` (line ~473)
6. `inlay_hint.rs` (line ~194)
7. `diagnostics/type_compat.rs` (lines ~20, ~107)
8. `diagnostics/type_mismatch.rs` (line ~22 用 `.values()` 遍历，line ~147 用 `.get()`)
9. `aggregation.rs` (line ~843) — 改为在 DocumentSummary 加 `referenced_type_names: HashSet<String>`

**建议拆分方式：**
- 7a: resolver + hover + goto（3 个文件，直接依赖链）
- 7b: type_inference.rs + 它的所有 callers（级联最大，涉及 hover, completion, signature_help, diagnostics）
- 7c: 其余独立消费方（completion resolve_local_item, inlay_hint, type_compat, type_mismatch）
- 7d: aggregation.rs（加 referenced_type_names 字段）

**注意点：**
- `type_mismatch.rs` line 22 遍历 `local_type_facts.values()` 检查 `TypeFactSource::EmmyAnnotation`，ScopeDecl 没有 source 字段，可能需要保留这一处或给 ScopeDecl 加 `is_emmy_annotated: bool` 字段
- `infer_node_type` 的级联改动最大——它被 hover, completion, signature_help, type_inference 自身的 `collect_call_arg_types`, diagnostics 等调用

### Task 8: 函数体完整遍历

- 加 `visit_function_body` 函数
- 注册 parameters 到 FunctionBody scope
- 注册 implicit self（冒号方法）
- 从 `visit_local_function` 和 `visit_function_declaration` 调用

### Task 9: 删除 local_type_facts + build_scope_tree

- 删 DocumentSummary.local_type_facts、LocalTypeFact、TypeFactSource
- 删 BuildContext.local_type_facts 和所有 `.insert(...)` 调用
- 删 scope.rs 的 TreeBuilder 和 build_scope_tree

### Task 10: 最终验证 + 文档更新

## 关键文件路径

- Plan: `docs/superpowers/plans/2026-04-26-scoped-type-system-phase1.md`
- Spec: `docs/superpowers/specs/2026-04-26-scoped-type-system-design.md`
- scope.rs: `lsp/crates/mylua-lsp/src/scope.rs`
- summary_builder/mod.rs: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs`
- visitors.rs: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`
- type_infer.rs: `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs`

## 测试命令

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo build && cargo test --tests  # 当前 461 tests 全过
```
