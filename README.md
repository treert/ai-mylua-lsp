# ai-mylua-lsp

Lua 5.3+ 语言支持：**自研 Tree-sitter 文法**、**独立 LSP**、**VS Code 扩展**，以 **Monorepo** 管理。

- 协作与 AI 必读：[ai-readme.md](ai-readme.md)
- 需求与架构：[docs/README.md](docs/README.md)

## 布局

| 目录 | 说明 |
|------|------|
| [grammar/](grammar/) | Tree-sitter 文法（`tree-sitter test`，parser 供 LSP 链入） |
| [lsp/](lsp/) | 语言服务器实现与构建产物 |
| [vscode-extension/](vscode-extension/) | VS Code 扩展（TextMate、拉起 LSP、配置） |
| [docs/](docs/) | 需求、架构、路线图 |

具体约定见 [docs/implementation-roadmap.md](docs/implementation-roadmap.md) §2。
