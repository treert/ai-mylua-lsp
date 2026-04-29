# CLAUDE.md

## 必读文档

实现/排错/规划前先读 [`ai-readme.md`](ai-readme.md) 和 [`docs/README.md`](docs/README.md)。

## 文档同步（强制）

- 新增/删除/重构 LSP 能力 → 同一次提交更新 `ai-readme.md`
- 改变架构或数据流 → 同一次提交更新 `docs/` 对应文档
- 纯 bug 修复、纯重构、配置微调不需要

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
