# Changelog

MyLua LSP 扩展的版本变更记录。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

<!-- 维护说明：发版前在下方 [Unreleased] 段追加条目，使用 Added / Changed / Fixed / Removed 小节，每条一行。
     执行 `npm run release` 时：若 package.json 版本与下方最新已发布版本不一致（新版本首发），校验 [Unreleased] 非空，
     通过后自动把本段改名为 [版本号] - 日期，并在顶部新开一个空 [Unreleased] 段；若版本一致（同一版本多平台重复发布），
     则跳过校验与改名，CHANGELOG 保持原样。发布后请手动提交 CHANGELOG.md（或用 `--git` 自动提交）。 -->

## [Unreleased]

### Added
- 新增配置项 `mylua.diagnostics.narrowByConditionGuard`（bool，默认开启）：当一次读取已被存在性检查包裹时，抑制 `Undefined global` 与 `Unknown field` 诊断。典型场景是宿主程序（通常是 C++）在运行时把符号注册进 Lua 全局表，工作区里查不到定义，于是脚本先探测再使用——此时报错纯属噪音，而满屏警告往往导致用户直接关掉整类诊断。识别 `if X then …`、`elseif X then …`、`if X == nil then … else …`、`if not X then … else …`、`while X do …`、`X and X.f()` 六种形态，条件形式支持 `X` / `X ~= nil` / `nil ~= X` / `not X` / `X == nil` / `nil == X`（`not` 与 `== nil` 翻转极性）。守卫以**访问路径**为键而非仅变量名，故 `x.m_some` 与全局名同等处理，且对前缀的检查覆盖更深的读取（检查 `x.cfg` 也覆盖 `x.cfg.opt`）。
  **这不改变任何类型推断结果**：名字仍保持原有类型（通常未知），仅丢弃诊断。写全 `---@class` / `---@meta` stub 仍是获得真实类型信息的唯一正解，本能力只为尚未写到那一步的代码降噪。要求读取在词法上嵌套于守卫区域内；`if not X then return end`、`assert(X)`、`if not X then X = {} end` 这类「保证其后语句」的写法**有意不支持**——抑制覆盖得越多，写注释的动力越少，而注释才是精确可预测的那条路。`or` 右操作数与 `repeat … until`（条件在循环体之后求值）同样不构成守卫。

### Changed
- `if` / `elseif` / `else` 的每个分支体现在各自拥有独立的词法作用域，互为兄弟挂在一个不含声明的 `if` 外壳下。条件表达式落在外壳而非任何分支内，符合 Lua 语义（条件在它守卫的分支之外求值）。
- `a or b` 的类型推断改为「优先取 `a`，仅当推断不出 `a` 的类型时才回退到 `b`」，此前无条件取 `a`。`a and b` 仍取 `b`（`a` 通常只是条件）。三处平行实现（类型推断、诊断类型兼容、summary 构建）统一到同一判定规则，不再各自漂移。

### Fixed
- 修复 `else` 分支能错误看到 `then` 分支中声明的 `local` 的问题：`if c then local a = 1 else print(a) end` 中的 `a` 此前会解析到 `then` 分支的声明，实际运行时该变量在 `else` 分支不存在。各 `elseif` 分支之间、以及分支与 `if` 语句之后的代码同样不再互相泄漏。
- 修复光标停在分支尾部空行时补全看不到该分支 `local` 的问题：tree-sitter 把分支节点的结束位置定在最后一条语句处，作用域区间现延伸至下一个分支或 `end`。

## [0.2.19] - 2026-08-31

### Added
- 沙箱环境 `local _ENV = setmetatable({}, { __index = _G })` 现已支持跳转到定义、hover 与查找引用。语言服务不追踪 `__index` 的实际指向，而是采用约定：**凡带元表（或类型推断不出）的环境一律视作 `{ __index = _G }`**，即可读取全局表、但写入不进全局表。这覆盖了内联写法、分离式 `local t = {}; setmetatable(t, mt); _ENV = t`、`_ENV` 形参、工厂函数返回值等所有情形。无元表的干净沙箱（`_ENV = {}`）不受影响，仍按字段集合完全已知严格处理。
- 沙箱内写入的名字现在可以跳转、hover 与查找引用。此前当 `_ENV` 的类型推断不出具体表时（如 `setmetatable(...)` 的返回值），`x = 1` 既不进全局索引也不进任何表结构，名字凭空消失。现在 `_ENV` 总会绑定到一张表结构上，写入有明确归属，同时仍不会污染全局索引。

### Changed
- 沙箱环境下读取不存在的名字现在会给出诊断。按上述 `{ __index = _G }` 约定，一个名字若既不在沙箱表中、也不在全局索引中，运行时就是 `nil`，因此报 `Undefined global`。此前这类环境下诊断全面静默，会掩盖真实的 nil 访问。
- 补全现在会考虑当前 `_ENV`：无元表的干净沙箱（`_ENV = {}`）只提示该表自身的字段，不再提示运行时为 `nil` 的全局名（此前提示了、一旦选用就被诊断立刻标红）；带元表的沙箱按约定同时提示沙箱字段与全局名。可见的 `local` 变量不受影响。
- `setmetatable(t, …)` / `rawset(t, …)` 现在在构建侧对目标表结构标记「字段集合不完整」。这是唯一能让所有消费方（导航回退、`envUnknownField` 诊断静默、`luaFieldWarning` 严重度降级）在一处对齐的地方。原先 `diagnostics/env_field.rs` 里的私有扫描副本已退役。

### Fixed
- 修复通过 `_G._G` / `_ENV._G` 二次限定读取全局时的误报与跳转失效：`_G._G.X`、`_ENV._G.X` 与裸 `X` 在 Lua 中是同一个全局（`_G._G == _G`），但此前只有写入侧做了归一，读取侧会报 `Unknown field '_G' on type '_G'`，且跳转与 hover 落空。现在读写两侧行为一致。

## [0.2.18] - 2026-08-30

### Added
- 支持 Lua 5.2+ 的 `_ENV` 环境语义：自由名 `x` 按语言定义等价于 `_ENV.x`。当 `local _ENV = t`、`_ENV` 形参或 `_ENV = t` 使环境指向别的表时，该作用域内的自由名读写会正确识别为那张表的字段——不再错误写入全局索引污染整个工作区，也不再对沙箱内的自由名误报 "Undefined global"。`local _ENV = _G` 与默认环境行为一致；`_ENV` 指向未知类型时保持静默，不猜测字段是否存在。
- 新增诊断 `mylua.diagnostics.envUnknownField`（默认 `warning`，抑制码 `env-field`）：环境被重定向后，读取新环境中不存在的名字会给出提示，区分"该字段根本不存在"与"赋值在读取之后"两种情形。仅在 `_ENV` 指向**形状完全已知**的表、且读取位于 chunk 顶层直线执行流时触发；跨函数体边界、顶层分支内赋值，以及 `setmetatable`（如常见沙箱写法 `setmetatable({}, {__index = _G})`）、`rawset`、动态键写入（`_ENV[k] = v`）等使表结构不再确定的情形均保持静默。
- 沙箱（`_ENV` 重定向）内的自由名现已支持跳转到定义、hover 与查找引用，解析为新环境表的字段。环境形状未知时保持静默；补全仍不提供。

### Changed
- `_G` 不再无条件视为全局环境表：`_G` 是全局表的一个普通字段，因此 `_ENV` 被重定向后新环境同样不提供它。现在自由名解析**先判环境重定向、后判内置 `_G`**。`local _ENV = _G` 与无重定向的常规情形行为不变，`_G.X` 也仍然不依赖 stdlib 中的 `_G = {}` 声明。
- 重构：跳转、hover、查找引用改为共用同一套「裸标识符指向什么」的解析实现（局部 → 重定向 `_ENV` 字段 → Emmy 类型名 → 全局）。此前三者各自内联了一份解析顺序，导致同一条语义规则只在其中一个能力生效——`_ENV` 相关的三处缺陷（见 Fixed）都源于此。点号字段、`require` 绑定与标签跳转仍由各能力自行处理。
- `_G` 改为语言服务内置识别，不再依赖内置 stdlib 中的 `_G = {}` 声明：即使未配置任何 `mylua.workspace.library`，`_G.X` 也能正确解析、跳转与诊断。内置 stdlib 相应移除了 `_G = {}`（保留 `---@class _G` 以提供 hover 文档），并移除了 `_ENV = {}`（`_ENV` 由每个 chunk 的隐式声明提供）。

### Fixed
- 修复 `_ENV` 被重定向后，点号形式的写入仍会污染全局索引的问题：`Foo.bar = 1`、`function Foo.f() end` 会被错误导出为全局 `Foo.bar` / `Foo.f`，`_G.x = 1` 更隐蔽——经 `_G.` 前缀规范化后登记成裸键 `x`。这些写入在运行时都是对 nil 取索引，现在一律不登记。
- 修复沙箱内 `function foo() end` 的符号凭空消失的问题：它既未进全局索引、也未写入新环境的结构，导致跳转、hover、查找引用全部失效（而等价的 `foo = function() end` 一直正常）。现已与赋值形式对称，写入新环境。
- 修复沙箱内自由名 hover 无内容的问题：现可显示其在新环境中的定义与类型；环境形状未知时保持静默。
- 修复沙箱内自由名的查找引用不区分环境边界的问题：`g = 1; _ENV = {}; g = 2` 中两个 `g` 运行时是不同变量，此前点击任一个都会返回全部四处；现在各自只返回所属环境的引用（跳转此前已正确）。
- 修复 `_ENV` 被重定向后 `_G.X` 仍解析到重定向前的全局符号的问题：跳转与 hover 会落到运行时不可达的符号上，且诊断给出误导性的 "Unknown field 'X' on type '_G'"（真实问题是 `_G` 本身为 nil）。
- 修复 `_ENV` 被错误高亮为全局变量（带 `global` 语义修饰符）的问题：`_ENV` 是闭包 upvalue 而非全局变量，现按局部变量着色。
- 修复 `local x = _ENV` 无法解析的问题：`_ENV` 的值即全局环境表，因此 `x.SomeGlobal` 现可正确跳转与 hover。
- 修复 `_ENV.foo = 1` 产生伪条目 `_ENV.foo` 的问题，现正确登记为全局 `foo`。
- 修复 `_ENV = {}` 被误登记为一个名为 `_ENV` 的全局变量的问题（`_G._ENV` 在 Lua 中恒为 nil）。
- 修复 `g = 1; _ENV = {}; g = 2` 中两个 `g` 被视为同一符号的问题：它们属于不同的环境表，跳转与查找引用不再互相关联。
- 修复内置 stdlib 中 `_ENV = {}` 导致 `_G`、`_VERSION` 未被索引，进而使 `_G.<字段>` 的相关诊断整体失效的问题（`print(_G.未定义字段)` 不再漏报）。

## [0.2.17] - 2026-08-30

### Fixed
- 修复局部变量作为全局表别名时，其上定义的函数被误报 "Unknown field"：`LuaPanda = {}; local this = LuaPanda; function this.f1() end` 之后调用 `this.f1()` 不再报警告（此前该函数未登记到全局索引）。
- 修复 `_G.X` 读取已定义全局时误报 "Unknown field"：`_G` 即全局环境表，`_G.X` 与裸 `X` 现统一视为同一个全局名，`_G.Foo`、`_G.Foo.bar`、`_G._G.Foo` 等写法均可正确解析、跳转与 hover。
- 修复 `_G.X = 1` 在工作区符号搜索（`workspace/symbol`）中产生 `Foo` 与 `_G.Foo` 两个重复条目的问题。
- 修复 `local _G = {}` 遮蔽全局环境时，其成员访问漏报 "Unknown field" 的问题：此时 `_G.X` 是普通 table 字段访问，不再享受全局别名待遇。
- 修复 `_G.` 触发的成员补全列不出任何全局符号的问题，现可正常列出工作区全局变量与全局函数。

## [0.2.16] - 2026-08-20

### Added
- 状态栏 tooltip 新增 server 进程内存显示：悬停状态栏第二行显示 `mem X.X GB`。server 通过新增的自定义通知 `mylua/memoryStatus` 推送采样值（索引完成后每 ~2 秒采样，变化 ≥ 1 MiB 才更新，内存平稳时不刷新），支持 Windows / Linux / macOS。

## [0.2.15] - 2026-08-19

### Added
- 新增扩展图标：深蓝月球 + 白色卫星，月球内白色 "Lua"、卫星内橙色 "M"（呼应 MyLua）。市场页显示 256×256 PNG（`assets/icon.png`），`.lua` 文件在资源管理器与编辑器标签页显示同设计 SVG 文件图标。设计源为 `assets/icon.svg`，PNG 由 `scripts/gen-icon.mjs` 光栅化生成。

## [0.2.14] - 2026-08-11

### Added
- 新增 `mylua.workspace.priorityKeyword` 配置项，支持自定义定义候选优先级关键词列表（默认 `["annotation"]`）。当多个文件定义同名符号时，路径含这些片段（大小写不敏感）的文件优先级更高。修改后需重启 LSP 生效。

## [0.2.13] - 2026-07-31

### Added
- 新增 `@customrequire` EmmyLua 注解，支持自定义类 require 函数。标记函数的某个参数为 module 路径参数，并可选地附带 regex 变换规则，使调用处返回值解析为目标 module 的返回类型。
- `@customrequire` 注解行 TextMate 语法高亮：tag、`param` 关键字、参数名、regex pattern、template 分别着色。
- `@customrequire` 调用处的字符串参数支持 documentLink 点击跳转（与 `require` 一致）。
- `@customrequire` 注解诊断：regex 编译失败、param_name 不匹配函数参数时给出 Warning。
- 支持全局函数、dotted 全局函数（`utils.custom_require`）、局部函数（`local function`）、局部变量赋值（`local f = function`）、局部表成员（`local M={}; function M.f`）、条件赋值全局变量（`if not X then X = {} end`）等所有常见定义方式。
- 支持同文件和跨文件调用场景。

## [0.2.12] - 2026-07-29

### Fixed
- 跨文件查找全局表字段引用时保留 TableShape owner，修复引用结果缺失。
- 跨文件补全全局表字段时保留 TableShape owner，修复补全候选缺失。

## [0.2.11] - 2026-07-28

### Fixed
- 修复合并类型后 hover / 补全中重复显示的问题。

## [0.2.10] - 2026-07-24

### Fixed
- 修复工作区扫描跟随符号链接，确保注解目录被正确索引。
- 调整 goto 定义候选排序，注解路径文件始终排在首位；改为返回 `LocationLink` 以避免客户端重排候选。

## [0.2.9] - 2026-07-24

### Added
- 新增 `mylua.references.scanComments` 配置项，控制普通注释（非 `---@`）中的类型名扫描，关闭可减少散文注释的误报。

### Fixed
- 修复类型名引用在赋值行重复出现的问题。

## [0.2.8] - 2026-07-23

### Fixed
- 修复 `FieldOf` 全局环路解析导致的栈溢出。
- 支持局部变量作为全局表别名的解析。

## [0.1.7] ~ [0.2.7] — 早期版本汇总

早期版本的完整提交历史见 `git log`，主要能力演进概述：

### Added
- Outline 视图：可配置详情级别（`compact` / `functions` / `allDeclarations` / `anonymousFunctions`），支持匿名函数嵌套显示。
- Hover：Markdown 格式化、类型链接、来源链接、字段按值类型排序、文档注释格式保留。
- EmmyLua：字段方法语法糖、类函数字段 hover 签名、表字段类型注解、泛型类方法返回值推断。
- Inlay hints：变量类型推断、参数名提示。
- 类型推断：Lua 逻辑表达式（`and` / `or`）类型推断。
- 诊断：监听文件变更并重新调度诊断；冒号调用普通函数字段的诊断。
- 索引：文件读取支持 UTF-8 / UTF-16；目录过滤排除子目录。

### Changed
- 热路径 AST kind 检查替换为 `SyntaxKind` 常量比较，提升解析性能。

## [0.1.6] 及更早

见 `git log`。
