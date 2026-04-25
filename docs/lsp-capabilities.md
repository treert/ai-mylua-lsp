# LSP 能力详细实现

**本文档记录 `mylua-lsp` 已实现的所有 LSP 能力的内部实现细节。**
概览级别的能力列表见 [`ai-readme.md`](../ai-readme.md)，语义约定与消费规则见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md)。

**技术栈**：Rust + `tower-lsp-server` 0.23 + `tree-sitter` 0.26 + `tokio`。

| 路径 | 说明 |
|------|------|
| [`lsp/Cargo.toml`](../lsp/Cargo.toml) | Cargo workspace root |
| [`lsp/crates/tree-sitter-mylua/`](../lsp/crates/tree-sitter-mylua/) | 包装 crate：`build.rs` 编译 `grammar/src/` 的 C parser，导出 `LANGUAGE` |
| [`lsp/crates/mylua-lsp/`](../lsp/crates/mylua-lsp/) | LSP server（lib + bin 架构） |

## 基础能力

### initialize / shutdown / 文档同步
**Incremental sync** + tree-sitter 增量 reparse：`tree.edit(&InputEdit)` → `parse(new, Some(old))`，只改动区域的子树被重建。

### 位置编码
LSP Position 按 **UTF-16 code unit** 语义处理；`util::byte_col_to_utf16_col` / `utf16_col_to_byte_col` 在 tree-sitter 字节列与 LSP UTF-16 列之间转换，中文/emoji 等非 ASCII 行上 hover/goto/semantic token 也能对齐。

### 配置体系
20 个扩展配置项，通过 `initializationOptions` + `didChangeConfiguration` 下发；`require.aliases` 参与模块解析；**`runtime.version` 真正生效**：`lua_builtins::builtins_for(version)` 统一输出内置标识符集合，`5.1`/`5.2`/`5.3`/`5.4`/`luajit` 各有细分（5.3+ 加 `utf8`、5.2 独有 `bit32`、5.1/5.2/luajit 保留 `unpack`、luajit 额外 `bit`/`jit`/`ffi`）；`collect_semantic_diagnostics` 与 `collect_semantic_tokens` 均有 `_with_version` 变体，lib.rs handler 从 config 读 runtime.version 透传。

### 外部库索引（`workspace.library`）
`workspace_scanner::resolve_library_roots(library, workspace_roots)` 把用户配置的 `string[]` 解析为绝对 canonical paths（支持 `~/`、相对首个 workspace root、去重、不存在路径静默剔除）；解析结果作为额外 scan root 并入 `scan_workspace_lua_files` + `collect_lua_files`，让库里的 `string.lua` / `table.lua` 等参与 `global_shard`、`type_shard`、`require_map` 与普通 workspace 文件等价。所有库 URI 汇入 `Backend.library_uris: Arc<Mutex<HashSet<Uri>>>`，并在 `run_workspace_scan` 的 ParsedFile 阶段把对应 Summary 强制 `is_meta = true`（fresh parse 与 cached 两条路径都覆盖，`parse_and_store_with_old_tree` 在用户编辑库文件时也重新 enforce）；`consumer_loop` pop 到 library URI 直接 publish 空 Diagnostics 列表，保证 Problems 面板不会被 stub 文件污染。VS Code 扩展侧 `mylua.workspace.useBundledStdlib`（默认 `true`）会把打包在 `<extensionPath>/assets/lua<version>/` 的 stdlib stubs 自动注入到 `workspace.library` 列表最前端，用户的自定义库 append 在后。

## 导航能力

### goto definition
local 作用域 + 全局符号表 + `require` 跳转（优先跳到目标文件的 `return` 语句位置，回退到首个全局贡献）+ **点击 `local x = require("mod")` 的 LHS 名称直接跳转到模块 return**（正确处理 `attribute_name_list` 中 `<const>` / `<close>` 属性的索引偏移，`local x <const>, y = require(...)` 的 `y` 也能跳到模块）+ **AST 驱动的任意深度 dotted field 跳转**（`a.b.c` / `a.b.c.d` 通过嵌套 `variable` 节点的 `object`/`field` 字段递归解析；写入侧严格 bail：`foo().c = 1` / `a[1].c = 1` / 中间字段非 Table 时的 `a.b = 1; a.b.c = 2` 等链不污染 `global_shard` 也不乱设 shape；读取侧 `infer_node_type` 覆盖 `function_call` / subscript / 纯点号三类 base，`make().field` 这类 CallReturn 链可追踪 `@return` 声明类型）+ **Emmy 类型名跳转**（`type_shard`）；**多候选策略可配置**（`gotoDefinition.strategy`: Auto/Single/List）。

### goto declaration
Lua 里 declaration ≡ definition，alias 到 `goto_definition`，让偏好 `textDocument/declaration` 的客户端也能得到跳转结果。

### goto typeDefinition
点击 `---@type Foo local f = nil` 的 `f` 跳到 `@class Foo` 的声明位置；点击注解内的类型名（`---@type Foo` 里的 `Foo`）走 `extract_word_at` fallback 同样跳到声明；支持 `EmmyType` / `EmmyGeneric` / `TypeRef`；stub 链（`GlobalRef` / `RequireRef` 等）通过 `resolve_type` 追踪到 Emmy 名；无 Emmy 类型时回退到 `goto_definition`（保证不会"什么都找不到"）。

### references
单文件 local scope（用 `scope_tree.resolve_decl` 比对 `decl_byte`，正确区分 `local x = x + 1` 的 RHS 指向外层；shadowing 下不把被遮蔽的内部使用算作对外层的引用）+ 全工作区全局符号引用（`global_shard` + `type_shard` 声明 + 去重）+ **EmmyLua 类型名的注解内引用**（扫描所有索引文件的 `emmy_comment` / `comment` 节点文本，按 ASCII 词边界匹配，能收集 `---@type Foo` / `---@param x Foo` / `---@return Foo` / `---@class Bar : Foo` 等所有用法）；**点击注解内的类型名也能触发**（`find_node_at_position` 找不到 identifier 时回落到按源码字节范围提取 ASCII 词）；**声明包含策略可配置**（`references.strategy`: Best/Merge/Select）。

### rename
单文件 local + 全工作区全局（含 prepareRename）；**新名字校验为合法 Lua 标识符**（非法返回 `InvalidParams` JSON-RPC 错误，关键字拒绝）；**Emmy 类型名 rename**：通过 `references::find_references` 的 Emmy 注解扫描链路，rename `Foo` → `Gadget` 时同步替换所有 `---@class`、`---@type Foo`、`---@param x Foo`、`---@return Foo`、`---@class Bar : Foo` 等跨文件注解引用。

### callHierarchy
`prepareCallHierarchy` + `incomingCalls` + `outgoingCalls`：数据来源是 `DocumentSummary.call_sites`（`(callee_name, caller_name, range)` 三元组），在 `build_summary` 中通过专门的 `collect_calls_in_scope` 遍历填充，遇到 `function_declaration` / `local_function_declaration` / `function_definition` 更新 `caller_name` 作为嵌套函数的作用域边界——内层匿名函数里的 call 不会被归到外层 caller 上。prepareCallHierarchy 支持光标落在声明名（直接构建 item）和调用点（通过 `function_summaries` → `global_shard` 链式解析到目标函数）两种形态；incomingCalls 用 `last_segment` 做名字匹配（`m.sub.foo` → `foo`，`obj:bar` → `bar`）扫所有文件的 call_sites；outgoingCalls 按 `caller_name` 过滤本文件的 call_sites 后解析每个 callee 到其声明位置。`CallSite` 存 `#[serde(default)]` 兼容旧缓存。

### documentLink
`require("mod")` / `require "mod"` 字符串内容作为可点击链接，`target` 为 `resolve_module_to_uri` 解析到的目标 URI；范围是引号内文本，未解析的模块不产生链接；`m = require; m("foo")` 的别名调用不跟随；`resolve_provider: false`（target 在 emit 时就绪）。

## 信息展示能力

### hover
定义源码 + EmmyLua 注解 + 文档注释 + **`function_name` tail-only 短路**（hover 在 `function a.b.c()` / `function obj:m()` 的 base/中间段 identifier 上不再错误地把整个函数签名当作 base 的 hover；只有 tail —— method 名 / dot 链末端 / bare 形式的唯一名字 —— 才返回函数声明 hover，其他位置落到普通 scope/global/type 解析，让未定义的 base 表现为"无 hover"而非"看起来是个函数"）+ **AST 驱动的推断类型展示**（`infer_node_type` 递归 `variable` 节点 + 新增 `function_call` / subscript 分支 → URI-aware `resolve_field_chain_in_file`；per-file `TableShapeId` 通过 `caller_uri` 正确寻址到原文件的 shape；`make().field` 构造 `CallReturn` stub 让 resolver 追踪 `@return` 声明类型；`a[1].x` 走 shape 的 `array_element_type`）+ **Emmy 类型 hover**（class/alias/enum 区分展示、字段列表）+ **全局多候选提示** + **`@overload` 签名展示** + **匿名函数 local/global 绑定签名展示**（`local f = function(a, b) end` / 带 `@param`/`@return` 时 hover 显示完整 `fun(a: number, b: string): boolean`）。

### signatureHelp
基于 `FunctionSummary` 的参数签名浮窗；支持 `foo(a, b)` / `obj.m(...)` / `obj:m(...)` / 跨文件 require 返回的 callable；`---@overload` 生成多个 SignatureInformation；**匿名函数绑定识别**（`local f = function(a, b) end` / `f = function(x, y) end` 走 `infer_node_type + resolve_type` 拉出 Function sig，AST 参数 + 前置 `@param`/`@return` Emmy 注解合并）；**跨文件 class 声明/实现分离时合并 overloads**（`@class Foo` + `@field init fun(...)` 在 a.lua，而 `function Foo:init() end ---@overload ...` 在 b.lua 时，`@field` sig 与 b.lua 的 overloads 一起回传；visually-empty 的 self-only impl primary 自动过滤掉）；`active_parameter` 由 `(` 到光标间顶层 `,` 的计数得到（感知嵌套 `()` / `{}` / `[]` 与字符串、行注释）；方法调用时 `self` 不出现在显示参数列表。

### inlayHint
两类虚拟标签，均 opt-in（`inlayHint.enable` 默认 off），细分 `parameterNames` / `variableTypes`。参数名：`foo(1, 2)` 当 `foo` 的 FunctionSummary 已知时在每个实参前加 `a:` / `b:` 标签，method 调用隐藏 `self`，实参名与形参名相同时不 emit，变参参数跳过；变量类型：`local n = 42` 在 `n` 后加 `: integer` 标签，已有 `---@type` 注解时不重复，`Unknown` / `Table` / `Function` / `Nil` 类型跳过以减少噪声；支持 range 过滤（客户端通常按视口请求）。

## 符号与大纲

### documentSymbol
层级化 outline：`DocumentSummary.type_definitions` 驱动 Class/Enum/Alias 顶层节点，`@field` 进入 Field 子节点，`function Class:m()` / `function Class.m()` 进入 Method/Function 子节点；`function`/`local function` 顶层声明 → Function；`local x = ...` / 非点号 LHS 的全局赋值 → Variable；点号/下标 LHS（`t.foo = 1` / `m[1] = v`）静默跳过避免噪声；class 名同名 local/global 的 anchor 声明被折叠进 class 节点不重复展示；**`selection_range` 精细化**：`@class Foo` / `@alias Foo` / `@enum Foo` / `@field bar T` 的 outline 条目 `selection_range` 指向 `Foo` / `bar` 标识符 byte range 本身（UTF-16 编码），而非粗粒度的整行或 anchor 语句——点击 outline 精准跳到类型/字段名；workspace/symbol 的 `location.range` 同源。

### workspace/symbol
全局函数/变量 + **Emmy class/alias/enum**（`type_shard`）模糊搜索；**Class 成员搜索**：`function Foo:m` / `function Foo.m` 以 METHOD/FUNCTION 形式展示 name=m + container_name=Foo（不再作为 `Foo:m` 融合名出现）；所有 `@field` 生成 FIELD 条目带 container_name，搜索 `ba` 能同时匹配两个 class 各自的 `bar` 字段；alias 的 kind 改为 INTERFACE 更符合语义。

## 语法着色

### semantic tokens
全局变量 `defaultLibrary` + 局部变量标记（作用域感知）；**发出的列号为 UTF-16 code unit**。**设计取舍：刻意最小化，只补 TextMate 无法静态判定的语义区分**（全局 `defaultLibrary`、全局/局部、Emmy 类型名等）；`keyword` / `number` / `string` / 注释等基色交由 TextMate，**不做** token type 细分（详见 [`requirements.md`](requirements.md) §3.1）。**range provider**：`textDocument/semanticTokens/range` 支持客户端只请求视口内 token，delta 编码从 (0,0) 重新起算（与 full 独立）；按起始行过滤。**delta provider**：`textDocument/semanticTokens/full/delta` 通过最长公共前缀/后缀算法计算一条 `SemanticTokensEdit`（start 和 delete_count 按 LSP 约定以 u32 为单位 = token_count × 5），client previous_result_id 匹配时返回 edits、否则 fallback 为完整 Tokens；per-URI 的 `TokenCacheEntry` 在 `did_close` 时清理避免内存泄漏；monotonic 计数器确保 result_id 全会话唯一。

## 编辑器辅助

### completion
局部变量 + 全局名 + 关键字 + **AST 驱动的点号字段补全**（通过 `hover::infer_node_type` 递归 `variable` 节点的 `object`/`field`，不再用字符串 splitn）+ **冒号补全过滤方法**（结构化 `is_function` 判定）+ **`---@` EmmyLua tag 补全**（class/field/param/return/type/alias/enum/generic/overload/vararg/deprecated/async/nodiscard/see/meta/diagnostic/cast/operator/private/protected/package/public/readonly/version）+ **`require("…")` 模块路径补全**（来自 `require_map`）；**声明 `trigger_characters = ['.', ':', '@', '"', "'"]`** 让客户端自动触发；**`completionItem/resolve`**：initial payload 不带 `documentation`/`detail`，client 高亮 item 时才用 `data: {kind, uri, name}` 回调 resolve 拉取——global function 附 markdown 签名 + origin 文件、local 附 `local <name>: <type>`、keyword / emmy tag / require path 无 data 保持原样。

### selectionRange
VS Code "智能扩展选区"。从光标处最深 named descendant 开始，沿 parent 链向上收集所有 named 节点 range，去掉相邻等价项后串成 `SelectionRange { range, parent: ... }` 链表。跳过 unnamed token（如 `(`、`,`、关键字）避免单字符抖动。多 position 各自独立构链。

### foldingRange
Tree-sitter walk 驱动。Region 折叠覆盖 `function/local function/function() ... end`、`do`/`while`/`for`/`repeat`、`if/elseif/else` 以及多行 table constructor；`end_line = end_row - 1` 保留闭合关键字可见。**`if/elseif/else` 每个分支都有独立 fold**：整个 `if_statement` 一个外层 fold，另加一个 if-branch（从 `if` 到首个 `elseif`/`else` 前一行）、每个 `elseif_clause` / `else_clause` 一个独立 fold（用 `next_sibling.start_row - 1` 避免 tree-sitter 把 clause 停在最后一条语句、导致最后一行 body 不被折叠）。Comment 折叠覆盖多行 `--[[ ... ]]` / `--[=[ ... ]=]` 长块注释和连续的 `---@tag` 注释行（按行号合并相邻 `emmy_comment`）；`end_line = end_row` 整块折成一行。单行构造自动跳过。

### documentHighlight
同文件 identifier 同义高亮，按 AST 祖先分 Read/Write —— 局部/函数/for-var/参数声明处发 Write，`assignment_statement` LHS 发 Write，其他发 Read。作用域感知：光标落在 local 上时通过 `scope_tree.resolve_decl` 过滤掉被 shadow 的同名占用；`local x = x + 1` 的 RHS `x` 正确归属于外层；全局/Emmy 类型名无 scope decl 时退化为文本匹配。

## 诊断

### 语法诊断
Tree-sitter ERROR/MISSING 节点自动转为 `publishDiagnostics`。

### 语义诊断
- **未定义全局变量**（正确处理 `function a.b.c()` / `function a:m()` 形式：首个 identifier 是对已有表的**读取**而非定义，未定义时报告 `Undefined global 'a'`；`function foo()` 纯 bare 形式仍识别为定义不报；`a` / `b` / `c` / `m` 等中间/尾段字段写入不报）
- **Emmy 类型未知字段访问** warning
- **Lua table shape 未知字段**（closed→error / open→warning，`luaFieldError`/`luaFieldWarning` 可配置）
- **Emmy 类型不匹配**（`---@type` 声明与赋值字面量类型冲突时报告，`emmyTypeMismatch` 可配置，覆盖 `local x = ...` 初始赋值 + `x = ...` 后续赋值两种场景；shadowing 下用 `scope_tree.resolve_decl` 过滤）
- **重复 table key** `{ a = 1, a = 2 }` / `{ [1] = "x", [1] = "y" }` 命中 warning（`duplicateTableKey` 可配置，默认 Warning）
- **未使用 local** 跳过 `_`/`_prefix` 习惯写法（`unusedLocal` 可配置，默认 Off）
- **函数调用参数个数不匹配**（`argumentCountMismatch` 可配置，默认 Off；vararg `...` 吸收多余实参；任一 `---@overload` 的形参数匹配都清掉诊断；`obj:m()` 隐式 `self` 不计入）
- **函数调用参数类型不匹配**（`argumentTypeMismatch` 可配置，默认 Off；`Unknown` 字面量跳过）
- **`@return` 与实际 return 不匹配**（`returnMismatch` 可配置，默认 Off；walk 所有嵌套 `return`，遇到内层 `function_declaration` / `function_definition` 停住避免污染外层）
- **诊断 enable/severity 受配置控制**

### `---@meta [name]` 元文件支持
识别靠前的 `---@meta` 标签标记该文件为 stub/定义文件（遇到真实代码前的 emmy_comment 才算，避免把文件中段的 `@meta` 误识别）；`DocumentSummary.is_meta + meta_name` 持久化；该文件内 `undefinedGlobal` 诊断被跳过（stub 文件常引用运行时提供的符号，本就没有声明），但其他诊断保留；meta 文件声明的 global 正常进 `global_shard` 参与 workspace 索引，让引用这些符号的其他文件也不会误报。

### `---@diagnostic` 抑制
`disable-next-line` / `disable-line` / `disable` ... `enable` (file-scoped) 覆盖所有诊断，支持逗号分隔的 code 列表（`undefined-global` / `unused-local` / `unknown-field` / `type-mismatch` / `duplicate-table-key` / `argument-count` / `argument-type` / `return-mismatch` / `syntax`）和通配符 `*`；未知 tag 静默忽略；每条存活的 Diagnostic 都带 `code` 字段供客户端展示；`apply_diagnostic_suppressions` 作为 post-process 在 `publish_diagnostics` 前运行，对 syntax + semantic 混合列表都生效。

## EmmyLua 注解系统

递归下降解析器（`emmy.rs`），完整支持类型表达式语法（union `|`、optional `?`、array `[]`、generic `<T>`、`fun()` 函数类型、`{k:v}` table 类型、括号分组）；注解标签 `@class`/`@field`/`@param`/`@return`/`@type`/`@alias`/`@enum`/`@generic`/`@overload`/`@vararg`/`@deprecated`/`@async`/`@nodiscard` 等。

- **泛型参数替换**（`EmmyGeneric` 变体；字段解析和补全中自动将 `T` 替换为实际类型参数）
- **`@overload` 参与 FunctionSummary + hover 展示**
- **`@alias` 右侧类型保存和展开**（`---@alias Foo { x: number, y: number }` 这种指向 inline table literal 的 alias，会把 **named 字段** 平铺进 `TypeDefinition.fields`，让 `---@type Foo local p = ...` 的 `p.x` 在 hover / 诊断 / 补全 / 字段解析中都等价于一个同名 class；`IndexType` 形式如 `[string]: T` 不可命名访问故跳过）
- **`@enum` 写入 TypeShard**
- **`self` 泛型绑定**：`---@return self` / `---@param x self` 在 `function Foo:method()` / `function Foo.method()` 形式的方法定义上自动替换为 `Foo`，让 fluent-style 的 `obj:chain():chain2()` 返回类型能正确链式追踪。`class_prefix_of(name)` 从 `Foo:m`/`Foo.m`/`a.b.c` 截取类名，`substitute_self(fact, class_name)` 递归走 `Union` / `Function.params` / `Function.returns` / `EmmyGeneric.args` 统一替换；`substitute_self` 同时应用到主签名和所有 `@overload`；自由函数（无类名前缀）保留 `self` 字面
- **`fun(): A, B` 多返回值**：`parse_fun_type` 已通过 `parse_type_list` 支持冒号后多类型

## 作用域树

`scope.rs`：arena-based `ScopeTree`，单趟 AST 遍历构建；支持 `function_body` / `do` / `while` / `repeat` / `if` / `for` 等所有块级作用域 + 参数 + for 变量 + 隐式 `self`；正确处理 `local x = x + 1` RHS 引用外层变量的 Lua 语义。

## 索引架构

详见 [`index-architecture.md`](index-architecture.md) 和 [`index-implementation-plan.md`](index-implementation-plan.md)。

关键模块：
- `summary_builder.rs`：单文件 AST → DocumentSummary
- `aggregation.rs`：WorkspaceAggregation（GlobalShard / TypeShard / RequireByReturn / TypeDependants）
- `resolver.rs`：跨文件 stub 链式解析 + 缓存 + 环路保护
- `summary.rs`：`DocumentSummary` 数据结构定义
- `workspace_scanner.rs`：include/exclude glob 过滤，扫描和增量文件变更

## 自定义 LSP 通知

### `mylua/indexStatus`（server → client，单向）
payload `{ state: "indexing" | "ready", indexed: number, total: number, elapsedMs?: number, phase?: string, message?: string }`。由 `lib.rs::run_workspace_scan` 在扫描开始、module_index 就绪、每批解析进度、以及 `IndexState::Ready` 后发出；扩展侧驱动 StatusBarItem。`phase` 字段值：`scanning` / `module_map_ready` / `parsing` / `merging`。`elapsedMs` **仅**在终态 `ready` 时出现，表示从 `initialized` handler 进入到 `IndexState::Ready` 的 wall-clock 毫秒数。Rust 定义：`lib.rs::IndexStatusNotification` / `IndexStatusParams`。

## 构建与测试

- 构建：`cd lsp && cargo build`
- 测试：`cd lsp && cargo test --tests`
