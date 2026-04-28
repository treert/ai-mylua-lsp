print("hello world")

local function test()
    local a1 = utils.get_a1()
    a1:Say()
    return a1:test()
end

local a1_test = test()

local function test2()
    local a1 = utils.get_a1_indirect()
    a1:Say()
    return a1:test()
end

local a1_test2 = test2()

local function test3()
    local a1 = utils.get_a1_indirect()
    return a1:get_a2()
end

---@type ClassA2
local a1_test3 = test3()
a1_test3:Say1()