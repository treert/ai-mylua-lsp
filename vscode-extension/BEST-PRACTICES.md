# 推荐写法

mylua-lsp 的静态分析建立在少量明确的约定之上：符合约定的写法能获得精确的跳转、类型与诊断；偏离约定的惯用法不是「待支持」，而是**有意不覆盖**。本文列出这些取舍与对应的推荐写法。

## 注释优于抑制

宿主程序注入的全局符号（引擎 API、框架回调等）没有 `require` 来源，也不在工作区代码中。推荐用 `---@meta` stub 描述它们，放到 `mylua.workspace.library` 指向的目录：

```lua
---@meta
---@class GameEngine
---@field fps number 引擎当前帧率
GameEngine = ...
```

存在性守卫（`if X then … end` 等条件中**嵌套**的读取）可以抑制 `undefinedGlobal` 等诊断，但**不带来类型信息**——hover、跳转、参数检查仍然缺失。以下惯用法有意**不**作为守卫支持：

```lua
if not P then return end   -- early return
assert(P)                  -- assert
if not P then P = {} end   -- 懒初始化
```

理由是产品取向而非难度：注释是精确且可预测的，抑制永远是启发式的；覆盖越多「不写注释」的写法，就越多掩盖真实 bug 的途径。需要存在性保证时请写注释，而不是期待诊断静默。

## `_ENV` 沙箱：两种推荐写法

`__index` 的实际指向不做追踪：凡是带元表（或类型推不出）的重定向环境，一律按 `{__index=_G}` 处理。推荐按以下两种写法之一构建沙箱：

```lua
-- 写法一（严格）：需要的全局提前 local 保存
local print = print
_ENV = {}
print("ok")   -- 使用提前保存的 print
x = 1         -- 沙箱内合法写入
-- 读取沙箱里不存在的名字会报 envUnknownField；
-- 连 print、_G 也不豁免——提前 local 保存正是标准沙箱该有的样子
```

```lua
-- 写法二（可读全局、不写全局）
_ENV = setmetatable({}, { __index = _G })
print("ok")   -- 环境表没有则回退查全局
x = 1         -- 写入沙箱，不污染全局索引
```

补充约定：

- 想恢复全局环境必须**提前**捕获：`local G = _G` 再 `_ENV = G`，或直接 `local _ENV = _G`。先重定向再 `_ENV = _G` 无效——那时 `_G` 已是 nil，该语句实际是 `_ENV = nil`。
- `_ENV = expr` 是位置敏感的：赋值前后的自由名属于不同环境，同名也不互相关联。
- 声明文件（内置 stdlib、`mylua.workspace.library` 下的 stub）中**不得**出现 `_ENV = {}`——它是真实生效的重绑语句，其后所有赋值形式的全局（`X = {}`）都会静默失去全局导出；函数形式（`function f() end`）不受影响，因此故障极为隐蔽。`_ENV` 无需任何声明。

## 表字段用静态字符串键

三种写法得到同一结论：`{ Name = v }`、`t.Name = v`、`t["Name"] = v`。想让字段被跳转、补全、诊断覆盖，就用静态字符串键。

动态键 `t[k] = v`（非常量下标）会让这张表转入「字段集合非穷尽」状态：点号读取未知字段从 `luaFieldError` 降级为保守的 `luaFieldWarning`。这不是误报，而是「静态记录不完整」的如实表达；需要精确检查请补 `---@class` 注解。

## require 用静态字符串

`local m = require("mod.name")` 才会建立绑定（跳转定义、返回值类型推断）；非静态参数、拼接路径不建绑定。使用 `@utils.foo` 这类路径别名时必须配置 `mylua.require.aliases`（最长前缀匹配），否则别名模块的跳转与类型推断都无法解析。

## 注解比推断优先

诊断采用「Emmy 路径严格、Lua 路径保守」：命中 `---@class` 等注解则按注解严格检查（`emmyTypeMismatch` / `emmyUnknownField`），否则按 table shape 保守推断。多候选跳转的打分优先级同样是 **Emmy 定义 > 显式注解 > shape 推断**。给公共 API 补上 `---@param` / `---@return`，即可从「保守不报」升级为「精确检查」。

## 想被全局搜索，就写全局或注解

工作区符号搜索收录：全局变量/函数、`---@class` / `---@alias`、类成员函数。不收录：局部变量、普通 table 内部字段。希望模块成员可被搜索，用 `return M` 模式导出全局，或为其写注解。

## `---@diagnostic` 抑制

支持 `disable-next-line` / `disable-line` / `disable` … `enable`，逗号分隔诊断 code 或通配符 `*`。抑制应是最后手段：先考虑能否用注释让静态分析真正理解代码。

---

## 附：支持的注解语法

完整支持类型表达式语法：union `|`、optional `?`、array `[]`、generic `<T>`、`fun()` 函数类型、`{k:v}` table 类型。

支持的标签：`@class` / `@field` / `@param` / `@return` / `@type` / `@alias` / `@enum` / `@generic` / `@overload` / `@vararg` / `@deprecated` / `@async` / `@nodiscard` / `@customrequire` 等。
