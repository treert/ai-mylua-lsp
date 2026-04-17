-- FEATURE: 面向对象 + 继承 + self 推断
--   * @class A: B,C 多继承链
--   * self:method() 隐式 self 参数
--   * 跨文件类型访问（在 main.lua 中会使用 Player）
--   * hover 应展示 class 注释 + 字段列表
--   * completion：在 `p:` 之后应列出继承链上的方法

---@class Entity
---@field id integer
---@field name string
local Entity = {}

--- 构造一个 Entity
---@param id integer
---@param name string
---@return Entity
function Entity.new(id, name)
    return setmetatable({ id = id, name = name }, { __index = Entity })
end

---@return string
function Entity:describe()
    return "entity#" .. self.id .. ":" .. self.name
end

---@class Damageable
---@field hp integer
local Damageable = {}

---@param dmg integer
function Damageable:take_damage(dmg)
    self.hp = self.hp - dmg
end

--- 玩家同时继承 Entity 和 Damageable
---@class Player: Entity, Damageable
---@field level integer
---@field inventory string[]
Player = {}    -- 故意导出为全局，测试 workspace/symbol 和跨文件 goto

---@param id integer
---@param name string
---@return Player
function Player.new(id, name)
    local self = setmetatable({}, { __index = Player })
    self.id = id
    self.name = name
    self.hp = 100
    self.level = 1
    self.inventory = {}
    return self
end

---@param item string
function Player:pick_up(item)
    table.insert(self.inventory, item)
end

---@return integer
function Player:level_up()
    self.level = self.level + 1
    self.hp = self.hp + 10
    return self.level
end

return Player
