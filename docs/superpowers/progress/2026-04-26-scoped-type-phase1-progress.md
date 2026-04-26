# Scoped Type System — Phase 1 进度记录

**日期**: 2026-04-26
**状态**: Task 1-7 完成，Task 8 未开始

## 已完成的 Commit 链

```
<pending>  refactor: migrate query-time consumers from local_type_facts to scope_tree  ← Task 7
b6bb86e feat: add anchor_shape_id to TypeDefinition, migrate cross-file consumers
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
- **查询时消费方已迁移**：所有 query-time consumer 优先使用 `scope_tree`，fallback 到 `local_type_facts`

## Task 7 迁移详情

**已迁移的消费方（scope_tree 优先 + local_type_facts fallback）：**
- `resolver.rs::resolve_local_in_file` — 签名加 `byte_offset` + `scope_tree`
- `hover.rs::resolve_local_type_info` — scope_tree → FunctionRef hover fix + fallback
- `goto.rs::type_definition_for_local` — scope_tree → local_type_facts fallback
- `type_inference.rs::infer_node_type` — 签名加 `scope_tree`，级联到所有 callers
- `inlay_hint.rs::collect_variable_type_hints` — scope_tree → local_type_facts fallback
- `type_compat.rs::infer_argument_type` — scope_tree → local_type_facts fallback
- `aggregation.rs` — 新增 `referenced_local_type_names: HashSet<String>` 字段

**级联更新的 callers（因 infer_node_type 签名变更）：**
- `hover.rs::build_field_hover`
- `goto.rs::goto_field_or_method`
- `completion.rs::try_dot_completion_ast`
- `signature_help.rs::resolve_call_signatures` + 3 callers（signature_help, inlay_hint, call_args）
- `diagnostics/field_access.rs` — FieldDiagCtx 加 scope_tree
- `diagnostics/call_args.rs` — scope_tree 传递到 pick_best_typing_overload

**保留 local_type_facts 的消费方（需 TypeFactSource / 无位置上下文）：**
- `type_compat.rs::infer_literal_type` — 需要 `TypeFactSource::EmmyAnnotation` 过滤
- `type_mismatch.rs` Pass 1 — 遍历 `local_type_facts.values()` 过滤 EmmyAnnotation
- `type_mismatch.rs` line 147 — 需要 `ltf.source` 检查
- `completion.rs::resolve_local_item` — completionItem/resolve 无位置上下文

## 剩余 Task

### Task 8: 函数体完整遍历

- 加 `visit_function_body` 函数
- 注册 parameters 到 FunctionBody scope
- 注册 implicit self（冒号方法）
- 从 `visit_local_function` 和 `visit_function_declaration` 调用
- **完成后可移除 Task 7 中的所有 local_type_facts fallback**

### Task 9: 删除 local_type_facts + build_scope_tree

- 删 DocumentSummary.local_type_facts、LocalTypeFact、TypeFactSource
- 删 BuildContext.local_type_facts 和所有 `.insert(...)` 调用
- 删 scope.rs 的 TreeBuilder 和 build_scope_tree
- 给 ScopeDecl 加 `is_emmy_annotated: bool` 字段（替代 TypeFactSource 的语义）
- 清理 resolver.rs 的 `_uri` 参数
- 清理 hover.rs 的双重 scope_tree lookup

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
