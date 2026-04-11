# 需求说明

本文档描述 **Mylua LSP**（工作名）在功能与非功能上的约定边界，供架构与实现方案对齐。当前处于 **需求分析阶段**；实现细节见 [`architecture.md`](architecture.md) 与 [`implementation-roadmap.md`](implementation-roadmap.md)。

## 1. 产品形态

### 1.1 分体交付（硬性）

- **VS Code 扩展（Extension）** 与 **语言服务器（LSP Server）** 为 **独立工程、独立版本与发布节奏**，可 **并行开发**。
  - 扩展侧：激活、配置 UI、可选编辑器内嵌能力（见下）、**启动/连接** LSP（stdio / socket 等）、打包与发布到 Marketplace。
  - **LSP 侧**：完整语言理解、索引、诊断、协议实现；**不依赖** VS Code API，可单独作为 CLI / 其他 IDE 的后端。
- **目标**：同一 LSP 二进制（或等价制品）可被 **VS Code、Neovim、Emacs、自研工具** 等任意标准 LSP 客户端使用；扩展仅是其一类「宿主」。
- **源码仓库**：**Monorepo**（单 Git 仓库）管理 grammar、LSP、扩展等子工程；目录与发版约定见 [`implementation-roadmap.md`](implementation-roadmap.md) §2。

### 1.2 全栈自研范围

- **解析管线**与 **基色语法高亮**须 **自主掌控**（见第 3.1、3.6 节）：方便后续 **定制语法 / 方言 / 内嵌 DSL**。**TextMate**（基础着色）与 **Tree-sitter**（语法树）**分工不同、互不替代**，见 §3.1。
- 允许使用 **Tree-sitter 运行时** 等底层基础设施；**Tree-sitter 文法与 parser 集成**为自己仓库中的一等公民；**TextMate grammar** 同理自研维护，与 Lua 5.3+ / EmmyLua 注释展示对齐。

## 2. Lua 语言版本

- **仅支持 Lua 5.3 及以上**（含 5.3、5.4 及后续兼容版本语法）。
- **不支持**：Lua 5.1 / 5.2、LuaJIT 专有异构语法（除非与 5.3+ 语法重合则不单独建模）。
- **标准库**：诊断与内置符号可按 5.3+ 行为建模；若项目声明更高版本，可通过配置覆盖「假定版本」。

## 3. 功能需求

### 3.1 语法高亮（TextMate）与解析树（Tree-sitter）

**二者不冲突**：职责分离——**TextMate** 管「看起来像什么语法」的**基色**；**Tree-sitter** 管**结构化语法树**，主要供 LSP 做语义、索引与诊断；再用 **LSP Semantic Tokens** 叠加上一层**语义着色**。

- **基础语法高亮**：以 **TextMate**（VS Code `contributes.grammars`）**为主**。使用 **自研** TextMate grammar，覆盖 Lua 5.3+ 与 EmmyLua 注释区块的常见结构，便于与产品迭代同步。
- **语法树与解析**：以 **自研 Tree-sitter 文法** 生成 parser，部署在 **LSP Server** 内；依赖增量解析、紧凑树表示，支撑大文件与高频编辑下的解析与查询（与编辑器是否内置 Tree-sitter 高亮 **无必然关系**）。
- **语义高亮增强**：LSP 实现 **`textDocument/semanticTokens/*`**，在 TextMate 基色之上提供 **token type / modifier**（例如区分**全局变量与局部**、模块字段、`upvalue` 意图、可由产品定义的「强调」类等）。颜色由 **编辑器主题**将 semantic token 映射到具体配色；扩展可提供 **semanticTokenScopes** 等默认映射，便于用户与主题定制。
- **关系小结**：TextMate ↔ Tree-sitter **不负责互相证明**同一套着色规则；**对外一致**靠：自研 TextMate 与自研 Tree-sitter **共用同一语言边界与版本策略**（同一文档约定的 Lua 5.3+ / 注释语法），避免「肉眼看是关键字、树里是标识符」的长期漂移。

### 3.2 语义跳转与工作区范围（硬性）

以下均须在 **全工作区** 语义下可用（在可配置的包含/排除之内），而非仅当前打开文件：

- **转到定义**（含跨文件全局符号、Emmy 类型名、以及 `local` + `require` 绑定，见下）。
- **查找所有引用**（`textDocument/references`）。
- **工作区符号**（`workspace/symbol`，全库搜索符号）。

另需支持基于 EmmyLua 的 **类型侧导航**（如 `@class` / `@alias` 名）作为与跳转一致体验的一部分。

#### 3.2.1 工作区语义：不模拟「按需加载模块」，默认全局已见

**本产品设计刻意偏离**「只有 `require` 才看见别的文件」的常见 Lua 模块模型，以降低与游戏/脚本项目中「到处全局」写法的摩擦：

- **约定**：工作区内凡纳入索引的 `.lua` 文件，在静态分析上视为 **已经加载进同一套全局环境**（在遵守 Lua **`local` / 块作用域** 的前提下）。某文件顶层 `function foo()`、`foo = function` 等产生的 **全局名**，对其他文件 **默认可见**，**无需** 先 `require` 该文件。
- **例外（模块返回值绑定）**：仅当源码为  
  `local <name> = require("<路径或模块名>")`  
  时，将 `<name>` **绑定**到 **被解析目标文件** 的 **`return` 表达式**所代表的模块值（类型/形状由 AST + Emmy 近似）；用于跳转/Hover「这个局部是哪个文件 return 出来的」。  
  **字符串 → 文件** 的解析须 **可配置**（根目录、`package.path` 风格、别名等），与 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §1.2 及 [`architecture.md`](architecture.md) §3.4 概要一致。
- **其它形态的 `require`**（无 `local`、表达式再包一层等）：**不要求**一律建成与 `return` 的绑定；实现可逐步扩展，但不与本条「全局已见」模型冲突。

### 3.3 Hover

- 展示 **签名、文档注释**；在存在 EmmyLua 注解时，合并 **`---@param` / `---@return` / `---@type`** 等信息。
- **类型推断**：以「尽量提供有用信息、不确定则不瞎报」为原则；深度推断可持续迭代。

### 3.4 诊断

- **解析/语法类**：必须。
- **语义/类型类**：须支持 **开关与严重级别**；在全工作区分析时与 **5 万文件规模** 下的调度策略协同（后台、分阶段、可取消），避免拖死 UI 线程或 LSP 响应。

### 3.5 大纲（Outline / Document Symbol）

- 函数、表字面量中的命名字段、`local function` 等 **可导航结构**。
- **EmmyLua**：`---@class` 等作为 **分组或附加元数据**。
- 匿名函数、极深嵌套：**至少不崩溃**；展示粒度可持续优化。

### 3.6 自研解析与定制语法（演进）

- **解析结构单一真相**：**Tree-sitter 文法仓库（或 monorepo 内 grammar 包）** 为 LSP 侧 **词法/句法树** 的唯一来源；LSP 内 **禁止**与文法不一致的「另一套手写 parser」长期并存（可仅有错误恢复或二次 AST 的薄层）。**TextMate** 不参与构建 AST，仅服务 **基色展示**，与 §3.1 一致。
- **定制需求**：文法须支持 **版本化扩展**（例如附加产生式、`externals`、或语言变体 feature flag），以便在不 fork 整个工程的情况下演进方言。
- **与语义层契约**：Syntax tree → 语义模型 builder 的接口稳定、可测试，便于文法迭代时不推翻全部分析器。

## 4. EmmyLua 注释

- **目标**：与业界常用的 **EmmyLua 风格注解** 兼容。
- **第一优先级注解**  
  `---@class`、`---@field`、`---@param`、`---@return`、`---@type`、`---@alias`、`---@generic`、`---@enum`（及文档区块内常见变体）。
- **兼容性说明**：不保证与某一固定发布版工具 **字节级一致**；以多客户端实用互通为准，重大偏差在变更日志中说明。

## 5. 非功能需求（性能与规模）

- **规模目标**：单工作区约 **5 万个 Lua 源文件** 仍可日常使用。
- **指标（实现阶段具体化数值）**
  - **冷启动 / 索引**：全量或分层索引在约定时间内达到 **workspace-wide 查询可用**；支持渐进增强。
  - **交互延迟**：当前文件的定义 / Hover **保持低延迟**；**全工作区引用、符号搜索** 允许后台完成，但须 **可取消、有进度或可感知渐进结果**。
  - **内存**：须有 **上限与淘汰策略**；索引数据结构 **为 workspace-wide 查询优化**（如倒排、符号摘要、按需加载 AST）。

## 6. 配置与互操作

- **用户/工作区配置**：Lua 版本假定、`require` 路径与别名、诊断与索引 profile、包含/排除路径（glob）等；扩展负责 **下发到 LSP**，配置 schema 与 LSP `initializationOptions` / `workspace/didChangeConfiguration` 对齐。
- **多客户端**：LSP 对外行为一致；扩展 ID 与文档说明如何与其他 Lua 扩展共存。

## 7. 文档维护

- 变更 **架构、数据流、索引策略、文法边界或对外配置项** 时，同步更新 [`architecture.md`](architecture.md)、[`lsp-semantic-spec.md`](lsp-semantic-spec.md)（若跨文件行为变化）与本文。
