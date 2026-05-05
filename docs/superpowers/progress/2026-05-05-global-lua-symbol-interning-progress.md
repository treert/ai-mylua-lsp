# Global LuaSymbol Interning — 进度记录

**日期**: 2026-05-05
**状态**: Task 1 完成并已提交；Task 2 拆分迁移已完成；Task 2.6 query lookup 边界修复完成并验证

## 当前目标

用全局字符串驻留池降低大型工作区内存占用。方案是引入 `LuaSymbol(Spur)`，内部封装全局 `lasso::ThreadedRodeo`，然后逐步把常驻结构里的重复 `String` 替换为 `LuaSymbol`。

只优化长期驻留内存的数据结构：

- `WorkspaceAggregation` / `GlobalShard`
- `DocumentSummary`
- `ScopeTree`
- `TypeFact`
- `TableShape`

不优化请求级临时字符串：

- hover markdown
- diagnostic message
- completion label/detail
- signature help label
- index status message
- CLI 参数和 config 字符串

## 关键文档

- Plan: `docs/superpowers/plans/2026-05-05-global-lua-symbol-interning.md`

## 已完成

### Task 1: Add LuaSymbol Infrastructure

提交：

```text
cf7fefb refactor: add LuaSymbol infrastructure
```

改动：

- `lsp/crates/mylua-lsp/Cargo.toml` 添加 `lasso = { version = "0.7.3", features = ["multi-threaded"] }`
- `lsp/Cargo.lock` 更新依赖锁定
- `lsp/crates/mylua-lsp/src/lua_symbol.rs` 新增：
  - `LuaSymbol(Spur)` newtype
  - private `OnceLock<ThreadedRodeo>`
  - `intern_lua_symbol(&str) -> LuaSymbol`
  - `resolve_lua_symbol(LuaSymbol) -> &'static str`
  - `LuaSymbol::as_str()`
  - `Debug`, `Display`, `From<&str>`, `Serialize`
  - 单元测试覆盖 interning、resolve、Display、Debug、From、JSON 序列化
- `lsp/crates/mylua-lsp/src/lib.rs` 导出 `pub mod lua_symbol;`

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test lua_symbol
cargo build
```

结果：

- `cargo test lua_symbol`: 7 passed
- `cargo build`: passed
- `ReadLints`: no linter errors
- `code-reviewer`: no issues found

### Task 2.1: Migrate Core TypeFact Value Names

改动：

- `TypeFact` / `KnownType` / `SymbolicStub` 中长期保存的类型名、模块名、全局名、字段名改为 `LuaSymbol`
- `FunctionSignature` / `ParamInfo` 参数名改为 `LuaSymbol`
- 更新 `emmy.rs`、`type_inference.rs`、`summary_builder/**`、`resolver.rs`、goto / hover / completion / signature help / references / diagnostics 相关调用点
- `summary.rs` / `table_shape.rs` 中依赖 `TypeFact`、`FunctionSignature` 的长期结构移除 `Deserialize` 派生，保留 `Serialize`
- 新增 `type_system` 回归测试，覆盖 `LuaSymbol` 存储下 JSON 仍输出字符串

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test long_lived_type_names_use_symbols_but_serialize_as_strings
cargo test --tests
cargo build
```

结果：

- targeted test: passed
- `cargo test --tests`: 580 passed
- `cargo build`: passed
- `ReadLints`: no linter errors
- `code-reviewer`: no blocking issues found

### Task 2.2: Migrate TableShape Field Names

改动：

- `TableShape.fields` key 从 `String` 改为 `LuaSymbol`
- `TableShape.owner_name` 从 `Option<String>` 改为 `Option<LuaSymbol>`
- `FieldInfo.name` 从 `String` 改为 `LuaSymbol`
- 更新 table extraction、nested field writes、resolver、field diagnostics、summary builder inference 的 field lookup 边界，查找前 intern key
- 新增 `table_shape` 回归测试，覆盖 owner、field key、FieldInfo JSON 仍输出字符串

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test long_lived_table_names_use_symbols_but_serialize_as_strings
cargo test --tests
cargo build
```

结果：

- targeted test: passed
- `cargo test --tests`: 581 passed
- `cargo build`: passed
- `ReadLints`: no linter errors
- `code-reviewer`: no blocking issues found

### Task 2.3: Migrate DocumentSummary Names

改动：

- `DocumentSummary.function_name_index` 从 `HashMap<String, FunctionSummaryId>` 改为 `HashMap<LuaSymbol, FunctionSummaryId>`
- `DocumentSummary.meta_name`、`CallSite` 的 callee/caller、`GlobalContribution.name`、`FunctionSummary.name/generic_params` 改为 `LuaSymbol`
- `TypeDefinition.name/parents/generic_params` 和 `TypeFieldDef.name` 改为 `LuaSymbol`
- `summary_builder` 在构造 `DocumentSummary` 长期字段时 intern 名称；消费者在 LSP/UI 输出边界用 `as_str()` / `to_string()` 转回字符串
- 新增 `get_lua_symbol` 非插入式查询 helper，`DocumentSummary::get_function_by_name` 查找 miss 时不会把请求级临时字符串写入全局 interner
- 新增 `summary` 回归测试，覆盖 summary JSON 字段和 `function_name_index` key 仍序列化为字符串，并验证 colon-normalized lookup

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test lua_symbol
cargo test summary_lua_symbols_serialize_as_strings_and_lookup_normalizes_colon
cargo test --tests
cargo build
```

结果：

- `cargo test lua_symbol`: 9 passed
- targeted summary test: passed
- `cargo test --tests`: 583 passed
- `cargo build`: passed, 0 warnings
- `ReadLints`: no linter errors
- spec review: no spec issues found
- `code-reviewer`: no issues found

### Task 2.4: Migrate ScopeTree and WorkspaceAggregation Names

提交：

```text
3793623 refactor: intern scope and aggregation names
```

改动：

- `ScopeDecl.name`、`ScopeDecl.bound_class` 改为 `LuaSymbol`
- `GlobalCandidate.name`、`TypeCandidate.name` 改为 `LuaSymbol`
- `GlobalNode.children`、`GlobalShard.roots`、`GlobalShard.uri_to_paths` 改为 `LuaSymbol` key / value
- `WorkspaceAggregation.type_shard`、`module_index`、`require_aliases` 改为 `LuaSymbol` key / value
- 新增 `WorkspaceAggregation::type_candidates` / `contains_type`，保持 type 查询 API 字符串边界且 lookup miss 不写入 interner
- 更新 scope completion、unused local diagnostics、goto / hover / references / resolver / workspace_symbol 等消费者，在 LSP 输出边界转回 `String`
- 新增 `scope` / `aggregation` 回归测试，覆盖长期字段已使用 `LuaSymbol` 且字符串查询行为不变

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test long_lived_ -- --nocapture
cargo test --test test_goto && cargo test --test test_references && cargo test --test test_diagnostics
cargo test --tests
cargo build
cargo run --bin lua-perf -- --summary-stdout /Users/zhuguosen/MyGit/ai-mylua-lsp/tests/lua-root/main.lua
```

结果：

- targeted migration tests: passed
- planned integration tests: `test_goto` 29 passed, `test_references` 14 passed, `test_diagnostics` 84 passed
- `cargo test --tests`: 585 passed
- `cargo build`: passed
- `lua-perf --summary-stdout`: passed; JSON name fields still output strings
- `ReadLints`: no linter errors
- `code-reviewer`: no blocking issues found

### Task 2.5: Tighten summary_builder Symbol Boundaries

提交：

```text
1832ba6 refactor: tighten summary builder symbol boundaries
```

改动：

- `BuildContext.pending_class_name` 从 `Option<String>` 改为 `Option<LuaSymbol>`
- `BuildContext.global_class_bindings` value 从 `String` 改为 `LuaSymbol`
- `resolve_bound_class_for_at` 改为返回 `Option<LuaSymbol>`，避免从 `ScopeDecl.bound_class` resolve 成 `String` 后再 intern
- 更新 local/global class binding、class field injection、method `self` scope declaration 的构造路径，直接传递 `LuaSymbol`

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test lua_symbol
cargo test summary_lua_symbols_serialize_as_strings_and_lookup_normalizes_colon
cargo test long_lived_scope_names_use_symbols_but_queries_stay_string_facing
cargo test --tests
cargo build
```

结果：

- targeted tests: passed
- `cargo test --tests`: 585 passed
- `cargo build`: passed
- `ReadLints`: no linter errors
- `code-reviewer`: no blocking issues found

### Task 2.6: Tighten Query Lookup Interning Boundaries

改动：

- `ScopeTree::resolve_decl` / `scope_byte_range_for_def` 从插入式 `intern_lua_symbol` 改为非插入式 `get_lua_symbol`
- `unused_local` 引用计数使用已解析出的 `decl.name`，避免对请求级 identifier 文本重复 interning
- `TableShape::get_field` 新增非插入式字段查询 helper
- resolver、field diagnostics、summary builder 的字段查询改用 `TableShape::get_field`
- summary builder 的 `function_name_index` fallback 查询改用 `get_lua_symbol`
- 新增 `scope_lookup_misses_do_not_intern_request_names` 回归测试，覆盖 lookup miss 不应写入全局 symbol pool
- 新增 `field_lookup_misses_do_not_intern_request_names` 回归测试，覆盖字段 lookup miss 不应写入全局 symbol pool

验证：

```bash
cd /Users/zhuguosen/MyGit/ai-mylua-lsp/lsp
cargo test scope_lookup_misses_do_not_intern_request_names
cargo test field_lookup_misses_do_not_intern_request_names
cargo run --bin lua-perf -- --summary-stdout /Users/zhuguosen/MyGit/ai-mylua-lsp/tests/lua-root/main.lua
cargo test --tests
cargo build
```

结果：

- RED: 新测试先失败，证明 `resolve_decl` / `scope_byte_range_for_def` 的 lookup miss 会写入 interner
- RED: 字段查询测试先因缺少非插入式查询 API 失败
- GREEN: 修复后 targeted test passed
- `lua-perf --summary-stdout`: passed; JSON name fields still output strings
- `cargo test --tests`: full suite passed
- `cargo build`: passed
- `ReadLints`: no linter errors

## 当前仓库状态

本轮开始时检查：

```text
git status --short --branch
git log --oneline -5
```

已确认最近提交：

```text
1832ba6 refactor: tighten summary builder symbol boundaries
3793623 refactor: intern scope and aggregation names
```

本轮改动覆盖 Task 2.6。

## 下一步建议

原计划 Task 2 已按更小粒度拆分并完成：

1. ~~`TypeFact` / `FunctionSignature` / `ParamInfo` 里的长期字符串改为 `LuaSymbol`~~
2. ~~`TableShape` / `FieldInfo` 字段名和 owner 改为 `LuaSymbol`~~
3. ~~`DocumentSummary` 名称字段和 `function_name_index` 改为 `LuaSymbol`~~
4. ~~`ScopeTree` / `WorkspaceAggregation` 名称字段、索引 key、reverse indexes 改为 `LuaSymbol`~~
5. ~~`summary_builder` 构造边界继续收口，避免先 resolve 回 `String` 再存储~~
6. ~~`lua_perf --summary` 验证 JSON 仍输出字符串~~
7. ~~修复 query lookup miss 不应写入全局 interner~~

每个小任务都应保持：

- 不运行 `cargo fmt` / `rustfmt`
- 只迁移常驻结构，不迁移 UI 临时字符串
- LSP 输出边界仍输出 `String`
- 验证至少包含 `cargo test --tests` 和 `cargo build`
- 完成后调用 code-reviewer

后续可继续原计划 Task 5/6：

- 若有 2w 文件级 workspace，采集 before/after RSS 与索引耗时，真实数据再写入 `docs/performance-analysis.md`
- 无大工作区数据时，不写推测数字；只保留行为验证、JSON 验证和最终 review

## 新会话起步提示

新会话建议先读：

1. `ai-readme.md`
2. `docs/README.md`
3. `docs/superpowers/plans/2026-05-05-global-lua-symbol-interning.md`
4. 本文件

然后从“是否具备大工作区 RSS 测量环境”继续；如果没有，就进入最终验证和收尾。

