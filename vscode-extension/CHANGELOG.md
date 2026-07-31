# Changelog

MyLua LSP 扩展的版本变更记录。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

<!-- 维护说明：发版前在下方 [Unreleased] 段追加条目，使用 Added / Changed / Fixed / Removed 小节，每条一行。
     执行 `npm run release` 时会校验 [Unreleased] 非空，通过后自动把本段改名为 [版本号] - 日期，
     并在顶部新开一个空 [Unreleased] 段。发布后请手动提交 CHANGELOG.md（或用 `--git` 自动提交）。 -->

## [Unreleased]

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
