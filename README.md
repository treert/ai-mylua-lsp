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

## 快速构建

首次构建前需要安装 Node.js/npm 与 Rust 工具链。推荐从仓库根目录按顺序执行：

```bash
(cd grammar && npm install && npx tree-sitter generate && npx tree-sitter test)
(cd lsp && cargo build && cargo test --tests)
(cd vscode-extension && npm install && npm run compile)
```

调试 VS Code 扩展时，先确保 `lsp/target/debug/mylua-lsp`（Windows 为 `mylua-lsp.exe`）已由 `cargo build` 生成，然后在 VS Code 中按 F5 启动 Extension Development Host。

也可以直接用仓库内脚本构建并拉起测试环境（默认打开 `tests/lua-root`）：

```bash
# macOS / Linux
.cursor/scripts/test-extension.sh

# Windows PowerShell
.cursor/scripts/test-extension.ps1
```

常用参数：`--skip-build` / `-SkipBuild` 跳过构建，`--release` / `-Release` 使用 release LSP，`-w` 打开 `tests/mylua-tests.code-workspace` 多 workspace 测试。

本地打包 `.vsix`：

```bash
cd vscode-extension
npm run build:local
```

更多细节见 [grammar/README.md](grammar/README.md)、[lsp/README.md](lsp/README.md) 与 [docs/vscode-extension.md](docs/vscode-extension.md)。
