# 推荐写法

EmmyLua 风格的类型注解主要参考以下两份文档，通用注解写法（`---@class`、`---@param`、`---@return` 等）请直接查阅：

- <https://emmylua.github.io/annotation.html>
- <https://luals.github.io/wiki/annotations/>

本文其余部分以举例说明 mylua-lsp 中一些特殊且容易踩坑的写法。总原则：**命中注解则严格检查，否则按 table shape 保守推断**；符合约定的写法能获得精确的跳转、类型与诊断。

## require 用静态字符串

```lua
local m = require("mod.name")        -- 建立绑定：跳转定义、返回值类型推断
local n = require(prefix .. ".x")    -- 非静态参数：不建绑定
```

路径别名（如 `@utils.foo`）必须配置 `mylua.require.aliases`（最长前缀匹配），否则别名模块的跳转与类型推断都无法解析。

### `---@customrequire`：自定义 require 函数

用 `---@customrequire` 标记封装了 `require` 的函数，调用处即可获得与原生 `require` 相同的绑定（跳转定义、返回值类型、document link）：

```lua
---@customrequire param=module_name mgr_abc module_abc
---@param module_name string 模块名称
function utils.custom_require(module_name)
end

local a = utils.custom_require("mgr_abc.abc_mgr")
-- 实参经 "mgr_abc" → "module_abc" 替换后解析，等价于 require("module_abc.abc_mgr")
```

**注意：变换规则是 Rust regex 语法，不是 Lua pattern。** 转义点号写 `\.` 而非 `%.`；锚点 `^`/`$`、字符类 `[a-z]` 等均按 Rust regex 解释。删除前缀（只写 pattern 不写 replacement）是常见用法：

```lua
---@customrequire param=luaPath ^Gameplay\.
function _MgrMgr.CustomRequire(luaPath)
end

local a = _MgrMgr.CustomRequire("Gameplay.target")
-- 删除 "Gameplay." 前缀后解析，等价于 require("target")
```

其余约定：

- `param=参数名`：指定哪个参数承载模块路径；不带变换规则则实参字符串直接作为模块路径。
- replacement 为字面量模板，`$1`/`$2` 等是捕获组占位符。
- 非字符串实参（变量、拼接）静默降级，不建绑定。

## 泛型：`---@generic` 与 `---@class Container<T>`

函数级泛型（`---@generic T` + `---@param x T` + `---@return T`，支持 `T : ConstraintType` 约束写法）与泛型 class（`---@class Stack<T>`）均支持实例化推断：调用处类型参数替换后，字段与返回值能推断出实际类型（如 `Stack.new()` 配 `---@type Stack<string>` 后 `pop()` 推断为 `string?`）。完整示例见 [`tests/lua-root/generics.lua`](../tests/lua-root/generics.lua)。

## `mylua.workspace.priorityKeyword`：定义候选的排序

多个文件定义了同名全局符号或 `---@class` 类型时，跳转、hover 的候选按以下规则排序（逐级比较）：

1. **路径命中 keyword 的次数越多越优先**——对完整路径做大小写不敏感的子串匹配，命中次数累加；
2. 路径深度浅的优先（`/` 个数少者）；
3. 路径短的优先。

默认 `["annotation"]`，即路径含 `annotation` 的文件天然靠前。**旧项目补类型注解时特别有用**：把注解覆盖文件集中放到带关键字的目录，新旧定义并存时无需删旧代码，跳转与 hover 会优先命中注解版本：

```jsonc
// .vscode/settings.json
{
  "mylua.workspace.priorityKeyword": ["annotation", "types"]
}
```

```text
scripts/feature/config_manager.lua   -- 旧代码：ConfigManager = {...}（无类型信息）
annotations/config_manager.lua       -- 新增：---@class 注解覆盖
                                     -- 同名定义中排在前，类型信息优先生效
```

注意：修改此配置需重启 server——优先级在文件注册时计算并缓存。

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

## `---@diagnostic` 抑制

支持 `disable-next-line` / `disable-line` / `disable` … `enable`，逗号分隔诊断 code 或通配符 `*`。抑制是最后手段：优先用注解让静态分析真正理解代码。
