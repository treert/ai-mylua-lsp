# 配置参考

MyLua LSP 的全部配置项。所有配置均有默认值，可在 VS Code 设置界面搜索 `mylua` 查看并修改，也可直接写入 `settings.json`。

诊断严重级别取值统一为：`error` / `warning` / `hint` / `off`。`hint` 只在编辑器中下划线标注，不计入 Problems 面板的错误/警告数。

## 诊断

### 总开关与范围

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.diagnostics.enable` | bool | `true` | 启用语义诊断。语法错误诊断始终报告，不受此项影响 |
| `mylua.diagnostics.scope` | string | `"full"` | 诊断范围。`full`：诊断整个工作区（冷启动全量，级联所有依赖方）；`openOnly`：只诊断当前打开的文件，级联跳过未打开文件，适合大型项目降低开销 |

### 检查项严重级别

| 配置 | 默认值 | 检查内容 |
|------|--------|---------|
| `mylua.diagnostics.undefinedGlobal` | `warning` | 读取未定义的全局变量 |
| `mylua.diagnostics.emmyTypeMismatch` | `warning` | 值与 EmmyLua 类型注解不匹配 |
| `mylua.diagnostics.emmyUnknownField` | `warning` | 访问 Emmy 类型值的未知字段 |
| `mylua.diagnostics.luaFieldError` | `warning` | 高置信度字段错误（如索引 nil） |
| `mylua.diagnostics.luaFieldWarning` | `warning` | 保守字段警告（如开放表上的未知字段） |
| `mylua.diagnostics.envUnknownField` | `warning` | `_ENV` 重定向后读取环境中不存在的名字。仅在环境表形状完全已知时触发 |
| `mylua.diagnostics.duplicateTableKey` | `warning` | 同一表构造器中的重复键（`{ a = 1, a = 2 }`） |
| `mylua.diagnostics.unusedLocal` | `hint` | 声明后未读取的局部变量。`_` / `_` 前缀名始终跳过 |
| `mylua.diagnostics.argumentCountMismatch` | `hint` | 调用实参数量与签名不符。变参 `...` 吸收多余实参，命中任一 `---@overload` 数量即抑制。默认 `hint`：省略尾参（自动补 `nil`）是 Lua 惯用法，在没有 `---@param` 标注可选性的代码库上误报多；注解完善的代码库可提到 `warning` |
| `mylua.diagnostics.argumentTypeMismatch` | `warning` | 实参字面量类型与形参 `---@param` 类型不符。实参类型未知时静默跳过 |
| `mylua.diagnostics.returnMismatch` | `hint` | 函数 `---@return` 注解与实际 `return` 语句不符（遍历嵌套 return，遇内层函数边界停止）。默认 `hint`：早退裸 `return`、各分支返回不同个数都是正常 Lua 写法，静态比较无法与真实错误区分 |

### 诊断行为

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.diagnostics.narrowByConditionGuard` | bool | `true` | 代码已检查名字存在时（`if X then … end`、`if X == nil then … else … end`、`X and X.f()`、`while X do … end` 等），抑制 `undefinedGlobal` / `unknownField` 诊断。适合宿主程序运行时注入全局符号的工程。不改变类型推断：要获得真实类型信息仍需写 `---@class` / `---@meta` stub |

## 内嵌提示（Inlay Hints）

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.inlayHint.enable` | bool | `true` | 内嵌提示总开关 |
| `mylua.inlayHint.parameterNames` | bool | `true` | 调用实参前显示形参名 |
| `mylua.inlayHint.variableTypes` | bool | `true` | 局部变量名后显示推断类型 |

## 工作区

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.workspace.include` | string[] | `["**/*.lua"]` | 参与工作区索引的文件 glob |
| `mylua.workspace.exclude` | string[] | `["**/.*", "**/node_modules"]` | 从索引中排除的文件/目录 glob。目录模式（如 `**/foo`）自动排除其下所有文件（等价于追加 `**/foo/**`） |
| `mylua.workspace.library` | string[] | `[]` | 额外索引的注解库目录，通常放 Lua stdlib stubs 或第三方 EmmyLua 注解包。支持绝对路径、`~/…`（按 `$HOME` 展开）、相对首个工作区根目录的路径。这些文件以只读方式参与索引：提供全局符号/类型（hover、跳转、补全、require 解析），自身不产生诊断 |
| `mylua.workspace.useBundledStdlib` | bool | `true` | 自动加载扩展内置的 Lua 标准库注解（按 `mylua.runtime.version` 选择版本），使 `print`、`string.format`、`table.concat` 等开箱即有 hover / 跳转 / 签名帮助。仅当你打算通过 `mylua.workspace.library` 提供完整 stdlib 注解时才需要关闭 |
| `mylua.workspace.priorityKeyword` | string[] | `["annotation"]` | 提升定义候选优先级的路径片段（大小写不敏感）。多个文件定义同名符号时，路径含这些片段的文件胜出，适合标记注解/stub 目录。修改后需重启语言服务生效 |

## 运行时

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.runtime.version` | string | `"5.4"` | 假定的 Lua 版本（`5.3` / `5.4`）。影响 `undefinedGlobal` 抑制所用的内置标识符集，以及 `mylua.workspace.useBundledStdlib` 加载的内置 stdlib stubs 版本（当前仅内置 5.4，请求 5.3 时回退到最新可用版本） |
| `mylua.runtime.topKeyword` | bool | `false` | 解析器顶层关键字切分。开启后，列 0 的关键字（`if`、`while`、`for`、`function` 等）产生特殊的 `TOP_WORD_*` token 强制闭合当前块——错误定位更前置，但要求嵌套代码必须缩进。单文件可用 `---#enable top_keyword` / `---#disable top_keyword` 指令覆盖 |

## 模块解析

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.require.aliases` | object | `{}` | require 路径别名。别名按模块路径的点分前缀匹配，最长前缀优先。例如 `{ "@": "src", "@utils": "src.utils" }` 使 `require('@utils.foo')` 解析为 `src.utils.foo` |

## 表结构分析

以下两项在服务启动时固定，修改后需重启语言服务（策略在建索引时写入每个文件的摘要）。

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.tableShape.stringKeys` | bool | `true` | 静态字符串键（`{ ["foo"] = 1 }`、`t["foo"] = 1`）是否登记为表字段，使 `t.foo` 可跳转/hover/补全、不报未知字段。关闭后强制 `.Name` 风格：字符串键不贡献字段，表被视作字段集合不完整，不产生伪未知字段错误 |
| `mylua.tableShape.stringKeysRequireIdentifier` | bool | `true` | 在上一项开启时，要求键文本为合法 Lua 标识符（`[A-Za-z_][A-Za-z0-9_]*`）才登记，字段集合恰好等于「`t.x` 能触达的东西」。关闭后 `t["a-b"]`、`t["1"]`、非 ASCII 键也会登记，只能经方括号读取访问 |

## 导航

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.gotoDefinition.strategy` | string | `"auto"` | 多候选定义的处理：`auto` 唯一最佳则直接跳转、否则列出候选；`single` 总是跳最佳；`list` 总是列候选 |
| `mylua.references.strategy` | string | `"best"` | 多候选引用的处理：`best` 使用最高优先候选；`merge` 合并全部候选；`select` 弹出选择 |
| `mylua.references.scanComments` | bool | `true` | 查找 Emmy 类型名引用时是否同时匹配普通（非 `---@`）注释。关闭后只匹配 `---@` 行内的类型名，减少散文注释的误报 |

## 大纲（Outline）

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.documentSymbol.detailLevel` | string | `"compact"` | Outline 视图详细程度：`compact` 文件级声明 + 类字段/方法；`functions` 追加嵌套具名函数；`allDeclarations` 追加函数体内的 local 声明（遮蔽的 local 保留为独立符号）；`anonymousFunctions` 追加匿名函数（含其体内的声明） |

## 性能

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.performance.slowParseKeepTreeThresholdMs` | int | `500` | 冷启动扫描期间的语法树保留阈值（毫秒）。扫描时解析慢于该值的文件保留语法树、其余只保留索引摘要——因此它**只影响初始扫描的内存占用**：一旦使用查找引用等需要语法树的功能，相关文件会重新解析并常驻内存（**首次查找引用会比较慢**），不再受此阈值控制。内存占用可在状态栏 MyLua tooltip 中查看，上限可按源码总量估算：索引信息约 10 倍 + 语法树约 20 倍 ≈ 30 倍；默认设置下语法树几乎不缓存，初始内存消耗较低。文件数量较少的项目可把该值设为 0（低于 15 即可），扫描时缓存全部语法树，免去首次使用时的重新解析 |

## 服务器

| 配置 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `mylua.server.path` | string \| object | `""` | 自定义 `mylua-lsp` 可执行文件路径。留空时自动定位：正式构建加载 `<扩展目录>/server/<bin>`；F5 调试（Extension Development Host）绕过 `server/`、直接加载 `<扩展目录>/../lsp/target/debug/<bin>`，使 `cargo build` 的新改动无需重新打包即可生效 |
| `mylua.server.autoRestartOnConfigChange` | bool | `true` | MyLua 配置变更后自动重启语言服务器。关闭时改为弹出重启提示 |
| `mylua.debug.fileLog` | bool | `false` | 输出调试日志到工作区 `.vscode/mylua-lsp.log`，可用于查看被跳过索引的文件列表 |

`mylua.server.path` 支持两种写法：

```jsonc
{
  // 字符串：所有平台共用
  "mylua.server.path": "/usr/local/bin/mylua-lsp",

  // 对象：按平台（Node.js process.platform）分别指定
  "mylua.server.path": {
    "win32": "C:/tools/mylua-lsp.exe",
    "darwin": "/usr/local/bin/mylua-lsp",
    "linux": "/usr/local/bin/mylua-lsp"
  }
}
```
