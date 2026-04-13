# vscode-extension

VS Code 扩展：自研 **TextMate** 基色高亮、语言配置、**启动** 同仓库构建的 `mylua-lsp` 二进制。

## 功能

- **TextMate 语法高亮**：Lua 5.3+ 关键字、字符串、数字、注释（含 EmmyLua `---@` 注解标注）
- **语言配置**：括号匹配、自动闭合、折叠、缩进规则
- **LSP 客户端**：通过 stdio 启动 `mylua-lsp`，自动获得诊断 / 大纲 / 跳转 / 悬浮 / 引用 / 全库搜索等能力

## 开发

```bash
cd vscode-extension
npm install
npm run compile

# 需先构建 LSP server:
cd ../lsp && cargo build
```

在 VS Code 中按 F5 启动 Extension Development Host 调试。

扩展通过 `mylua.server.path` 配置项指定 LSP 二进制路径；缺省时自动查找 `../lsp/target/debug/mylua-lsp`。

## 约定

- **不做**语言理解；解析与语义均在 `../lsp`。
- TextMate grammar 仅负责基色高亮，语义着色由 LSP semantic tokens 叠加。
