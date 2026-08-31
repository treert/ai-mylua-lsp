# LSP 能力

已实现的所有 LSP 能力。语义约定见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md)，索引架构见 [`index-architecture.md`](index-architecture.md)。

## 基础能力

| 能力 | 说明 |
|------|------|
| 文档同步 | Incremental sync + tree-sitter 增量 reparse |
| 位置编码 | 优先协商 UTF-8，客户端不支持时回退 UTF-16；中文/emoji 正确对齐 |
| 配置体系 | `initializationOptions` + `didChangeConfiguration` 下发；VS Code 扩展在配置变更后提示重启 LSP |
| `runtime.version` | 支持 `5.3`/`5.4`，影响内置标识符集合和诊断 |
| `workspace.library` | 外部库路径解析为额外 scan root，库文件强制 `is_meta = true`，不产生诊断 |
| `workspace.priorityKeyword` | 路径片段（大小写不敏感）列表，默认 `["annotation"]`；当多个文件定义同名符号时，路径含这些片段的文件优先级更高。修改后需重启 LSP 才能对已索引文件生效 |

| 内置 stdlib | 扩展侧自动注入 `<extensionPath>/assets/lua<version>/` 的 stub 文件 |

## 导航

### goto definition
- local 作用域 → 全局符号表 → `require` 跳转（优先 `return` 语句位置）
- `local x = require("mod")` 的 LHS 名称直接跳转到模块（含 `<const>`/`<close>` 属性偏移处理）
- AST 驱动的任意深度 dotted field 跳转（`a.b.c`）
- Emmy 类型名跳转（`type_shard`）
- `_ENV` 重定向后的自由名跳转（解析为环境表的字段，见 [`lsp-semantic-spec.md` §1.3](lsp-semantic-spec.md)）
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
- `_ENV` 重定向后的自由名按环境表字段查找，区分环境边界（重绑前后的同名符号不合并）
- 声明包含策略可配置（`references.strategy`: Best/Merge/Select）
- `references.scanComments`（默认 `true`）：是否在普通注释（非 `---@`）里扫描已注册 Emmy 类型名。关闭后仅匹配 `---@` 注解行内的类型名，减少散文注释的误报。rename 跟随同一开关

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
- `_ENV` 重定向后的自由名解析为环境表字段；环境形状非穷尽时（`setmetatable` / 类型未知）按 `{__index=_G}` 约定回退到全局命名空间

### signatureHelp
- 基于 `FunctionSummary` 的参数签名浮窗
- 支持 `@overload` 多签名、匿名函数绑定、跨文件 require 返回的 callable
- 跨文件 class 声明/实现分离时合并 overloads
- 方法调用时 `self` 不出现在参数列表
- `active_parameter` 感知嵌套括号与字符串

### inlayHint
两类虚拟标签：`mylua.inlayHint.enable` 控制总开关，`parameterNames` 与 `variableTypes` 分别控制参数名提示和变量类型提示；默认值以 `vscode-extension/package.json` 为准。


| 类型 | 行为 | 跳过条件 |

|------|------|----------|
| `parameterNames` | 实参前加 `a:` 标签 | 实参名与形参名相同、变参、method 的 self |
| `variableTypes` | 变量后加 `: type` 标签 | 已有 `@type` 注解、Unknown/Table/Function/Nil 类型 |

## 符号与大纲

### documentSymbol
层级化 outline，默认使用 `mylua.documentSymbol.detailLevel = "compact"`：
- `@class`/`@enum`/`@alias` → 顶层节点，`@field` / `function Class:m()` → 子节点
- `function`/`local function` → Function，`local x` / 全局赋值 → Variable
- 点号/下标 LHS（`t.foo = 1`）静默跳过避免噪声
- `selection_range` 精确指向标识符本身（非整行）

可配置更细粒度：
- `"functions"`：在 compact 基础上递归展示函数体内的具名函数，并挂到外层函数子节点下。
- `"allDeclarations"`：在 functions 基础上展示函数体内的参数、普通 local、for 变量；Lua shadowing 产生的同名 local 会保留为多个独立 symbol。
- `"anonymousFunctions"`：在 allDeclarations 基础上把匿名函数也展示为 Function 节点；`local cb = function() ... end` / `cb = function() ... end` 使用绑定名，无法推导绑定名时显示为 `<anonymous>`。

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
- 裸名候选按当前 `_ENV` 分层：无元表沙箱只给该环境表的字段（不给运行时为 nil 的全局名），带元表 / 类型未知的沙箱按 `{__index=_G}` 约定给「环境字段 + 全局名」，无重定向时给全局名。判据与导航/诊断同源，见 [`lsp-semantic-spec.md` §1.3](lsp-semantic-spec.md)
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

`mylua.diagnostics.enable=false` 会关闭语义诊断；语法诊断继续保留。

| 诊断 | 配置键 | 默认 |

|------|--------|------|
| 未定义全局变量 | `undefinedGlobal` | Warning |
| 重定向 `_ENV` 的未定义字段 | `envUnknownField` | Warning |
| Emmy 类型未知字段 | `emmyUnknownField` | Warning |
| Table shape 未知字段 | `luaFieldError`/`luaFieldWarning` | Warning |

| 类型不匹配 | `emmyTypeMismatch` | Warning |
| 重复 table key | `duplicateTableKey` | Warning |
| 未使用 local | `unusedLocal` | Hint |
| 参数个数不匹配 | `argumentCountMismatch` | Warning |
| 参数类型不匹配 | `argumentTypeMismatch` | Warning |
| return 不匹配 | `returnMismatch` | Warning |
| `@param` 名称不匹配 Lua 参数 | 内置 | Warning |

`@param` 名称不匹配诊断随 `diagnostics.enable` 开关启停，也可用 `---@diagnostic disable: param-annotation` 抑制。

`envUnknownField` 与 `undefinedGlobal` **按环境形态分工**（不是"只要重定向就交接"）：`_ENV` 指向形状**穷尽**（无元表、无 `rawset`）的表时由 `envUnknownField` 判断字段是否存在（抑制码 `env-field`），要求读写均位于 chunk 顶层直线执行流；形状**非穷尽**时按 `{__index=_G}` 约定处理——环境表没有的名字回退查全局索引，两处皆无则由 `undefinedGlobal` 报出。`__index` 的实际指向刻意不追踪。因此内置库名（含 `_G`）在穷尽环境下**不豁免**。详见 [`lsp-semantic-spec.md` §1.3.1](lsp-semantic-spec.md)。

### `---@meta [name]`
文件标记为 stub，跳过 `undefinedGlobal` 诊断，声明的 global 正常参与索引。

### `---@diagnostic` 抑制
支持 `disable-next-line` / `disable-line` / `disable` ... `enable`，逗号分隔 code 列表或通配符 `*`。

## EmmyLua 注解

递归下降解析器，完整支持类型表达式语法（union `|`、optional `?`、array `[]`、generic `<T>`、`fun()` 函数类型、`{k:v}` table 类型）。

支持的标签：`@class` / `@field` / `@param` / `@return` / `@type` / `@alias` / `@enum` / `@generic` / `@overload` / `@vararg` / `@deprecated` / `@async` / `@nodiscard` / `@customrequire` 等。

关键特性：
- 泛型参数替换（`EmmyGeneric`）
- `@alias` 指向 inline table literal 时字段平铺进 `TypeDefinition.fields`
- `self` 泛型绑定：`---@return self` 在方法定义上自动替换为所属 class 名
- `fun(): A, B` 多返回值

### `@customrequire` — 自定义 require 函数

标记函数为类 `require` 的封装，使其返回值解析为目标 module 的返回类型。

**语法：**

```
---@customrequire param=<name> [regex-pattern] [template]
```

- `param=<name>`：指定哪个参数是 module 路径参数（必填）
- `regex-pattern`：Rust regex 语法的变换规则（可选）
- `template`：替换模板，`$1`/`$2` 为捕获组占位符，其余字符字面量（可选）

各部分以空格分隔；无 pattern/template 时直接用原参数值作为 require 路径。

**示例：**

```lua
--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    return require(module_path)
end

local a = custom_require("mgr_abc.abc_mgr")
-- a 解析为 module_abc.abc_mgr 的返回类型
```

**限制：**
- 仅当调用实参是字符串字面量时生效（变量/表达式降级为 Unknown）
- 不支持多条变换规则链式
- 注解失效时静默降级，不生成诊断

## 自定义通知

### `mylua/indexStatus`（server → client）

```typescript
{ state: "indexing" | "diagnosing" | "ready", indexed: number, total: number,
  elapsedMs?: number, phase?: string, message?: string, remaining?: number }
```

`phase` 值：`scanning` / `module_map_ready` / `parsing` / `merging` / `diagnosing`。
`elapsedMs` 仅在冷启动索引完成的 `ready` 时出现；`remaining` 用于后台诊断进度，采样到从非零变为 0 时 server 会发送一次 `ready`。

### `mylua/memoryStatus`（server → client）

```typescript
{ memoryBytes: number }
```

server 进程常驻内存（working set / RSS）字节数，供扩展在状态栏 tooltip 显示。ready 之后 server 每 ~2 秒采样一次，仅当相比上次推送变化 ≥ 1 MiB 时才发送，因此内存平稳时可能长时间不推送。采样失败或平台不支持（非 Windows/Linux/macOS）时不发送。
