# Scoped Type System — Phase 1 进度记录

**日期**: 2026-04-27
**状态**: ✅ Phase 1 完成（Task 1-10）

## Commit 链

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

## 最终架构状态

- `build_file_analysis` 单次 AST 遍历同时产出 `DocumentSummary` + `ScopeTree`
- `ScopeDecl` 携带 `type_fact`, `bound_class`, `is_emmy_annotated`
- `ScopeTree` 查询 API: `resolve_type(byte_offset, name)`, `resolve_decl(byte_offset, name)`, `all_declarations()`
- 函数体完整遍历: 参数（含 Emmy 类型）、隐式 self（冒号方法）、匿名函数体均入 scope
- **`local_type_facts` 已完全删除**: `LocalTypeFact`, `TypeFactSource`, `DocumentSummary.local_type_facts` 均已移除
- **`build_scope_tree` + `TreeBuilder` 已删除**: scope.rs 仅保留数据结构和查询方法
- 所有消费方（hover, goto, completion, inlay_hint, type_inference, diagnostics, aggregation）已切换到 scope_tree

## 已删除的代码

| 删除项 | 原位置 |
|--------|--------|
| `LocalTypeFact` struct | summary.rs |
| `TypeFactSource` enum | summary.rs |
| `DocumentSummary.local_type_facts` field | summary.rs |
| `BuildContext.local_type_facts` field | summary_builder/mod.rs |
| `build_scope_tree()` function | scope.rs |
| `TreeBuilder` struct + impl | scope.rs |
| `build_summary()` deprecated wrapper | summary_builder/mod.rs |
| 所有 `ctx.local_type_facts.insert(...)` 调用 | visitors.rs |
| 所有查询时 `local_type_facts` fallback | hover/goto/type_inference/inlay_hint/type_compat/type_mismatch/aggregation |

## 关键文件路径

- Plan: `docs/superpowers/plans/2026-04-26-scoped-type-system-phase1.md`
- Spec: `docs/superpowers/specs/2026-04-26-scoped-type-system-design.md`
- scope.rs: `lsp/crates/mylua-lsp/src/scope.rs`
- summary_builder/mod.rs: `lsp/crates/mylua-lsp/src/summary_builder/mod.rs`
- visitors.rs: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs`

## 测试命令

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo build && cargo test --tests  # 461 tests 全过，0 warnings
```
