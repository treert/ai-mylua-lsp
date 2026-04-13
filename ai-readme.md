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
| [`tests/lua-root/test.lua`](tests/lua-root/test.lua) | 基础测试入口：`require`、EmmyLua `---@class` 注解、成员函数定义 |
| [`tests/lua-root/json.lua`](tests/lua-root/json.lua) | 真实第三方库（json4lua）：闭包模块模式、table 方法、复杂控制流，用于验证解析与索引能力 |

### Grammar — Tree-sitter 解析器（阶段 A 核心）

**BNF 规范**：[`grammar/lua-emmy.bnf`](grammar/lua-emmy.bnf) — Lua 5.3+/5.4 EBNF + EmmyLua 子语法。

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
| [`lsp/crates/mylua-lsp/`](lsp/crates/mylua-lsp/) | LSP server（16 个模块） |

**已实现 LSP 能力**：
- `initialize` / `shutdown` / 文档同步（Full sync）
- **语法诊断**：Tree-sitter ERROR/MISSING 节点自动转为 `publishDiagnostics`
- **documentSymbol**：顶层 function / local / assignment 提取为大纲
- **goto definition**：local 作用域解析（block/function/for 参数）+ 全局符号表 + `require` 跨文件跳转
- **hover**：定义源码展示 + EmmyLua 注解（`@param`/`@return`/`@type`/`@class`）+ 文档注释
- **references**：单文件 local scope 引用 + 全工作区全局符号引用查找
- **workspace/symbol**：全局函数/变量模糊搜索
- **EmmyLua 注解解析**：从 `---` 注释文本提取结构化注解
- **全局符号表**：跨文件全局函数/变量索引 + `require` 路径映射
- **completion**：局部变量 + 全局名 + 关键字自动补全
- **rename**：单文件 local + 全工作区全局符号重命名（含 prepareRename）
- **semantic tokens**：遍历 AST 产出真实 token（函数/变量/参数/关键字/字符串/数字/注释/运算符 + declaration/definition 修饰符）
- **语义诊断**：未定义全局变量 warning（区分 local/builtin/全局符号表）

- 构建：`cd lsp && cargo build`
- 测试：`cargo test`

### VS Code Extension（已实现）

| 文件 | 说明 |
|------|------|
| [`vscode-extension/package.json`](vscode-extension/package.json) | 扩展清单：语言注册、TextMate grammar、配置项 |
| [`vscode-extension/syntaxes/lua.tmLanguage.json`](vscode-extension/syntaxes/lua.tmLanguage.json) | TextMate grammar：关键字、字符串、数字、注释、EmmyLua 注解着色 |
| [`vscode-extension/src/extension.ts`](vscode-extension/src/extension.ts) | LSP 客户端：启动 `mylua-lsp` 二进制（开发时自动查找 `lsp/target/debug/`） |

- 构建：`cd vscode-extension && npm install && npm run compile`
- 调试：F5 启动 Extension Development Host

### 后续
- 5 万文件规模硬化（增量索引、内存优化）。
- rename、completion、code action 等。