-- FEATURE:
--   * @class / @field（闭合 shape 与字段声明）
--   * @alias（别名展开）
--   * @enum（枚举成员作为类型成员）
--   * @type 变量声明与推断
--   * goto definition / hover：点击类型名或字段跳到声明
--   * workspace/symbol：class/alias/enum 均应出现

---@class Vector2
---@field x number
---@field y number
local Vector2 = {}

--- 计算模长
---@return number
function Vector2:length()
    return math.sqrt(self.x * self.x + self.y * self.y)
end

--- 加法
---@param other Vector2
---@return Vector2
function Vector2:add(other)
    return { x = self.x + other.x, y = self.y + other.y }
end

--- 二维向量的别名形式（等价写法）
---@alias Vec2 { x: number, y: number }

--- 支持的事件名别名
---@alias EventName "click" | "hover" | "leave"

--- 方向枚举
---@enum Direction
local Direction = {
    Up    = 0,
    Down  = 1,
    Left  = 2,
    Right = 3,
}

---@type Vector2
local origin = { x = 0, y = 0 }

---@type Vec2
local p = { x = 1, y = 2 }

---@type EventName
local ev = "click"

---@type Direction
local d = Direction.Up

print(origin:length(), p.x, ev, d)

return {
    Vector2 = Vector2,
    Direction = Direction,
}
