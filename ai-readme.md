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

### 后续
- 定稿 **LSP 实现语言等** 技术栈后，在 `grammar/` / `lsp/` / `vscode-extension/` 内落地实现与单仓 CI。