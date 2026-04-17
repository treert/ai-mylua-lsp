-- FEATURE: 覆盖 EmmyLua 类型表达式解析的各种形态
--   * union          number | string
--   * optional       string?
--   * array          number[]
--   * generic        Array<number>
--   * fun()          fun(x: number): string
--   * table shape    { name: string, age: integer }
--   * 括号分组       (number | string)[]

--- union + optional 组合
---@param v number | string | nil
---@return string?
local function show(v)
    if v == nil then return nil end
    return tostring(v)
end

--- 数组 + 泛型
---@param xs number[]
---@return number[]
local function copy_arr(xs)
    local r = {}
    for i, v in ipairs(xs) do r[i] = v end
    return r
end

--- 回调类型
---@param action fun(ok: boolean, msg: string): nil
local function on_done(action)
    action(true, "ok")
end

--- 返回 table shape
---@return { name: string, age: integer, tags: string[] }
local function profile()
    return { name = "lua", age = 30, tags = { "lang", "script" } }
end

--- 括号分组 + 数组
---@param xs (number | string)[]
local function mixed(xs)
    for _, v in ipairs(xs) do print(v) end
end

--- 泛型容器
---@class Array<T>
---@field items T[]
local Array = {}

---@generic T
---@param item T
function Array:push(item)
    table.insert(self.items, item)
end

---@generic T
---@return T?
function Array:pop()
    return table.remove(self.items)
end

---@type Array<number>
local nums = { items = { 1, 2, 3 } }
nums:push(4)
local top = nums:pop()

show(123)
show("hi")
copy_arr({ 1, 2, 3 })
on_done(function(ok, msg) print(ok, msg) end)
mixed({ 1, "two", 3 })
print(profile().name, top)
