# 测试体系

**本文档记录项目的测试框架、测试资源和完整测试清单。**

## 测试资源文件

| 路径 | 用途 |
|------|------|
| [`vscode-extension/assets/lua5.4/`](../vscode-extension/assets/lua5.4/) | Lua 5.4 标准库 EmmyLua 类型注释（11 个文件），作为 VS Code 扩展打包时的内置 stdlib stub 资源 |
| [`tests/lua-root/`](../tests/lua-root/) | **手工端到端测试目录**：用于在 Extension Development Host 中实际体验 LSP 能力 |
| [`tests/lua-root2/`](../tests/lua-root2/) | 跨 workspace 端到端测试目录 |
| [`tests/complete/`](../tests/complete/) | 补全功能测试 Lua fixture（17 个文件） |
| [`tests/hover/`](../tests/hover/) | Hover 功能测试 Lua fixture（18 个文件） |
| [`tests/define/`](../tests/define/) | 跳转定义测试 Lua fixture（7 个文件） |
| [`tests/parse/`](../tests/parse/) | 解析测试 Lua fixture（2 个文件） |
| [`tests/project/`](../tests/project/) | 多文件工程级测试 Lua fixture（5 个文件） |

## 手工端到端测试

`tests/lua-root/` + `tests/lua-root2/` 一起被 [`tests/mylua-tests.code-workspace`](../tests/mylua-tests.code-workspace) 作为**两个 workspace folder** 挂载，用于在 Extension Development Host 中人工验证 LSP 行为（含跨 workspace 场景）。

**启动方式**：运行 `.cursor/scripts/test-extension.sh`（macOS/Linux）或 `.cursor/scripts/test-extension.ps1`（Windows），脚本会自动构建 LSP + 扩展并打开这两个 workspace。

### `tests/lua-root/` 文件清单

| 文件 | 覆盖场景 |
|------|----------|
| `main.lua` | 入口；require 跳转、module return 类型、跨 workspace require、跨文件全局调用、completion 测试点 |
| `math_utils.lua` | `return M` 模块风格；`@overload` / `@vararg` / `@deprecated` / `@async` / `@nodiscard`；复杂类型 |
| `emmy_basics.lua` | `@class` / `@field` / `@alias` / `@enum` / `@type` |
| `emmy_types.lua` | EmmyLua 类型表达式全覆盖：union、optional、array、泛型、`fun()`、`{k:v}`、括号分组 |
| `player.lua` | OOP：`@class A: B,C` 多继承、self 方法、字段 |
| `scopes.lua` | 作用域树全部 block 类型、参数、vararg、隐式 self、closure |
| `generics.lua` | `@generic T`（函数级）+ `@class C<T>`（容器）+ 泛型参数替换 |
| `diagnostics.lua` | 预期诊断清单（每行 `-- !diag:` 标注） |
| `refs_rename.lua` | references / rename / semantic tokens |
| `json.lua` | 真实第三方库（json4lua）解析健壮性 |
| `UEAnnotation/test_utils.lua` | UE4 场景：多继承、链式调用、UE 风格 stub 重写 |
| `UEAnnotation/ue-comment/ue-comment-xxxxx.lua` | UE4 自动导出风格 |

### `tests/lua-root2/` 文件清单

| 文件 | 覆盖场景 |
|------|----------|
| `shared/config.lua` | 跨 workspace require |
| `shared/logger.lua` | 跨 workspace require + `@overload` 示例 |
| `cross_globals.lua` | 跨 workspace 全局贡献，测试 workspace/symbol + 跨 root goto |

## 独立测试框架

LSP crate 采用 **lib + bin 拆分架构**：`lib.rs` 导出所有核心模块，`main.rs` 仅为薄启动入口。集成测试直接调用核心函数，无需 LSP stdio 通信。

测试工具模块 `test_helpers.rs` 提供：`parse_doc()`、`setup_single_file()`、`setup_workspace_from_dir()` 等函数，可从 `tests/` 下的 Lua fixture 目录构建完整工作区上下文。

> **注意**：如果 VS Code 扩展正在运行会锁住 `mylua-lsp.exe`，可用独立 target 目录避免冲突：
> `$env:CARGO_TARGET_DIR="target-test"; cargo test --tests`

## 集成测试清单

| 测试文件 | 测试数 | 覆盖功能 |
|----------|--------|----------|
| `test_parse.rs` | 8 | 基础解析、EmmyLua 注解、方法链、for 循环、fixture 文件 |
| `test_hover.rs` | 34 | 局部变量、表字面量、EmmyLua class 返回类型、链式调用、块注释文档、函数声明处 hover、点号变量 base/field 区分、链中间字段 AST 驱动 hover、匿名函数展示、多返回值分派、`obj:m()` 消歧、嵌套 shape、CallReturn stub、alias-to-inline-table 字段展开 |
| `test_completion.rs` | 11 | 局部变量补全、点号字段补全、class 方法、关键字、去重、`---@` tag 补全、`require("…")` 模块路径补全 |
| `test_completion_resolve.rs` | 5 | resolve data 携带、global/local detail、keyword 保持原样、function markdown 签名 |
| `test_signature_help.rs` | 13 | 参数签名、参数进度、嵌套 `{}` 逗号、`@overload` 多签名、`:method` self 隐藏、class 消歧、跨文件合并 overloads、匿名函数绑定 |
| `test_goto.rs` | 11 | 局部变量、函数、参数、for 变量、嵌套作用域、require LHS 跳转、attribute 索引偏移、UTF-16 对齐、链式赋值 goto |
| `test_type_definition.rs` | 6 | `@type Foo` 跳到 class、注解内类型名、原始类型回退、`EmmyGeneric`、CallReturn、空文件 |
| `test_workspace_symbol.rs` | 7 | `@field` container_name、`function Foo:m` 拆分、global function、class 搜索、dot-form 方法 |
| `test_rename.rs` | 5 | 局部 rename、非法名拒绝、跨文件全局、Emmy 类名跨文件注解替换、`@field` 名 rename |
| `test_type_dependants.rs` | 9 | 反向依赖注册、re-summary 清除、remove_file 清除、class rename 旧 key 保留 |
| `test_scope.rs` | 11 | 函数体 local、声明站点、参数、for 变量、嵌套遮蔽、`local x = x + 1` 语义、`:method` self |
| `test_diagnostics.rs` | 42 | 语法错误、语义诊断全覆盖（未定义全局、unknown field、type mismatch、duplicate key、unused local、arg count/type、return mismatch、alias 字段、dotted base 未定义等） |
| `test_symbols.rs` | 13 | 函数/方法声明、空文件、@class 层级、dotted LHS 跳过、selection_range 精确化、visibility 关键字 |
| `test_folding_range.rs` | 16 | 空文件、function 体、嵌套 if/for、repeat/while/do、多行 table、块注释、emmy 注释合并、if/elseif/else 分支独立 fold |
| `test_selection_range.rs` | 5 | 空 positions、范围单调外扩、多 position 构链、函数体链延伸、跳过 unnamed token |
| `test_semantic_tokens_delta.rs` | 4 | 相同 token 流、append/delete/中间修改 |
| `test_semantic_tokens_range.rs` | 4 | 范围过滤、delta 编码、范围外返回空、全覆盖等价 full |
| `test_runtime_version.rs` | 5 | 5.3 utf8、5.1 undefinedGlobal、5.2 bit32、luajit bit/jit、unpack 差异 |
| `test_inlay_hint.rs` | 6 | 默认 disabled、参数名、同名跳过、变量类型、`@type` 不重复、范围过滤 |
| `test_call_hierarchy.rs` | 8 | prepare 声明名/调用点/非函数、incoming 单文件/跨文件、outgoing 聚合、dotted/method last_segment、内层匿名函数隔离 |
| `test_shape_owner.rs` | 4 | local/global shape owner、独立 owner、非 table 无 shape |
| `test_emmy_self_and_multireturn.rs` | 8 | class_prefix_of、substitute_self、Builder chain、自由函数保留 self、多返回值 |
| `test_meta.rs` | 6 | `@meta` 识别、名称保留、真实代码后不识别、undefinedGlobal 抑制、global 贡献 |
| `test_diagnostic_suppress.rs` | 9 | disable-next-line、disable-line、disable+enable 区域、通配符、code slug、未知 tag |
| `test_document_link.rs` | 6 | paren/短调用形式、未解析不发、非 require 不发、别名不跟随、多 require |
| `test_document_highlight.rs` | 10 | Read/Write 分类、参数、for 变量、shadowing scope、全局变量、`local x = x + 1`、base READ 分类 |
| `test_references.rs` | 8 | 局部/参数引用、包含/排除声明、shadowing 隔离、Emmy 类型名注解引用、词边界 |
| `test_workspace.rs` | 7 | 多文件 hover/completion/goto、require 解析、全局优先级、upsert 后 require_map 保留 |
| `test_workspace_library.rs` | 5 | stdlib globals 贡献、require 解析到库文件、is_meta 强制、用户文件不误标 |

## 内嵌单元测试

`src/*.rs` 中的 `#[cfg(test)] mod tests`：
- `util.rs`：UTF-8 ↔ UTF-16 列转换、LSP Position 转字节偏移、`apply_text_edit` 单行/跨行编辑
- `lib.rs`：`percent_decode` UTF-8 多字节解码（中文路径）
- `rename.rs`：Lua 标识符校验
- `signature_help.rs`：`count_top_level_commas` 嵌套 `{}`、未终止 `--[[`
- `folding_range.rs`：长括号块注释前缀识别
- `lua_builtins.rs`：5.1/5.2/5.3/5.4/luajit 版本差异 builtin 列表
- `workspace_scanner.rs`：`resolve_library_roots` 空值/missing 路径剔除、canonical 去重
- `emmy.rs`：注解解析

合计 **434 条测试**（`cargo test --tests`）全绿。

## 构建与运行

- 构建：`cd lsp && cargo build`
- 测试：`cd lsp && cargo test --tests`
