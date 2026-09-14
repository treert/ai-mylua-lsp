# MyLua LSP

MyLua LSP 为 VS Code 提供 Lua 语言支持，内置 `mylua-lsp` 语言服务器，并支持 EmmyLua 注解。

## 特点

- **Rust 实现，启动快**：语言服务器用纯 Rust 编写，原生多线程并发解析；在 2 万+ Lua 文件的工程上，打开后约 3 秒即可完成索引初始化（具体取决于硬件与文件分布）。
- **大规模工程不卡顿**：增量解析只重算变动子树，编辑响应稳定；全工作区索引常驻内存，跳转、引用、符号搜索毫秒级返回。

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

所有配置均有默认值，开箱即用。以下是最常调整的几项：

```jsonc
{
  // 诊断范围：full（默认）诊断整个工作区；
  // 大型项目可改为 "openOnly"，只诊断已打开的文件，避免大量警告堆积。
  "mylua.diagnostics.scope": "full",

  // 额外的 EmmyLua 注解库目录（LÖVE、OpenResty、内部 SDK 等），
  // 支持绝对路径、~/… 与相对首个工作区根目录的路径
  "mylua.workspace.library": ["./typings"],

  // 觉得内嵌提示太吵时关闭
  "mylua.inlayHint.enable": true
}
```

### 两个不常用但影响较大的配置

- **`mylua.require.aliases`** — require 路径别名（点分前缀匹配，最长前缀优先）。项目里用了 `require('@utils.foo')` 这类别名时必须配置，否则别名模块的跳转与返回值类型推断都无法解析。例：`{ "@": "src", "@utils": "src.utils" }`
- **`mylua.performance.slowParseKeepTreeThresholdMs`** — 冷启动扫描期间的语法树保留阈值（默认 500ms）：扫描时只有解析慢于该值的文件保留语法树，其余只留索引摘要，因此初始内存占用较低。注意它**只影响初始扫描**——之后使用查找引用等需要语法树的功能时，会重新解析并常驻内存，**第一次查找引用比较慢**。内存占用可在状态栏 mylua 的tip里看到，上限约为源码总量的 30 倍（索引信息约 10 倍 + 语法树约 20 倍）。**如果你的项目文件数量较少，降低这个值到 0，就会全部缓存语法树。**

Lua 版本、诊断细项等全部配置见[配置参考](./CONFIGURATION.md)。

## EmmyLua 注解支持

扩展会自动加载内置 Lua 标准库注解，让 `print`、`string.format`、`table.concat`、`io.open` 等标准库符号获得悬浮、跳转、签名帮助与补全能力。

你也可以通过 `mylua.workspace.library` 添加第三方或项目私有注解库。支持绝对路径、`~/...`，以及相对首个工作区根目录的路径。

## 常见问题

### 没有诊断或跳转结果？

- 确认当前文件语言模式是 `Lua`。
- 等待工作区索引完成。
- 检查 `mylua.workspace.include` / `mylua.workspace.exclude` 是否排除了目标文件。

### 某些 `.lua` 文件没有被索引？

MyLua LSP 支持 **UTF-8**（含 BOM）和 **UTF-16**（LE/BE，需 BOM）编码的 Lua 源文件。以下文件在工作区扫描时会被跳过：

- 编译后的 Lua 字节码（`luac` 输出）
- 使用 GBK 等不支持的编码保存的文件

开启 `mylua.debug.fileLog` 后可在 `.vscode/mylua-lsp.log` 中查看被跳过的文件列表。

## 反馈

问题与建议请提交到仓库 Issues：<https://github.com/treert/ai-mylua-lsp/issues>

