-- FEATURE: 作用域树 (scope.rs) 测试 —— 所有块级作用域都应正确建树
--   * do / while / repeat / if / for(numeric) / for(generic) 块
--   * 函数参数 / vararg / for 变量 / 隐式 self
--   * 嵌套遮蔽（inner local 遮蔽 outer）
--   * `local x = x + 1` 的 RHS 引用外层 x 的 Lua 语义
--   * closure 捕获
--   * references：不同作用域下相同名字的 local 应被正确区分

local x = 10

-- do 块作用域
do
    local x = x + 1     -- RHS x 指向外层（值 10），LHS 新建 local x（值 11）
    print("do.x =", x)
end

-- while 块作用域
local i = 0
while i < 3 do
    local step = 1
    i = i + step
end

-- repeat-until：until 条件可访问 repeat 体内定义的 local
repeat
    local finished = i >= 3
until finished

-- if / elseif / else 块作用域
if i > 0 then
    local branch = "positive"
    print(branch)
elseif i < 0 then
    local branch = "negative"
    print(branch)
else
    local branch = "zero"
    print(branch)
end

-- 数值 for
for n = 1, 5, 1 do
    local squared = n * n
    print(n, squared)
end

-- 泛型 for
local fruits = { "apple", "pear", "peach" }
for idx, name in ipairs(fruits) do
    print(idx, name)
end

-- 嵌套函数 + closure
local function counter()
    local count = 0
    return function()
        count = count + 1
        return count
    end
end

local tick = counter()
print(tick(), tick(), tick())

-- 方法形式隐式 self 参数
local obj = { value = 42 }
function obj:inspect()
    return self.value
end
print(obj:inspect())

-- 多重 return + 多值赋值
local function pair()
    return 1, 2
end
local a, b = pair()
print(a, b)

-- vararg
local function joined(...)
    local t = { ... }
    return table.concat(t, ",")
end
print(joined("a", "b", "c"))

return {
    counter = counter,
    joined = joined,
}
