-- FEATURE: 泛型相关
--   * @generic T                         — 函数级泛型
--   * @generic T : ConstraintType        — 泛型约束（解析不报错即可）
--   * @class Container<T>                — 泛型 class 定义
--   * 泛型参数在字段/方法返回类型中被替换为实际参数

--- 身份函数：返回值类型应与参数一致
---@generic T
---@param x T
---@return T
local function identity(x)
    return x
end

--- first：返回数组第一个元素（可能为 nil）
---@generic T
---@param xs T[]
---@return T?
local function first(xs)
    return xs[1]
end

--- 泛型容器 class
---@class Stack<T>
---@field items T[]
local Stack = {}
Stack.__index = Stack

---@generic T
---@return Stack<T>
function Stack.new()
    return setmetatable({ items = {} }, Stack)
end

---@generic T
---@param item T
function Stack:push(item)
    self.items[#self.items + 1] = item
end

---@generic T
---@return T?
function Stack:pop()
    local n = #self.items
    local v = self.items[n]
    self.items[n] = nil
    return v
end

---@type Stack<string>
local sstack = Stack.new()
sstack:push("hello")
local top_str = sstack:pop()   -- hover 类型应推断为 string?

---@type Stack<number>
local nstack = Stack.new()
nstack:push(1)
nstack:push(2)
local top_num = nstack:pop()   -- hover 类型应推断为 number?

-- 直接调用 identity
local n = identity(123)        -- T = number
local s = identity("abc")      -- T = string

print(top_str, top_num, n, s, first({ "a", "b" }))
