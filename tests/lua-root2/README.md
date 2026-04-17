# `tests/lua-root2` — 第二工作区（跨 workspace 场景）

本目录与 [`../lua-root`](../lua-root) 一起被 [`../mylua-tests.code-workspace`](../mylua-tests.code-workspace) 同时挂载，专门用于验证 **多 workspace folder + `indexMode = merged`** 下的行为：

* 跨 workspace `require`
* 跨 workspace 全局定义 / goto definition / workspace symbol
* 跨 workspace 诊断（未定义全局在另一 root 定义后应消失）

## 文件

| 文件 | 说明 |
|------|------|
| [`shared/config.lua`](shared/config.lua) | 在主工作区通过 `require("shared.config")` 引用，`@class AppConfig` + `return M` |
| [`shared/logger.lua`](shared/logger.lua) | 在主工作区通过 `require("shared.logger")` 引用，提供 `info` / `debug` / `warn` / `error`，含 `@overload` 示例 |
| [`cross_globals.lua`](cross_globals.lua) | 仅挂全局符号：`AppName` / `AppVersion` / `Audit`；在主工作区 `main.lua` 中使用，测试跨 workspace 全局 goto / hover |

## 验证点

* 在 `lua-root/main.lua` 里对 `config.env` / `logger.info` 用 <kbd>F12</kbd> 应跳进本目录。
* 在 `lua-root/main.lua` 里对 `AppName` 用 <kbd>F12</kbd> 应跳到 `cross_globals.lua`。
* <kbd>Ctrl+T</kbd> 搜索 `Audit` / `AppConfig` / `Logger` 应在本目录命中。
