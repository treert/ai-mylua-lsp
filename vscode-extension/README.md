# MyLua LSP

MyLua LSP 为 VS Code 提供 Lua 语言支持，内置 `mylua-lsp` 语言服务器，并支持 EmmyLua 注解。

## 主要功能

- **语法高亮**：Lua 关键字、字符串、数字、注释与 EmmyLua `---@` 注解高亮
- **语言服务**：诊断、悬浮提示、跳转定义、查找引用、大纲、工作区符号搜索
- **语义增强**：Lua 标准库符号识别、全局变量提示、EmmyLua 类型检查
- **开箱即用**：扩展包内置对应平台的 `mylua-lsp`，一般无需额外配置

## 快速开始

1. 安装扩展后打开 `.lua` 文件。
2. 等待状态栏中的 MyLua 索引完成提示。
3. 直接使用 VS Code 的跳转、悬浮、引用、诊断和大纲等能力。

## 常用配置

在 VS Code `settings.json` 中可按需调整：

```jsonc
{
  // Lua 运行时版本；目前内置 stdlib stubs 默认覆盖 5.4
  "mylua.runtime.version": "5.4",

  // 额外注解库，例如 LÖVE、OpenResty 或项目内部 SDK 的 EmmyLua stubs
  "mylua.workspace.library": ["./typings"],

  // 是否自动加载扩展内置 Lua 标准库注解
  "mylua.workspace.useBundledStdlib": true,

  // 诊断范围：full 诊断整个工作区；openOnly 只诊断已打开文件
  "mylua.diagnostics.scope": "full"
}
```

如果需要使用自定义语言服务器二进制，可设置 `mylua.server.path`：

```jsonc
{
  "mylua.server.path": {
    "win32": "C:/tools/mylua-lsp.exe",
    "darwin": "/usr/local/bin/mylua-lsp",
    "linux": "/usr/local/bin/mylua-lsp"
  }
}
```

## EmmyLua 注解支持

扩展会自动加载内置 Lua 标准库注解，让 `print`、`string.format`、`table.concat`、`io.open` 等标准库符号获得悬浮、跳转、签名帮助与补全能力。

你也可以通过 `mylua.workspace.library` 添加第三方或项目私有注解库。支持绝对路径、`~/...`，以及相对首个工作区根目录的路径。

## 常见问题

### 没有诊断或跳转结果？

- 确认当前文件语言模式是 `Lua`。
- 等待工作区索引完成。
- 检查 `mylua.workspace.include` / `mylua.workspace.exclude` 是否排除了目标文件。

### 想减少大型项目的诊断开销？

可以将诊断范围改为只处理已打开文件：

```jsonc
{
  "mylua.diagnostics.scope": "openOnly"
}
```

## 反馈

问题与建议请提交到仓库 Issues：<https://github.com/treert/ai-mylua-lsp/issues>

开发、构建和发布说明见仓库文档：<https://github.com/treert/ai-mylua-lsp/blob/master/docs/vscode-extension.md>

