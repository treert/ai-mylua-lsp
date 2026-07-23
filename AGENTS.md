# AGENTS.md

**本文件面向 AI 助手与人类协作者：在新对话或接手本仓库时，请先阅读本文，再按需深入 `docs/` 目录。**

## 强制规则（给 AI）

1. **在回答与本项目相关的实现、排错、重构或规划前**，应阅读本文件（`AGENTS.md`）与 [`docs/README.md`](docs/README.md)，并按主题查阅 [`docs/`](docs/) 下对应文档。
2. **修改架构、图层、数据路径或依赖时**，同步更新 `docs/` 中相关文档（跨文件索引见 [`docs/lsp-semantic-spec.md`](docs/lsp-semantic-spec.md)），避免文档与代码脱节。

## 文档同步（强制）

- 新增/删除/重构 LSP 能力（如 semantic tokens、completion、diagnostics）→ 同一次提交更新 [`docs/lsp-capabilities.md`](docs/lsp-capabilities.md)
- 改变架构或数据流（如索引策略、模块边界）→ 同一次提交更新 `docs/architecture.md` 相关章节
- 改变实现路线或阶段完成状态 → 更新 `docs/implementation-roadmap.md`

不需要更新文档的场景：bug 修复（不改变功能描述）、纯重构（对外行为不变）、配置微调。

## 完成代码修改后

功能性改动完成后、告知用户之前：

1. `cd lsp && cargo build`（零 error）
2. 涉及 TS：`cd vscode-extension && npm run compile`
3. 调用 code-reviewer 子代理

跳过：单行修改、注释/文档、配置微调。

## Gotchas

- 改 `grammar.js` 后必须先 `tree-sitter generate` 再 `cargo build`
- 平台二进制名：win32 → `mylua-lsp.exe`，其他 → `mylua-lsp`
- 多平台发布走 `.github/workflows/release.yml`

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
├── tests/            # 手工测试用 Lua 文件（可随意增删改，Rust 测试不依赖此目录）
└── docs/             # 设计文档中心
```

## 开发进度

| 阶段 | 状态 | 说明 |
|------|------|------|
| A — Grammar | ✅ 完成 | Tree-sitter 文法 + 外部扫描器 |
| B — LSP 骨架 | ✅ 完成 | Rust + tower-lsp-server + 增量解析 |
| C — 全功能 LSP | ✅ 完成 | 30+ LSP 能力，全工作区索引 |
| D — VS Code Extension | ✅ 完成 | TextMate 着色 + LSP 客户端 + 打包发布 |

## 功能索引

| 模块 | 说明 | 详细文档 |
|------|------|---------|
| Grammar | Tree-sitter 文法 + 外部扫描器 | [`grammar/`](grammar/) |
| LSP 能力 | 30+ 能力（导航/着色/诊断/EmmyLua 注解等） | [`docs/lsp-capabilities.md`](docs/lsp-capabilities.md) |
| 索引架构 | 冷启动流水线、模块解析、并发安全 | [`docs/index-architecture.md`](docs/index-architecture.md) |
| 性能分析 | 5 万文件级目标与瓶颈 | [`docs/performance-analysis.md`](docs/performance-analysis.md) |
| VS Code 扩展 | TextMate 着色 + LSP 客户端 | [`docs/vscode-extension.md`](docs/vscode-extension.md) |
| 测试 | 460+ 条集成测试 | [`docs/testing.md`](docs/testing.md) |

完整文档目录见 [`docs/README.md`](docs/README.md)。
