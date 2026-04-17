-- FEATURE: 入口文件，串联所有测试模块
--   * require 跳转：点击模块名应跳到对应文件
--   * require 返回 module_return_type：对 math_utils 的字段调用应能 hover 出类型
--   * 跨文件全局 goto：Player / greet 等定义在其它文件，应能跳转
--   * 跨 workspace require：`shared.config` / `shared.logger` 来自 lua-root2
--   * completion 测试点：用 `<完成此处>` 注释标注期望的补全触发位置

-- ─── 同工作区 require ───────────────────────────────────────────
local math_utils = require("math_utils")
local basics     = require("emmy_basics")
local Player     = require("player")

-- ─── 跨工作区 require（来自 lua-root2） ─────────────────────────
local config = require("shared.config")
local logger = require("shared.logger")

-- 仅为了让解析器加载这些文件（它们可能会定义全局、贡献类型或触发诊断）
require("emmy_types")
require("scopes")
require("generics")
require("refs_rename")

-- ─── 使用 math_utils（module return type 推断） ─────────────────
local sum     = math_utils.add(1, 2)          -- hover sum 应为 number
local product = math_utils.mul({ 2, 3, 4 })   -- overload：传数组
local maybe   = math_utils.to_number("42")    -- 返回 number?
local total   = math_utils.sum(1, 2, 3, 4)    -- vararg
local doubled = math_utils.map({ 1, 2, 3 }, function(v) return v * 2 end)
print(sum, product, maybe, total, doubled[1])

-- math_utils.          -- <completion 测试点：应列出 add/mul/sum/to_number/map/point/legacy_add/fetch/unique_id>

-- ─── 使用 basics 模块中导出的类型 ──────────────────────────────
local v = basics.Vector2
print(v)

-- ─── OOP：Player 类（定义在 player.lua） ───────────────────────
local hero = Player.new(1, "Alice")
hero:pick_up("sword")
hero:take_damage(5)    -- 来自 Damageable 继承链
local lvl = hero:level_up()
print(hero:describe(), lvl)

-- hero:           -- <completion 测试点：应包含 Player / Entity / Damageable 全部方法>

-- ─── 跨文件全局调用（greet 定义在 refs_rename.lua） ────────────
print(greet("world"))
print(greet("lua"))

-- ─── 跨 workspace 模块 ─────────────────────────────────────────
logger.info("app started, env = " .. config.env)
logger.debug("port = " .. tostring(config.port))

-- ─── 全局多候选测试（AppName 在 cross_globals.lua 中定义） ─────
print(AppName)

return {
    hero = hero,
}
