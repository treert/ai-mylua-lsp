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

**方案取向**：
- **全工作区能力**：定义、所有引用、工作区符号均为硬性目标
- **解析与高亮**：自研 Tree-sitter（LSP 内）+ 自研 TextMate（基色）+ LSP semantic tokens（语义着色）
- **分体工程**：VS Code Extension 与 LSP Server 分开实现、可分开发布
- **Monorepo**：单仓管理文法、LSP、扩展

## 仓库结构

```
ai-mylua-lsp/
├── grammar/          # Tree-sitter 解析器（BNF + scanner.c + 测试）
├── lsp/              # Rust LSP Server（Cargo workspace）
│   └── crates/
│       ├── tree-sitter-mylua/   # Tree-sitter 包装 crate
│       └── mylua-lsp/           # LSP server 主 crate
├── vscode-extension/ # VS Code 扩展（TypeScript）
├── tests/            # 测试 fixture + 端到端测试目录
└── docs/             # 设计文档中心
```

## 开发进度

| 阶段 | 状态 | 说明 |
|------|------|------|
| A — Grammar | ✅ 完成 | Tree-sitter 文法 + 外部扫描器 |
| B — LSP 骨架 | ✅ 完成 | Rust + tower-lsp-server + 增量解析 |
| C — 全功能 LSP | ✅ 完成 | 30+ LSP 能力，全工作区索引 |
| D — VS Code Extension | ✅ 完成 | TextMate 着色 + LSP 客户端 + 打包发布 |

## 已实现 LSP 能力一览

> 详细实现描述见 [`docs/lsp-capabilities.md`](docs/lsp-capabilities.md)

**基础**：initialize / shutdown / 文档同步（Incremental）/ 位置编码（UTF-16）/ 配置体系（20 项）/ 外部库索引（`workspace.library`）

**导航**：goto definition / goto declaration / goto typeDefinition / references / rename / callHierarchy / documentLink

**信息展示**：hover / signatureHelp / inlayHint

**符号与大纲**：documentSymbol / workspace/symbol

**语法着色**：semantic tokens（full / range / delta）

**编辑器辅助**：completion（+ resolve）/ selectionRange / foldingRange / documentHighlight

**诊断**：语法诊断 / 语义诊断（undefinedGlobal / unknownField / typeMismatch / duplicateTableKey / unusedLocal / argumentCount / argumentType / returnMismatch）/ `---@meta` 元文件 / `---@diagnostic` 抑制

**EmmyLua 注解**：完整类型表达式 / @class / @field / @param / @return / @type / @alias / @enum / @generic / @overload / self 泛型绑定 / 多返回值

**自定义通知**：`mylua/indexStatus`（索引进度）

## Grammar — Tree-sitter 解析器

| 文件 | 说明 |
|------|------|
| [`grammar/grammar.js`](grammar/grammar.js) | Tree-sitter 文法：15 种语句、12 级优先级表达式 |
| [`grammar/src/scanner.c`](grammar/src/scanner.c) | 外部扫描器：短/长字符串、注释、shebang |
| [`grammar/test/corpus/`](grammar/test/corpus/) | 36 个回归测试 |
| [`grammar/lua.bnf`](grammar/lua.bnf) | Lua 5.3+/5.4 EBNF |
| [`grammar/emmy.bnf`](grammar/emmy.bnf) | EmmyLua 注解子语法 |

命令：`cd grammar && npm install && npx tree-sitter generate && npx tree-sitter test`

## LSP — Rust 语言服务器

**技术栈**：Rust + `tower-lsp-server` 0.23 + `tree-sitter` 0.26 + `tokio`。

> 能力实现细节见 [`docs/lsp-capabilities.md`](docs/lsp-capabilities.md)
> 索引架构见 [`docs/index-architecture.md`](docs/index-architecture.md)
> 性能分析见 [`docs/performance-analysis.md`](docs/performance-analysis.md)

**关键架构特性**：
- **索引状态机**：`Initializing` → `Ready`，4 阶段冷启动流水线：scan → rayon 全量并行 parse → 原子 `build_initial` 构建全局索引 → Ready；`mylua/indexStatus` 通知携带 `phase` 字段（scanning / parsing / merging）
- **增量解析**：tree-sitter `tree.edit` + `parse(new, Some(old))`
- **并发安全**：per-URI `edit_locks`，锁顺序 `edit_locks` → `open_uris` → `documents` → `index` → `scheduler.inner`
- **诊断调度**：`DiagnosticScheduler` 统一管理，300ms debounce，hot/cold 双队列
- **磁盘持久化缓存**：`CacheMeta` 三维失效（默认纯内存模式）
- **文件过滤**：`workspace.include` / `workspace.exclude` glob

命令：
- 构建：`cd lsp && cargo build`
- 测试：`cd lsp && cargo test --tests`（434 条测试）

> 测试清单见 [`docs/testing.md`](docs/testing.md)

## VS Code Extension

> 详细说明见 [`docs/vscode-extension.md`](docs/vscode-extension.md)

**核心功能**：LSP 客户端 + TextMate 语法着色 + 索引状态 StatusBar + 内置 stdlib stubs

命令：
- 构建：`cd vscode-extension && npm install && npm run compile`
- 调试：F5 启动 Extension Development Host
- 打包：`cd vscode-extension && npm run build:local`

## 测试

> 完整测试清单见 [`docs/testing.md`](docs/testing.md)

- **集成测试**：28 个测试文件，434 条测试，覆盖所有 LSP 能力
- **手工端到端**：`tests/lua-root/` + `tests/lua-root2/`（多 workspace 场景）
- **启动方式**：`.cursor/scripts/test-extension.sh`（macOS/Linux）或 `.cursor/scripts/test-extension.ps1`（Windows）

## 文档索引

| 文档 | 内容 |
|------|------|
| [`docs/README.md`](docs/README.md) | 文档中心索引 |
| [`docs/lsp-capabilities.md`](docs/lsp-capabilities.md) | **LSP 能力详细实现**：每个能力的内部机制、边界处理、配置项 |
| [`docs/testing.md`](docs/testing.md) | **测试体系**：测试框架、测试资源、完整测试清单 |
| [`docs/vscode-extension.md`](docs/vscode-extension.md) | **VS Code 扩展**：文件结构、构建打包、运行时行为 |
| [`docs/requirements.md`](docs/requirements.md) | 功能/非功能需求 |
| [`docs/architecture.md`](docs/architecture.md) | Extension / LSP / Grammar 三分解、数据流 |
| [`docs/index-architecture.md`](docs/index-architecture.md) | 索引内部架构：数据模型、推断、类型、构建与维护 |
| [`docs/lsp-semantic-spec.md`](docs/lsp-semantic-spec.md) | LSP 语义能力：语义约定、消费规则 |
| [`docs/implementation-roadmap.md`](docs/implementation-roadmap.md) | 阶段门禁、Monorepo 布局、技术栈 |
| [`docs/performance-analysis.md`](docs/performance-analysis.md) | 性能现状、瓶颈分析、优化路线图 |
| [`docs/index-implementation-plan.md`](docs/index-implementation-plan.md) | 索引架构落地实施步骤（历史归档） |
| [`docs/future-work.md`](docs/future-work.md) | 后续待办与优化方向（索引坑点 + 泛型缺口 + 维护清单） |
| [`docs/col0-block-end-redesign.md`](docs/col0-block-end-redesign.md) | Column-0 块边界重设计讨论（WIP） |
