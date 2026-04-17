# `tests/lua-root` — 主工作区（手工端到端测试）

此目录与 [`../lua-root2`](../lua-root2) 一起被 [`../mylua-tests.code-workspace`](../mylua-tests.code-workspace) 作为**两个 workspace folder** 同时挂载，用于在 Extension Development Host 中人工验证 LSP 行为。

启动方式（Windows）：运行 `.cursor/scripts/test-extension.ps1`；其他平台见 Skill `.cursor/skills/test-extension/`。

## 文件覆盖矩阵

| 文件 | 覆盖的 LSP 特性 |
|------|-----------------|
| [`main.lua`](main.lua) | 入口文件；require 跳转、module return 类型、跨 workspace require、跨文件全局调用、completion 测试点（`math_utils.` / `hero:`）|
| [`math_utils.lua`](math_utils.lua) | `return M` 模块风格、`@overload` / `@vararg` / `@deprecated` / `@async` / `@nodiscard`、复杂类型（union / optional / array / fun() / table shape / 泛型）|
| [`emmy_basics.lua`](emmy_basics.lua) | `@class` / `@field`、`@alias`（字面量 alias + union 字符串 alias）、`@enum`、`@type` 声明与推断 |
| [`emmy_types.lua`](emmy_types.lua) | EmmyLua 类型表达式全覆盖：`number \| string`、`string?`、`T[]`、`Array<T>`、`fun(x): y`、`{k:v}`、括号分组 |
| [`player.lua`](player.lua) | OOP：`@class A: B,C` 多继承、self 方法、字段；全局 `Player` 在 `main.lua` 被跨文件使用 |
| [`scopes.lua`](scopes.lua) | 作用域树全部 block 类型：`do` / `while` / `repeat` / `if` / 数值 `for` / 泛型 `for`、参数、vararg、隐式 self、`local x = x + 1` Lua 语义、closure |
| [`generics.lua`](generics.lua) | `@generic T`（函数级）、`@class C<T>`（容器）、泛型参数替换 |
| [`diagnostics.lua`](diagnostics.lua) | 预期诊断一览（每行 `-- !diag:` 标注）：`undefinedGlobal` / `emmyTypeMismatch` / `emmyUnknownField` / `luaFieldError`（closed）/ `luaFieldWarning`（open）/ 语法错误 |
| [`refs_rename.lua`](refs_rename.lua) | references / rename：多处引用的 local、跨文件全局函数 `greet`、Service 方法多处调用；semantic tokens（内置库标为 defaultLibrary）|
| [`json.lua`](json.lua) | 真实第三方库（json4lua）解析健壮性 |
| [`UEAnnotation/`](UEAnnotation/) | UE4 风格场景：多继承、UE4 全局表（`UE4.UMiscSystemLibrary`）、`---@type` 类型重写、跨文件 class stub 重写 |

## 建议的操作路径

1. 先打开 `main.lua`，把它作为浏览起点：
   - hover 任意 `math_utils.xxx`、`hero:xxx` 查看类型信息。
   - 在 `math_utils.` / `hero:` 后输入 `.` / `:` 触发 completion。
   - 对 `require("player")` 用 <kbd>F12</kbd> 跳转。
2. 打开 `diagnostics.lua`，按文件头的清单核对每行诊断是否按预期出现。
3. 在 `refs_rename.lua` 中右键 `counter` / `greet` 测 Find References 与 Rename Symbol。
4. 打开 `emmy_types.lua` 观察复杂类型表达式的 hover 展示。
5. 使用 <kbd>Ctrl+T</kbd> 搜索 `Player` / `Vector2` / `Direction` / `AppName` 验证 workspace/symbol。
