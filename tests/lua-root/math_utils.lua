-- FEATURE:
--   * 模块 `return M` 风格（测试 module_return_type + require 跨文件解析）
--   * @param / @return / @vararg / @overload / @deprecated / @async / @nodiscard
--   * 复杂类型：union `|`、optional `?`、array `T[]`、fun() 类型、泛型 `<T>`、table 形状 `{k:v}`

local M = {}

--- 返回两数之和
---@param a number
---@param b number
---@return number
function M.add(a, b)
    return a + b
end

--- 多种签名的乘法：支持 number 或 number[] 求积
---@overload fun(values: number[]): number
---@param a number
---@param b number
---@return number
function M.mul(a, b)
    if type(a) == "table" then
        local r = 1
        for _, v in ipairs(a) do r = r * v end
        return r
    end
    return a * b
end

--- 求和，可变参数
---@vararg number
---@return number
function M.sum(...)
    local total = 0
    for _, v in ipairs({ ... }) do
        total = total + v
    end
    return total
end

--- 将字符串或数字归一化为数字
---@param v number|string     -- union 类型
---@return number?            -- optional 返回
function M.to_number(v)
    if type(v) == "number" then return v end
    return tonumber(v)
end

--- 映射一个数组
---@generic T, U
---@param arr T[]
---@param fn fun(item: T, index: integer): U
---@return U[]
function M.map(arr, fn)
    local out = {}
    for i, v in ipairs(arr) do
        out[i] = fn(v, i)
    end
    return out
end

--- 构造一个二维点
---@param x number
---@param y number
---@return { x: number, y: number }    -- table shape 类型
function M.point(x, y)
    return { x = x, y = y }
end

--- 已废弃的旧 api，请改用 M.add
---@deprecated
---@param a number
---@param b number
---@return number
function M.legacy_add(a, b)
    return a + b
end

--- 异步加载（示意）
---@async
---@param url string
---@return string
function M.fetch(url)
    return "payload:" .. url
end

--- 返回值不应丢弃
---@nodiscard
---@return integer
function M.unique_id()
    return os.time()
end

return M
