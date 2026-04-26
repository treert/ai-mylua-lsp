# 测试体系

## 测试资源

| 路径 | 用途 |
|------|------|
| `tests/lua-root/` | 手工端到端测试目录（含 UE4 场景） |
| `tests/lua-root2/` | 跨 workspace 端到端测试目录 |
| `tests/complete/` | 补全功能 fixture |
| `tests/hover/` | Hover 功能 fixture |
| `tests/define/` | 跳转定义 fixture |
| `tests/parse/` | 解析 fixture |
| `tests/project/` | 多文件工程级 fixture |
| `vscode-extension/assets/lua5.4/` | Lua 5.4 stdlib stub（扩展内置） |

## 手工端到端测试

`tests/lua-root/` + `tests/lua-root2/` 通过 [`tests/mylua-tests.code-workspace`](../tests/mylua-tests.code-workspace) 挂载为两个 workspace folder，用于在 Extension Development Host 中人工验证 LSP 行为。

**启动**：运行 `.cursor/scripts/test-extension.sh`（macOS/Linux）或 `test-extension.ps1`（Windows），自动构建 LSP + 扩展并打开 workspace。

## 集成测试

采用 **lib + bin 拆分**：集成测试直接调用 `lib.rs` 导出的核心函数，无需 LSP stdio 通信。测试工具模块 `test_helpers.rs` 提供 `parse_doc()`、`setup_single_file()`、`setup_workspace_from_dir()` 等辅助函数。

| 测试文件 | 数量 | 覆盖功能 |
|----------|------|----------|
| `test_parse.rs` | 8 | 基础解析、EmmyLua 注解 |
| `test_hover.rs` | 34 | hover 全场景（类型、文档、链式调用等） |
| `test_completion.rs` | 11 | 补全（字段、方法、关键字、require 路径） |
| `test_completion_resolve.rs` | 5 | 补全 resolve |
| `test_signature_help.rs` | 13 | 签名帮助（overload、self 隐藏等） |
| `test_goto.rs` | 11 | 跳转定义 |
| `test_type_definition.rs` | 6 | 跳转类型定义 |
| `test_references.rs` | 8 | 引用查找 |
| `test_rename.rs` | 5 | 重命名（含跨文件） |
| `test_call_hierarchy.rs` | 8 | 调用层次 |
| `test_workspace_symbol.rs` | 7 | 工作区符号 |
| `test_symbols.rs` | 13 | 文档符号 |
| `test_scope.rs` | 11 | 作用域树 |
| `test_diagnostics.rs` | 42 | 语法 + 语义诊断全覆盖 |
| `test_diagnostic_suppress.rs` | 9 | `@diagnostic` 抑制 |
| `test_folding_range.rs` | 16 | 折叠范围 |
| `test_selection_range.rs` | 5 | 选区扩展 |
| `test_semantic_tokens_delta.rs` | 4 | 语义 token 增量 |
| `test_semantic_tokens_range.rs` | 4 | 语义 token 范围 |
| `test_runtime_version.rs` | 5 | Lua 版本差异 |
| `test_inlay_hint.rs` | 6 | 内嵌提示 |
| `test_shape_owner.rs` | 4 | table shape 归属 |
| `test_emmy_self_and_multireturn.rs` | 8 | self 替换 + 多返回值 |
| `test_meta.rs` | 6 | `@meta` 元文件 |
| `test_document_link.rs` | 6 | require 链接 |
| `test_document_highlight.rs` | 10 | 文档高亮 |
| `test_workspace.rs` | 7 | 多文件工作区 |
| `test_workspace_library.rs` | 5 | workspace.library |

## 内嵌单元测试

`src/*.rs` 中的 `#[cfg(test)] mod tests`，覆盖：`util.rs`（UTF 转换）、`lib.rs`（URL 解码）、`rename.rs`（标识符校验）、`signature_help.rs`（逗号计数）、`folding_range.rs`（块注释识别）、`lua_builtins.rs`（版本差异）、`workspace_scanner.rs`（路径解析）、`emmy.rs`（注解解析）。

## 运行

```bash
cd lsp && cargo test --tests    # 全部 460+ 条测试
```

> **注意**：VS Code 扩展运行时会锁住 `mylua-lsp.exe`，可用独立 target 目录：
> `CARGO_TARGET_DIR="target-test" cargo test --tests`
