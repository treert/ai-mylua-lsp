# AI 会话入口（必读）

**本文件面向 AI 助手与人类协作者：在新对话或接手本仓库时，请先阅读本文，再按需深入 `docs/` 目录。**

## 强制规则（给 AI）

1. **在回答与本项目相关的实现、排错、重构或规划前**，应阅读本文件（`ai-readme.md`）与 [`docs/README.md`](docs/README.md)，并按主题查阅 [`docs/`](docs/) 下对应文档。
2. **修改架构、图层、数据路径或依赖时**，同步更新 `docs/` 中相关文档（跨文件索引见 [`docs/lsp-semantic-spec.md`](docs/lsp-semantic-spec.md)），避免文档与代码脱节。

## 项目目标
实现 lua vscode 插件，支持语法高亮，语义跳转，hover tips, 诊断，outline 等功能。
**需要支持 emmylua 类型的类型注释。**
**仅支持 Lua 5.3 及以上版本。**
对性能有较高要求，需要支持5万个lua文件级别。

**方案取向（需求分析阶段）**

- **全工作区能力**：定义、**所有引用**、**工作区符号** 均为硬性目标，而非「仅打开文件」级能力。
- **解析与高亮**：**自研 Tree-sitter** 置于 **LSP** 内，负责 **语法树** 与增量解析；**基色高亮**以 **自研 TextMate** 为主；**LSP semantic tokens** 在 TextMate 之上叠加语义着色（如全局/局部等），与 Tree-sitter **不冲突、分工不同**。
- **分体工程**：**VS Code Extension** 与 **LSP Server** **分开实现、可分开发布**，可并行开发；LSP 可独立服务其他编辑器或工具。
- **仓库**：**Monorepo**（单仓）管理文法、LSP、扩展等，详见 [`docs/implementation-roadmap.md`](docs/implementation-roadmap.md) §2。

## 开发进度

### 需求分析
- 文档见 [`docs/README.md`](docs/README.md)（需求、架构、路线图与技术倾向）。

### Monorepo 骨架
- 已按计划创建顶层目录：`grammar/`、`lsp/`、`vscode-extension/`（各含 README）；根目录 [`README.md`](README.md) 说明布局。

### 测试与资源文件

| 路径 | 用途 |
|------|------|
| [`assets/lua5.4/`](assets/lua5.4/) | Lua 5.4 标准库 EmmyLua 类型注释（`basic.lua`、`string.lua`、`table.lua`、`math.lua`、`io.lua`、`os.lua` 等 11 个文件），作为内置类型定义的参考来源 |
| [`tests/lua-root/`](tests/lua-root/) | **手工端到端测试目录**（详见下方说明）：用于在 Extension Development Host 中实际体验 LSP 能力 |
| [`tests/complete/`](tests/complete/) | 补全功能测试 Lua fixture（17 个文件）：局部变量、表字段、require、class 方法、智能补全 |
| [`tests/hover/`](tests/hover/) | Hover 功能测试 Lua fixture（18 个文件）：EmmyLua class、链式调用、require 模块、表展开、函数返回类型 |
| [`tests/define/`](tests/define/) | 跳转定义测试 Lua fixture（7 个文件）：局部/全局定义、require 跳转、文件夹 init.lua |
| [`tests/parse/`](tests/parse/) | 解析测试 Lua fixture（2 个文件）：语法错误恢复、各种语句类型 |
| [`tests/project/`](tests/project/) | 多文件工程级测试 Lua fixture（5 个文件）：全局变量跨文件、枚举段 |

#### `tests/lua-root/` — 手工端到端测试目录

此目录用作 **Extension Development Host 的工作区**，开发者在其中打开 `.lua` 文件来实际体验 LSP 效果（高亮、hover、补全、跳转、诊断等）。与 `tests/` 下其他目录的区别：其他目录是 Rust 集成测试读取的 fixture，而 `lua-root` 是人手动操作的端到端体验环境。

**启动方式**：运行 `.cursor/scripts/test-extension.sh`（macOS/Linux）或 `.cursor/scripts/test-extension.ps1`（Windows），脚本会自动构建 LSP + 扩展并打开此目录。参见 Skill `.cursor/skills/test-extension/`。

**目录结构与测试重点**：

| 文件 | 覆盖场景 |
|------|----------|
| `test.lua` | `require` 跳转（json4lua）、`---@class` 声明 + 成员方法 |
| `json.lua` | 真实第三方库（json4lua），验证无语法错误解析 |
| `test_lua_helper.lua` | `---@class` + `---@field`、`---@type` 类型断言、`self` 字段推断、跨文件 `UE4.UMiscSystemLibrary` 链式调用、未定义字段诊断 |
| `test_lua_2.lua` | 跨文件 `ABC` class 成员方法 + 字段赋值、全局 dotted 表达式 |
| `UEAnnotation/test_utils.lua` | 多 class 继承（`T3: T1,T2`）、`---@return` 链式调用、UE 风格 stub 重写（`UMiscSystemLibrary_` 继承 `UMiscSystemLibrary`）|
| `UEAnnotation/ue-comment/ue-comment-xxxxx.lua` | UE4 自动导出注释风格（`--[[ ]]` + `---@class` 继承链）、子目录 require 解析 |

**特殊模式**：
- **UE4 全局表** — `UE4.UMiscSystemLibrary` 模拟 Unreal Engine Lua 绑定的多层 dotted 全局访问，测试跨文件字段解析和类型推断
- **Class 继承链** — `UMiscSystemLibrary_ : UMiscSystemLibrary`、`T3 : T1, T2`，测试多继承和方法/字段解析
- **未定义字段** — `x.no_exist`、`ttt1:f333()` 等，测试语义诊断是否正确报 warning/error
- **跨文件 self 推断** — `ABC` class 在 `test_lua_helper.lua` 定义，在 `test_lua_2.lua` 扩展方法和字段

### Grammar — Tree-sitter 解析器（阶段 A 核心）

**BNF 规范**：[`grammar/lua.bnf`](grammar/lua.bnf)（Lua 5.3+/5.4 EBNF）+ [`grammar/emmy.bnf`](grammar/emmy.bnf)（EmmyLua 注解子语法）。

**解析器实现**（已完成并通过验证）：

| 文件 | 说明 |
|------|------|
| [`grammar/grammar.js`](grammar/grammar.js) | Tree-sitter 文法：15 种语句、12 级优先级表达式、table/function/prefix 完整语法；EmmyLua 注解产生式已定义 |
| [`grammar/src/scanner.c`](grammar/src/scanner.c) | 外部扫描器：短字符串（全部 Lua 5.3+ 转义）、长字符串、所有注释类型、shebang、**column-0 块边界** |
| [`grammar/test/corpus/`](grammar/test/corpus/) | 37 个回归测试，100% 通过 |

**定制扩展 — Column-0 块边界**：行首 column 0 处的关键字/标识符强制关闭未配对的嵌套块，让缺少 `end` 的错误在下一个顶层语句处即时报出。嵌套代码必须缩进。详见 BNF §2.1.1。

- 无错误解析验证：`tests/lua-root/test.lua`、`tests/lua-root/json.lua`、`assets/lua5.4/` 全部 11 个标准库桩文件。
- 命令：`cd grammar && npm install && npx tree-sitter generate && npx tree-sitter test`

### LSP — Rust 语言服务器（阶段 C 完成）

**技术栈**：Rust + `tower-lsp-server` 0.23 + `tree-sitter` 0.26 + `tokio`。

| 路径 | 说明 |
|------|------|
| [`lsp/Cargo.toml`](lsp/Cargo.toml) | Cargo workspace root |
| [`lsp/crates/tree-sitter-mylua/`](lsp/crates/tree-sitter-mylua/) | 包装 crate：`build.rs` 编译 `grammar/src/` 的 C parser，导出 `LANGUAGE` |
| [`lsp/crates/mylua-lsp/`](lsp/crates/mylua-lsp/) | LSP server（lib + bin 架构，24 个模块 + 9 个集成测试文件） |

**已实现 LSP 能力**：
- `initialize` / `shutdown` / 文档同步（Full sync）
- **配置体系**：15 个扩展配置项，通过 `initializationOptions` + `didChangeConfiguration` 下发
- **语法诊断**：Tree-sitter ERROR/MISSING 节点自动转为 `publishDiagnostics`
- **documentSymbol**：顶层 function / local / assignment 提取为大纲
- **goto definition**：local 作用域 + 全局符号表 + `require` 跳转（到首个全局贡献位置）+ **字段表达式跨文件跳转** + **Emmy 类型名跳转**（`type_shard`）+ **链式 field_expression 递归解析**（`a.b.c`）
- **hover**：定义源码 + EmmyLua 注解 + 文档注释 + **推断类型展示**（链式字段解析）+ **Emmy 类型 hover**（class/alias/enum 区分展示、字段列表）+ **全局多候选提示**
- **references**：单文件 local scope + 全工作区全局符号引用（`global_shard` + `type_shard` 声明 + 去重）
- **workspace/symbol**：全局函数/变量 + **Emmy class/alias/enum**（`type_shard`）模糊搜索
- **EmmyLua 注解**：递归下降解析器（`emmy.rs`），完整支持类型表达式语法（union `|`、optional `?`、array `[]`、generic `<T>`、`fun()` 函数类型、`{k:v}` table 类型、括号分组）；注解标签 `@class`/`@field`/`@param`/`@return`/`@type`/`@alias`/`@generic`/`@overload`/`@vararg`/`@deprecated`/`@async`/`@nodiscard` 等；**泛型参数保留**（`EmmyGeneric` 变体）；**`@overload` 参与 FunctionSummary**；**`@alias` 右侧类型保存和展开**
- **completion**：局部变量 + 全局名 + 关键字 + **点号字段补全** + **冒号补全过滤方法** + **链式 dotted base 解析**
- **rename**：单文件 local + 全工作区全局（含 prepareRename）
- **semantic tokens**：全局变量 `defaultLibrary` + 局部变量标记（作用域感知）
- **语义诊断**：未定义全局变量 + **Emmy 类型未知字段访问** warning（severity 可配置）；**诊断 enable/severity 受配置控制**
- **作用域树**（`scope.rs`）：arena-based `ScopeTree`，单趟 AST 遍历构建；支持 `function_body` / `do` / `while` / `repeat` / `if` / `for` 等所有块级作用域 + 参数 + for 变量 + 隐式 `self`；正确处理 `local x = x + 1` RHS 引用外层变量的 Lua 语义

**索引架构（步骤 1-6）**：
- `summary_builder.rs`：单文件 AST → DocumentSummary；支持文件级 `return` 语句提取（`module_return_type`）、递归进入 `if`/`do`/`for`/`while` 块（含 local/emmy_comment）收集全局赋值和函数声明、全局函数 `GlobalContribution` 携带真实 `FunctionSignature`、**冒号方法调用生成 CallReturn stub**、**Known(EmmyType) base 生成 TypeRef**
- `aggregation.rs`：WorkspaceAggregation（GlobalShard / TypeShard / RequireByReturn）；同名全局候选按 URI 路径深度优先排序（浅路径 > 深路径）；`resolve_module_to_uri` 优先查 `require_map`
- `resolver.rs`：跨文件 stub 链式解析 + 缓存 + 环路保护；`resolve_require` 基于目标文件 `module_return_type` 解析模块返回值类型；`resolve_field_chain` 对 table-extension 全局变量支持 global_shard 限定名回退；Emmy 继承链字段解析（沿 `parents` 递归）+ **alias 类型展开**（`resolve_emmy_field` 自动跟踪 alias 目标）；`collect_fields` / `resolve_table_field` 强制 `source_uri`；**EmmyGeneric 类型的字段/补全支持**
- `summary.rs`：`DocumentSummary` 含 `module_return_type`；`TypeDefinition` 含 `parents`（继承链）、`alias_type`（别名目标）；`FunctionSummary` 含 `overloads`；签名指纹包含全局类型信息和 module return
- 设计文档：[`docs/index-architecture.md`](docs/index-architecture.md) / [`docs/index-implementation-plan.md`](docs/index-implementation-plan.md)

- 构建：`cd lsp && cargo build`
- 测试：`cd lsp && cargo test --tests`

**独立测试框架**（无需 VS Code 联调）：

LSP crate 采用 **lib + bin 拆分架构**：`lib.rs` 导出所有核心模块（hover / completion / goto / diagnostics 等），`main.rs` 仅为薄启动入口。集成测试直接调用核心函数，无需 LSP stdio 通信。

| 测试文件 | 测试数 | 覆盖功能 |
|----------|--------|----------|
| `test_parse.rs` | 8 | 基础解析、EmmyLua 注解、方法链、for 循环、fixture 文件 |
| `test_hover.rs` | 14 | 局部变量、表字面量、EmmyLua class 返回类型、链式调用、块注释文档、函数声明处 hover、点号变量 base/field 区分 |
| `test_completion.rs` | 7 | 局部变量补全、点号字段补全、class 方法、关键字、去重 |
| `test_goto.rs` | 6 | 局部变量、函数、参数、for 变量、嵌套作用域跳转 |
| `test_scope.rs` | 11 | 函数体内 local 解析、声明站点、参数、for 变量、嵌套遮蔽、`local x = x + 1` 语义、`:method` self、visible_locals |
| `test_diagnostics.rs` | 4 | 干净代码无诊断、语法错误检测、语义诊断 |
| `test_symbols.rs` | 5 | 函数声明、方法声明、空文件、fixture 文件 |
| `test_references.rs` | 4 | 局部引用、参数引用、包含/排除声明选项 |
| `test_workspace.rs` | 5 | 多文件 hover/completion/goto、require 解析、project 目录 |

测试工具模块 `test_helpers.rs` 提供：`parse_doc()`、`setup_single_file()`、`setup_workspace_from_dir()` 等函数，可从 `tests/` 下的 Lua fixture 目录构建完整工作区上下文。

> **注意**：如果 VS Code 扩展正在运行会锁住 `mylua-lsp.exe`，可用独立 target 目录避免冲突：
> `$env:CARGO_TARGET_DIR="target-test"; cargo test --tests`

### VS Code Extension（已实现）

| 文件 | 说明 |
|------|------|
| [`vscode-extension/package.json`](vscode-extension/package.json) | 扩展清单：语言注册、TextMate grammar、配置项 |
| [`vscode-extension/syntaxes/lua.tmLanguage.json`](vscode-extension/syntaxes/lua.tmLanguage.json) | TextMate grammar：Lua 基础语法（关键字、字符串、数字、注释）+ 完整 EmmyLua 注解着色（16 种 `@tag` 结构化匹配、`fun()`/`{}`/`()` 嵌套类型表达式、内置类型 vs 用户类型区分、点号类型名） |
| [`vscode-extension/src/extension.ts`](vscode-extension/src/extension.ts) | LSP 客户端：启动 `mylua-lsp` 二进制（开发时自动查找 `lsp/target/debug/`） |

- 构建：`cd vscode-extension && npm install && npm run compile`
- 调试：F5 启动 Extension Development Host

### 后续（步骤 7）
- 索引状态机（Initializing/Ready + 进度通知）。
- 并行冷启动 + 诊断调度。
- 磁盘持久化缓存 + 类型感知诊断。
- 5 万文件规模硬化。