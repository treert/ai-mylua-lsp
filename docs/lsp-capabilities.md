# LSP 能力

已实现的所有 LSP 能力。语义约定见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md)，索引架构见 [`index-architecture.md`](index-architecture.md)。

## 基础能力

| 能力 | 说明 |
|------|------|
| 文档同步 | Incremental sync + tree-sitter 增量 reparse |
| 位置编码 | UTF-16 code unit，中文/emoji 正确对齐 |
| 配置体系 | 20 项配置，`initializationOptions` + `didChangeConfiguration` 下发 |
| `runtime.version` | 支持 `5.1`/`5.2`/`5.3`/`5.4`/`luajit`，影响内置标识符集合和诊断 |
| `workspace.library` | 外部库路径解析为额外 scan root，库文件强制 `is_meta = true`，不产生诊断 |
| 内置 stdlib | 扩展侧自动注入 `<extensionPath>/assets/lua<version>/` 的 stub 文件 |

## 导航

### goto definition
- local 作用域 → 全局符号表 → `require` 跳转（优先 `return` 语句位置）
- `local x = require("mod")` 的 LHS 名称直接跳转到模块（含 `<const>`/`<close>` 属性偏移处理）
- AST 驱动的任意深度 dotted field 跳转（`a.b.c`）
- Emmy 类型名跳转（`type_shard`）
- 多候选策略可配置（`gotoDefinition.strategy`: Auto/Single/List）

### goto declaration
alias 到 `goto_definition`（Lua 中 declaration ≡ definition）。

### goto typeDefinition
- 点击变量跳到其 `@class` 声明位置
- 点击注解内类型名同样跳到声明
- 无 Emmy 类型时回退到 `goto_definition`

### references
- 单文件 local scope（shadowing 感知）+ 全工作区全局符号引用
- EmmyLua 注解内的类型名引用（`@type`/`@param`/`@return`/`@class : Parent` 等）
- 点击注解内类型名也能触发
- 声明包含策略可配置（`references.strategy`: Best/Merge/Select）

### rename
- 单文件 local + 全工作区全局（含 prepareRename）
- 新名字校验为合法 Lua 标识符，关键字拒绝
- Emmy 类型名 rename 同步替换所有跨文件注解引用

### callHierarchy
- `prepareCallHierarchy` + `incomingCalls` + `outgoingCalls`
- 数据来源：`DocumentSummary.call_sites`（callee_name, caller_name, range 三元组）
- 嵌套函数作用域边界正确处理（内层匿名函数的 call 不归到外层 caller）

### documentLink
`require("mod")` 字符串内容作为可点击链接，target 为解析到的目标文件 URI。

## 信息展示

### hover
- 定义源码 + EmmyLua 注解 + 文档注释
- AST 驱动的推断类型展示（dotted field chain、function call return、subscript）
- Emmy 类型 hover（class/alias/enum 区分展示、字段列表）
- `@overload` 签名展示
- 匿名函数绑定签名展示（`local f = function(a, b) end`）
- `function a.b.c()` 中间段 identifier 不误报为函数 hover

### signatureHelp
- 基于 `FunctionSummary` 的参数签名浮窗
- 支持 `@overload` 多签名、匿名函数绑定、跨文件 require 返回的 callable
- 跨文件 class 声明/实现分离时合并 overloads
- 方法调用时 `self` 不出现在参数列表
- `active_parameter` 感知嵌套括号与字符串

### inlayHint
两类虚拟标签（均 opt-in，`inlayHint.enable` 默认 off）：

| 类型 | 行为 | 跳过条件 |
|------|------|----------|
| `parameterNames` | 实参前加 `a:` 标签 | 实参名与形参名相同、变参、method 的 self |
| `variableTypes` | 变量后加 `: type` 标签 | 已有 `@type` 注解、Unknown/Table/Function/Nil 类型 |

## 符号与大纲

### documentSymbol
层级化 outline：
- `@class`/`@enum`/`@alias` → 顶层节点，`@field` / `function Class:m()` → 子节点
- `function`/`local function` → Function，`local x` / 全局赋值 → Variable
- 点号/下标 LHS（`t.foo = 1`）静默跳过避免噪声
- `selection_range` 精确指向标识符本身（非整行）

### workspace/symbol
- 全局函数/变量 + Emmy class/alias/enum 模糊搜索
- Class 成员以 METHOD/FUNCTION/FIELD 形式展示，带 `container_name`

## 语法着色

### semantic tokens
- 全局变量 `defaultLibrary` + 局部变量标记（作用域感知）
- **设计取舍**：刻意最小化，只补 TextMate 无法静态判定的语义区分
- 支持 `full` / `range`（视口过滤）/ `full/delta`（最长公共前缀/后缀算法）

## 编辑器辅助

### completion
- 局部变量 + 全局名 + 关键字
- AST 驱动的点号字段补全 + 冒号补全过滤方法
- `---@` EmmyLua tag 补全（class/field/param/return/type 等 24 种）
- `require("…")` 模块路径补全
- `trigger_characters`: `.` `:` `@` `"` `'`
- `completionItem/resolve` 延迟加载 documentation/detail

### selectionRange
从最深 named descendant 沿 parent 链向上收集，去掉等价项后串成链表。

### foldingRange
- 函数、do/while/for/repeat、if/elseif/else（每个分支独立 fold）、多行 table
- 多行块注释 + 连续 `---@tag` 注释行合并折叠
- `end_line` 保留闭合关键字可见

### documentHighlight
同文件 identifier 同义高亮，按 AST 祖先区分 Read/Write，作用域感知 shadowing。

## 诊断

### 语法诊断
Tree-sitter ERROR/MISSING 节点自动转为诊断。

### 语义诊断

| 诊断 | 配置键 | 默认 |
|------|--------|------|
| 未定义全局变量 | `undefinedGlobal` | Warning |
| Emmy 类型未知字段 | `unknownField` | Warning |
| Table shape 未知字段 | `luaFieldError`/`luaFieldWarning` | 可配置 |
| 类型不匹配 | `emmyTypeMismatch` | 可配置 |
| 重复 table key | `duplicateTableKey` | Warning |
| 未使用 local | `unusedLocal` | Off |
| 参数个数不匹配 | `argumentCountMismatch` | Off |
| 参数类型不匹配 | `argumentTypeMismatch` | Off |
| return 不匹配 | `returnMismatch` | Off |

### `---@meta [name]`
文件标记为 stub，跳过 `undefinedGlobal` 诊断，声明的 global 正常参与索引。

### `---@diagnostic` 抑制
支持 `disable-next-line` / `disable-line` / `disable` ... `enable`，逗号分隔 code 列表或通配符 `*`。

## EmmyLua 注解

递归下降解析器，完整支持类型表达式语法（union `|`、optional `?`、array `[]`、generic `<T>`、`fun()` 函数类型、`{k:v}` table 类型）。

支持的标签：`@class` / `@field` / `@param` / `@return` / `@type` / `@alias` / `@enum` / `@generic` / `@overload` / `@vararg` / `@deprecated` / `@async` / `@nodiscard` 等。

关键特性：
- 泛型参数替换（`EmmyGeneric`）
- `@alias` 指向 inline table literal 时字段平铺进 `TypeDefinition.fields`
- `self` 泛型绑定：`---@return self` 在方法定义上自动替换为所属 class 名
- `fun(): A, B` 多返回值

## 自定义通知

### `mylua/indexStatus`（server → client）

```typescript
{ state: "indexing" | "ready", indexed: number, total: number,
  elapsedMs?: number, phase?: string, message?: string }
```

`phase` 值：`scanning` / `module_map_ready` / `parsing` / `merging`。`elapsedMs` 仅在 `ready` 时出现。
