# Changelog

MyLua LSP 扩展的版本变更记录。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

<!-- 维护说明：发版前在下方 [Unreleased] 段追加条目，使用 Added / Changed / Fixed / Removed 小节，每条一行。
     执行 `npm run release` 时：若 package.json 版本与下方最新已发布版本不一致（新版本首发），校验 [Unreleased] 非空，
     通过后自动把本段改名为 [版本号] - 日期，并在顶部新开一个空 [Unreleased] 段；若版本一致（同一版本多平台重复发布），
     则跳过校验与改名，CHANGELOG 保持原样。发布后请手动提交 CHANGELOG.md（或用 `--git` 自动提交）。 -->

## [Unreleased]

### Added
- 支持 Lua 5.2+ 的 `_ENV` 环境语义：自由名 `x` 按语言定义等价于 `_ENV.x`。当 `local _ENV = t`、`_ENV` 形参或 `_ENV = t` 使环境指向别的表时，该作用域内的自由名读写会正确识别为那张表的字段——不再错误写入全局索引污染整个工作区，也不再对沙箱内的自由名误报 "Undefined global"。`local _ENV = _G` 与默认环境行为一致；`_ENV` 指向未知类型时保持静默，不猜测字段是否存在。

### Fixed
- 修复 `_ENV` 被错误高亮为全局变量（带 `global` 语义修饰符）的问题：`_ENV` 是闭包 upvalue 而非全局变量，现按局部变量着色。
- 修复 `local x = _ENV` 无法解析的问题：`_ENV` 的值即全局环境表，因此 `x.SomeGlobal` 现可正确跳转与 hover。
- 修复 `_ENV.foo = 1` 产生伪条目 `_ENV.foo` 的问题，现正确登记为全局 `foo`。
- 修复 `_ENV = {}` 被误登记为一个名为 `_ENV` 的全局变量的问题（`_G._ENV` 在 Lua 中恒为 nil）。
- 修复 `g = 1; _ENV = {}; g = 2` 中两个 `g` 被视为同一符号的问题：它们属于不同的环境表，跳转与查找引用不再互相关联。

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
