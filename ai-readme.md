# AI 会话入口（必读）

**本文件面向 AI 助手与人类协作者：在新对话或接手本仓库时，请先阅读本文，再按需深入 `docs/` 目录。**

## 强制规则（给 AI）

1. **在回答与本项目相关的实现、排错、重构或规划前**，应阅读本文件（`ai-readme.md`）与 [`docs/README.md`](docs/README.md)，并按主题查阅 [`docs/`](docs/) 下对应文档。
2. **修改架构、图层、数据路径或依赖时**，同步更新 `docs/` 中相关文档（跨文件索引见 [`docs/lsp-semantic-spec.md`](docs/lsp-semantic-spec.md)），避免文档与代码脱节。

## 项目目标
实现 lua vscode 插件，支持语法高亮，语义跳转，hover tips, 诊断，outline 等功能。
**需要支持 emmylua 类型的类型注释。**
**仅支持 Lua 5.3 及以上版本。**
对性能有较高要求，需要支持5万个lua文件级别。

**方案取向（需求分析阶段）**

- **全工作区能力**：定义、**所有引用**、**工作区符号** 均为硬性目标，而非「仅打开文件」级能力。
- **解析与高亮**：**自研 Tree-sitter** 置于 **LSP** 内，负责 **语法树** 与增量解析；**基色高亮**以 **自研 TextMate** 为主；**LSP semantic tokens** 在 TextMate 之上叠加语义着色（如全局/局部等），与 Tree-sitter **不冲突、分工不同**。
- **分体工程**：**VS Code Extension** 与 **LSP Server** **分开实现、可分开发布**，可并行开发；LSP 可独立服务其他编辑器或工具。
- **仓库**：**Monorepo**（单仓）管理文法、LSP、扩展等，详见 [`docs/implementation-roadmap.md`](docs/implementation-roadmap.md) §2。

## 开发进度

### 需求分析
- 文档见 [`docs/README.md`](docs/README.md)（需求、架构、路线图与技术倾向）。

### Monorepo 骨架
- 已按计划创建顶层目录：`grammar/`、`lsp/`、`vscode-extension/`（各含 README）；根目录 [`README.md`](README.md) 说明布局。

### 测试与资源文件

| 路径 | 用途 |
|------|------|
| [`assets/lua5.4/`](assets/lua5.4/) | Lua 5.4 标准库 EmmyLua 类型注释（`basic.lua`、`string.lua`、`table.lua`、`math.lua`、`io.lua`、`os.lua` 等 11 个文件），作为内置类型定义的参考来源 |
| [`tests/lua-root/`](tests/lua-root/) | **手工端到端测试目录**（详见下方说明）：用于在 Extension Development Host 中实际体验 LSP 能力 |
| [`tests/complete/`](tests/complete/) | 补全功能测试 Lua fixture（17 个文件）：局部变量、表字段、require、class 方法、智能补全 |
| [`tests/hover/`](tests/hover/) | Hover 功能测试 Lua fixture（18 个文件）：EmmyLua class、链式调用、require 模块、表展开、函数返回类型 |
| [`tests/define/`](tests/define/) | 跳转定义测试 Lua fixture（7 个文件）：局部/全局定义、require 跳转、文件夹 init.lua |
| [`tests/parse/`](tests/parse/) | 解析测试 Lua fixture（2 个文件）：语法错误恢复、各种语句类型 |
| [`tests/project/`](tests/project/) | 多文件工程级测试 Lua fixture（5 个文件）：全局变量跨文件、枚举段 |

#### `tests/lua-root/` + `tests/lua-root2/` — 手工端到端测试目录

这两个目录一起被 [`tests/mylua-tests.code-workspace`](tests/mylua-tests.code-workspace) 作为**两个 workspace folder** 挂载，用于在 Extension Development Host 中人工验证 LSP 行为（含跨 workspace 场景）。与 `tests/` 下其他目录的区别：其他目录是 Rust 集成测试读取的 fixture，而这两个目录是人手动操作的端到端体验环境。

**启动方式**：运行 `.cursor/scripts/test-extension.sh`（macOS/Linux）或 `.cursor/scripts/test-extension.ps1`（Windows），脚本会自动构建 LSP + 扩展并打开这两个 workspace。参见 Skill `.cursor/skills/test-extension/` 和 [`tests/lua-root/README.md`](tests/lua-root/README.md)、[`tests/lua-root2/README.md`](tests/lua-root2/README.md)。

**`tests/lua-root/` 文件清单**：

| 文件 | 覆盖场景 |
|------|----------|
| `main.lua` | 入口；require 跳转、module return 类型、跨 workspace require、跨文件全局调用、completion 测试点 |
| `math_utils.lua` | `return M` 模块风格；`@overload` / `@vararg` / `@deprecated` / `@async` / `@nodiscard`；复杂类型（union / optional / array / fun() / table shape / 泛型）|
| `emmy_basics.lua` | `@class` / `@field` / `@alias`（字面量 + union 字符串）/ `@enum` / `@type` |
| `emmy_types.lua` | EmmyLua 类型表达式全覆盖：union、optional、array、`T<U>`、`fun()`、`{k:v}`、括号分组 |
| `player.lua` | OOP：`@class A: B,C` 多继承、self 方法、字段；全局 `Player` 跨文件使用 |
| `scopes.lua` | 作用域树全部 block 类型、参数、vararg、隐式 self、`local x = x + 1` 语义、closure |
| `generics.lua` | `@generic T`（函数级）+ `@class C<T>`（容器）+ 泛型参数替换 |
| `diagnostics.lua` | 预期诊断清单（每行 `-- !diag:` 标注）：undefinedGlobal / emmyTypeMismatch / emmyUnknownField / luaFieldError / luaFieldWarning / syntax error |
| `refs_rename.lua` | references / rename / semantic tokens（defaultLibrary）|
| `json.lua` | 真实第三方库（json4lua）解析健壮性 |
| `UEAnnotation/test_utils.lua` | UE4 场景：多继承 `T3: T1,T2`、`---@return` 链式调用、UE 风格 stub 重写 |
| `UEAnnotation/ue-comment/ue-comment-xxxxx.lua` | UE4 自动导出风格：`--[[ ]]` + `---@class` 继承链、子目录 require |

**`tests/lua-root2/` 文件清单**（跨 workspace 场景）：

| 文件 | 覆盖场景 |
|------|----------|
| `shared/config.lua` | 跨 workspace require：`lua-root/main.lua` 通过 `require("shared.config")` 引用 |
| `shared/logger.lua` | 跨 workspace require + `@overload` 示例 |
| `cross_globals.lua` | 跨 workspace 全局贡献（`AppName` / `Audit` 等），测试 workspace/symbol + 跨 root goto |

### Grammar — Tree-sitter 解析器（阶段 A 核心）

**BNF 规范**：[`grammar/lua.bnf`](grammar/lua.bnf)（Lua 5.3+/5.4 EBNF）+ [`grammar/emmy.bnf`](grammar/emmy.bnf)（EmmyLua 注解子语法）。

**解析器实现**（已完成并通过验证）：

| 文件 | 说明 |
|------|------|
| [`grammar/grammar.js`](grammar/grammar.js) | Tree-sitter 文法：15 种语句、12 级优先级表达式、table/function/prefix 完整语法；EmmyLua 注解产生式已定义 |
| [`grammar/src/scanner.c`](grammar/src/scanner.c) | 外部扫描器：短字符串（全部 Lua 5.3+ 转义）、长字符串、所有注释类型、shebang、**column-0 块边界** |
| [`grammar/test/corpus/`](grammar/test/corpus/) | 37 个回归测试，100% 通过 |

**定制扩展 — Column-0 块边界**：行首 column 0 处的关键字/标识符强制关闭未配对的嵌套块，让缺少 `end` 的错误在下一个顶层语句处即时报出。嵌套代码必须缩进。详见 BNF §2.1.1。

- 无错误解析验证：`tests/lua-root/test.lua`、`tests/lua-root/json.lua`、`assets/lua5.4/` 全部 11 个标准库桩文件。
- 命令：`cd grammar && npm install && npx tree-sitter generate && npx tree-sitter test`

### LSP — Rust 语言服务器（阶段 C 完成）

**技术栈**：Rust + `tower-lsp-server` 0.23 + `tree-sitter` 0.26 + `tokio`。

| 路径 | 说明 |
|------|------|
| [`lsp/Cargo.toml`](lsp/Cargo.toml) | Cargo workspace root |
| [`lsp/crates/tree-sitter-mylua/`](lsp/crates/tree-sitter-mylua/) | 包装 crate：`build.rs` 编译 `grammar/src/` 的 C parser，导出 `LANGUAGE` |
| [`lsp/crates/mylua-lsp/`](lsp/crates/mylua-lsp/) | LSP server（lib + bin 架构，24 个模块 + 9 个集成测试文件） |

**已实现 LSP 能力**：
- `initialize` / `shutdown` / 文档同步（**Incremental sync** + tree-sitter 增量 reparse：`tree.edit(&InputEdit)` → `parse(new, Some(old))`，只改动区域的子树被重建）
- **位置编码**：LSP Position 按 **UTF-16 code unit** 语义处理；`util::byte_col_to_utf16_col` / `utf16_col_to_byte_col` 在 tree-sitter 字节列与 LSP UTF-16 列之间转换，中文/emoji 等非 ASCII 行上 hover/goto/semantic token 也能对齐
- **配置体系**：15 个扩展配置项，通过 `initializationOptions` + `didChangeConfiguration` 下发；`require.aliases` 参与模块解析
- **语法诊断**：Tree-sitter ERROR/MISSING 节点自动转为 `publishDiagnostics`
- **documentSymbol**：顶层 function / local / assignment 提取为大纲
- **goto definition**：local 作用域 + 全局符号表 + `require` 跳转（优先跳到目标文件的 `return` 语句位置，回退到首个全局贡献）+ **点击 `local x = require("mod")` 的 LHS 名称直接跳转到模块 return**（正确处理 `attribute_name_list` 中 `<const>` / `<close>` 属性的索引偏移，`local x <const>, y = require(...)` 的 `y` 也能跳到模块）+ **AST 驱动的纯点号 dotted field 跳转**（`a.b.c` 通过嵌套 `variable` 节点的 `object`/`field` 字段递归解析；带下标 `a[1].b` 或方法调用 `a:m().c` 的 base 目前退化为 `TypeFact::Unknown`，后续补全）+ **Emmy 类型名跳转**（`type_shard`）；**多候选策略可配置**（`gotoDefinition.strategy`: Auto/Single/List）
- **hover**：定义源码 + EmmyLua 注解 + 文档注释 + **AST 驱动的推断类型展示**（`infer_node_type` 递归嵌套 `variable` 节点 → `resolve_field_chain`；同样覆盖纯点号链，下标/方法调用 base 返回 `Unknown` 而非伪造 stub）+ **Emmy 类型 hover**（class/alias/enum 区分展示、字段列表）+ **全局多候选提示** + **`@overload` 签名展示**
- **references**：单文件 local scope（用 `scope_tree.resolve_decl` 比对 `decl_byte`，正确区分 `local x = x + 1` 的 RHS 指向外层；shadowing 下不把被遮蔽的内部使用算作对外层的引用）+ 全工作区全局符号引用（`global_shard` + `type_shard` 声明 + 去重）+ **EmmyLua 类型名的注解内引用**（扫描所有索引文件的 `emmy_comment` / `comment` 节点文本，按 ASCII 词边界匹配，能收集 `---@type Foo` / `---@param x Foo` / `---@return Foo` / `---@class Bar : Foo` 等所有用法）；**点击注解内的类型名也能触发**（`find_node_at_position` 找不到 identifier 时回落到按源码字节范围提取 ASCII 词）；**声明包含策略可配置**（`references.strategy`: Best/Merge/Select）
- **workspace/symbol**：全局函数/变量 + **Emmy class/alias/enum**（`type_shard`）模糊搜索
- **EmmyLua 注解**：递归下降解析器（`emmy.rs`），完整支持类型表达式语法（union `|`、optional `?`、array `[]`、generic `<T>`、`fun()` 函数类型、`{k:v}` table 类型、括号分组）；注解标签 `@class`/`@field`/`@param`/`@return`/`@type`/`@alias`/`@enum`/`@generic`/`@overload`/`@vararg`/`@deprecated`/`@async`/`@nodiscard` 等；**泛型参数替换**（`EmmyGeneric` 变体；字段解析和补全中自动将 `T` 替换为实际类型参数）；**`@overload` 参与 FunctionSummary + hover 展示**；**`@alias` 右侧类型保存和展开**；**`@enum` 写入 TypeShard**
- **completion**：局部变量 + 全局名 + 关键字 + **AST 驱动的点号字段补全**（通过 `hover::infer_node_type` 递归 `variable` 节点的 `object`/`field`，不再用字符串 splitn）+ **冒号补全过滤方法**（结构化 `is_function` 判定）+ **`---@` EmmyLua tag 补全**（class/field/param/return/type/alias/enum/generic/overload/vararg/deprecated/async/nodiscard/see/meta/diagnostic/cast/operator/private/protected/package/public/readonly/version）+ **`require("…")` 模块路径补全**（来自 `require_map`）；**声明 `trigger_characters = ['.', ':', '@', '"', "'"]`** 让客户端自动触发
- **signatureHelp**：基于 `FunctionSummary` 的参数签名浮窗；支持 `foo(a, b)` / `obj.m(...)` / `obj:m(...)` / 跨文件 require 返回的 callable；`---@overload` 生成多个 SignatureInformation；**跨文件 class 声明/实现分离时合并 overloads**（`@class Foo` + `@field init fun(...)` 在 a.lua，而 `function Foo:init() end ---@overload ...` 在 b.lua 时，`@field` sig 与 b.lua 的 overloads 一起回传；visually-empty 的 self-only impl primary 自动过滤掉）；`active_parameter` 由 `(` 到光标间顶层 `,` 的计数得到（感知嵌套 `()` / `{}` / `[]` 与字符串、行注释）；方法调用时 `self` 不出现在显示参数列表
- **rename**：单文件 local + 全工作区全局（含 prepareRename）；**新名字校验为合法 Lua 标识符**（非法返回 `InvalidParams` JSON-RPC 错误，关键字拒绝）
- **semantic tokens**：全局变量 `defaultLibrary` + 局部变量标记（作用域感知）；**发出的列号为 UTF-16 code unit**。**设计取舍：刻意最小化，只补 TextMate 无法静态判定的语义区分**（全局 `defaultLibrary`、全局/局部、Emmy 类型名等）；`keyword` / `number` / `string` / 注释等基色交由 TextMate，**不做** token type 细分（详见 [`docs/requirements.md`](docs/requirements.md) §3.1）
- **foldingRange**：Tree-sitter walk 驱动。Region 折叠覆盖 `function/local function/function() ... end`、`do`/`while`/`for`/`repeat`、`if/elseif/else` 以及多行 table constructor；`end_line = end_row - 1` 保留闭合关键字可见。Comment 折叠覆盖多行 `--[[ ... ]]` / `--[=[ ... ]=]` 长块注释和连续的 `---@tag` 注释行（按行号合并相邻 `emmy_comment`）；`end_line = end_row` 整块折成一行。单行构造自动跳过
- **documentHighlight**：同文件 identifier 同义高亮，按 AST 祖先分 Read/Write —— 局部/函数/for-var/参数声明处发 Write，`assignment_statement` LHS 发 Write，其他发 Read。作用域感知：光标落在 local 上时通过 `scope_tree.resolve_decl` 过滤掉被 shadow 的同名占用；`local x = x + 1` 的 RHS `x` 正确归属于外层；全局/Emmy 类型名无 scope decl 时退化为文本匹配
- **语义诊断**：未定义全局变量 + **Emmy 类型未知字段访问** warning + **Lua table shape 未知字段**（closed→error / open→warning，`luaFieldError`/`luaFieldWarning` 可配置）+ **Emmy 类型不匹配**（`---@type` 声明与赋值字面量类型冲突时报告，`emmyTypeMismatch` 可配置）；**诊断 enable/severity 受配置控制**
- **作用域树**（`scope.rs`）：arena-based `ScopeTree`，单趟 AST 遍历构建；支持 `function_body` / `do` / `while` / `repeat` / `if` / `for` 等所有块级作用域 + 参数 + for 变量 + 隐式 `self`；正确处理 `local x = x + 1` RHS 引用外层变量的 Lua 语义

**索引架构（步骤 1-7）**：
- `summary_builder.rs`：单文件 AST → DocumentSummary；支持文件级 `return` 语句的**类型与源 range 提取**（`module_return_type` + `module_return_range`，后者供 `require` goto 跳到 return 行）、递归进入 `if`/`do`/`for`/`while` 块（含 local/emmy_comment）收集全局赋值和函数声明、全局函数 `GlobalContribution` 携带真实 `FunctionSignature`、**冒号方法调用生成 CallReturn stub**、**Known(EmmyType) base 生成 TypeRef**；**Open/Closed shape 判定**（动态 bracket key 写入自动 `mark_open()`）
- `aggregation.rs`：WorkspaceAggregation（GlobalShard / TypeShard / RequireByReturn）；同名全局候选按 URI 路径深度优先排序（浅路径 > 深路径）；`resolve_module_to_uri` 优先查 `require_map`；**精细化级联失效**（签名指纹变化仅标脏受影响的缓存条目，非全量标脏）；**`require_map` 在 `upsert_summary`（重新索引）中保留，只在 `remove_file`（文件删除）中清除**——编辑已打开文件不会丢失 "别人能 require 到我" 的映射；legacy `globals` 字段已移除，所有消费方统一使用 `global_shard`
- `resolver.rs`：跨文件 stub 链式解析 + 缓存 + 环路保护；`resolve_require` 基于目标文件 `module_return_type` 解析模块返回值类型；`resolve_field_chain` 对 table-extension 全局变量支持 global_shard 限定名回退；Emmy 继承链字段解析（沿 `parents` 递归）+ **alias 类型展开**（`resolve_emmy_field` 自动跟踪 alias 目标）；`collect_fields` / `resolve_table_field` 强制 `source_uri`；**EmmyGeneric 类型的字段/补全支持**；**CacheKey 语义分离**（`Global`/`Type`/`FieldAccess` 独立变体）
- `summary.rs`：`DocumentSummary` 含 `module_return_type`；`TypeDefinition` 含 `parents`（继承链）、`alias_type`（别名目标）；`FunctionSummary` 含 `overloads`；签名指纹包含全局类型信息和 module return
- `workspace_scanner.rs`：**include/exclude glob 过滤**（`FileFilter` + `globset`），扫描和增量文件变更均受 `WorkspaceConfig` 控制
- 设计文档：[`docs/index-architecture.md`](docs/index-architecture.md) / [`docs/index-implementation-plan.md`](docs/index-implementation-plan.md)

- 构建：`cd lsp && cargo build`
- 测试：`cd lsp && cargo test --tests`

**独立测试框架**（无需 VS Code 联调）：

LSP crate 采用 **lib + bin 拆分架构**：`lib.rs` 导出所有核心模块（hover / completion / goto / diagnostics 等），`main.rs` 仅为薄启动入口。集成测试直接调用核心函数，无需 LSP stdio 通信。

| 测试文件 | 测试数 | 覆盖功能 |
|----------|--------|----------|
| `test_parse.rs` | 8 | 基础解析、EmmyLua 注解、方法链、for 循环、fixture 文件 |
| `test_hover.rs` | 15 | 局部变量、表字面量、EmmyLua class 返回类型、链式调用、块注释文档、函数声明处 hover、点号变量 base/field 区分、**链中间字段 AST 驱动 hover** |
| `test_completion.rs` | 11 | 局部变量补全、点号字段补全、class 方法、关键字、去重、**`---@` tag 补全**、**`require("…")` 模块路径补全**、**AST 驱动的点号 base 对方法调用链不 spill 全局列表** |
| `test_signature_help.rs` | 10 | 简单 local 函数签名、参数进度、嵌套 `{}` 里的逗号不推进、`---@overload` 多签名、非 call 位置返回 None、`:method` 调用隐藏 self、**table-call (`foo{}`) active_param 恒为 0**、**同名方法 class 消歧（只出当前 class 的 overload）**、**跨文件 class 声明/实现分离时合并 @field sig + impl overloads**、**`@field` sig 不被同名 top-level global function 覆盖（P0-R3 bare fallback 移除）** |
| `test_goto.rs` | 10 | 局部变量、函数、参数、for 变量、嵌套作用域跳转、**require LHS 跳到 module return**、**`local x <const>, y = require(...)` 正确处理 attribute 索引偏移**、**非 ASCII 行上 UTF-16 位置对齐**、**semantic token 列数为 UTF-16 unit** |
| `test_scope.rs` | 11 | 函数体内 local 解析、声明站点、参数、for 变量、嵌套遮蔽、`local x = x + 1` 语义、`:method` self、visible_locals |
| `test_diagnostics.rs` | 9 | 干净代码无诊断、语法错误检测、语义诊断、**LHS 链式赋值不误报 unknown field**、**closed table RHS 仍报错**、`@type` 不匹配、enum workspace symbol、泛型字段替换 |
| `test_symbols.rs` | 5 | 函数声明、方法声明、空文件、fixture 文件 |
| `test_folding_range.rs` | 13 | 空文件/单行不折叠、function 体、嵌套 if/for、repeat/while/do、for 数值+泛型、多行 table、块注释、行注释不折、emmy 注释连续行合并、带 `=` 层级的块注释 |
| `test_document_highlight.rs` | 10 | 局部 Read/Write、参数 Write、for 数值/泛型循环变量、function 声明名、**shadowing 尊重 scope**、全局变量、`local x = x + 1` 内外层区分、**`t.x = 1` / `t[k] = v` 的 base 分类为 READ**、空文件 |
| `test_references.rs` | 8 | 局部引用、参数引用、包含/排除声明选项、**`local x = x + 1` RHS 不算新 local 的引用**、**shadowing 下不把被遮蔽的内部使用算作外层引用**、**Emmy 类型名扫注解内 `---@type/@param/@return/@class : Foo` 全部引用**、**词边界不匹配 `FooBar` 子串** |
| `test_workspace.rs` | 7 | 多文件 hover/completion/goto、require 解析、project 目录、全局优先级排序、**upsert 后 require_map 不丢失** |

内嵌单元测试（`src/*.rs` 中的 `#[cfg(test)] mod tests`）：`util.rs` 覆盖 UTF-8 ↔ UTF-16 列转换、LSP Position 转字节偏移、`apply_text_edit` 单行/跨行编辑的 `InputEdit` 构造；`lib.rs` 覆盖 `percent_decode` 的 UTF-8 多字节解码（中文路径）；`rename.rs` 覆盖 Lua 标识符校验；`signature_help.rs` 覆盖 `count_top_level_commas` 的嵌套 `{}`、未终止 `--[[` 不误计 trailing `,`、正确闭合的块注释等场景；`workspace_scanner.rs`、`emmy.rs` 含大量单元测试覆盖模块路径推导、注解解析等。总计 **80+ 单元测试**。

测试工具模块 `test_helpers.rs` 提供：`parse_doc()`、`setup_single_file()`、`setup_workspace_from_dir()` 等函数，可从 `tests/` 下的 Lua fixture 目录构建完整工作区上下文。

> **注意**：如果 VS Code 扩展正在运行会锁住 `mylua-lsp.exe`，可用独立 target 目录避免冲突：
> `$env:CARGO_TARGET_DIR="target-test"; cargo test --tests`

### VS Code Extension（已实现）

| 文件 | 说明 |
|------|------|
| [`vscode-extension/package.json`](vscode-extension/package.json) | 扩展清单：语言注册、TextMate grammar、配置项 |
| [`vscode-extension/syntaxes/lua.tmLanguage.json`](vscode-extension/syntaxes/lua.tmLanguage.json) | TextMate grammar：Lua 基础语法（关键字、字符串、数字、注释）+ 完整 EmmyLua 注解着色（16 种 `@tag` 结构化匹配、`fun()`/`{}`/`()` 嵌套类型表达式、内置类型 vs 用户类型区分、点号类型名） |
| [`vscode-extension/src/extension.ts`](vscode-extension/src/extension.ts) | LSP 客户端：启动 `mylua-lsp` 二进制（开发时自动查找 `lsp/target/debug/`） |

- 构建：`cd vscode-extension && npm install && npm run compile`
- 调试：F5 启动 Extension Development Host

### 索引生命周期与性能（步骤 7 已完成）

- **索引状态机**：`Initializing` → `Ready` 两状态；`Initializing` 阶段跳过语义诊断，`Ready` 后自动补发已打开文件的完整诊断
- **进度通知**：冷启动通过 `window/workDoneProgress`（`$/progress`）向客户端报告索引进度百分比
- **并行冷启动**：使用 `rayon` 并行解析 + 生成 Summary，按 200 文件一批流式 merge 到聚合层
- **增量解析**：编辑时 `textDocument/sync = Incremental`；对每个 content change 先 `apply_text_edit`（把 UTF-16 Range 转成字节偏移并 splice 文本；越界 Position 会 clamp 到 EOF 以 append 而非损坏文档，并写入日志告警），再 `tree.edit(&InputEdit)` 通知 tree-sitter，最后 `parser.parse(new_text, Some(&old_tree))`——未变区域的子树原地复用；解析失败时先尝试 fresh parse，仍失败则保留旧 Document 而不是让文件 "消失"
- **并发安全**：每个 URI 一把 `Arc<tokio::sync::Mutex<()>>`（`Backend::edit_locks`），`did_open` / `did_change` 在处理前 `.await.lock()`，防止同一文件的两次编辑在 `remove → process → insert` 的两阶段之间交错；不同 URI 并行不受影响；`did_close` 清理对应条目，HashMap 有界
- **诊断调度**：编辑期语义诊断采用 300ms 去抖（generation counter 去重），syntax 诊断即时发布；**签名指纹变化时自动级联调度**已打开的依赖方文件重算语义诊断（依赖 `require_by_return` 反向索引，而 `require_map` 在编辑重入时被保留，跨文件依赖不会失联）
- **磁盘持久化缓存**：`mylua.index.cacheMode = "summary"` 时，`DocumentSummary` 序列化到用户缓存目录（FNV 内容哈希 + schema 版本 + 配置指纹三维失效）；缓存命中跳过 Summary 重建
- **文件过滤**：`workspace.include` / `workspace.exclude` glob 配置在冷启动扫描和增量文件变更中均生效
- **依赖**：`rayon` 1.x（并行处理）、`globset` 0.4（glob 模式匹配）