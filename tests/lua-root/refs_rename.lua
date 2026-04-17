-- FEATURE: references / rename / semantic tokens
--   * 同一 local 在多处被引用（测 references）
--   * 全局函数在 main.lua 中被调用（跨文件 references）
--   * rename 局部变量只改当前文件；rename 全局应改全工作区
--   * semantic tokens：string/table/math 等内置库函数应被标为 defaultLibrary

-- 局部变量：多处引用
local counter = 0
counter = counter + 1
counter = counter + 1
counter = counter + 1
print("counter =", counter)

-- 全局函数（会在 main.lua 中被调用 → 跨文件 references）
---@param name string
---@return string
function greet(name)
    return "hello, " .. name
end

---@class Service
---@field name string
local Service = {}

---@return string
function Service:name_of()
    return self.name
end

-- 方法多处调用
local svc = { name = "auth" }
setmetatable(svc, { __index = Service })
print(svc:name_of())
print(svc:name_of())

-- 内置库（semantic tokens: defaultLibrary）
local s = string.upper("hello")
local t = table.concat({ "a", "b" }, ", ")
local m = math.max(1, 2, 3)
local n = tostring(123)
print(s, t, m, n)
