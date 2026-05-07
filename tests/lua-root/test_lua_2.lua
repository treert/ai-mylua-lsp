print("hello world")

local function test()
    local a1 = utils.get_a1()
    return a1:test()
end
local a1_test = test()

local function test2()
    return utils.get_a1():test()
end
local a1_test2 = test2()

local utils2 = utils
local function test2_1()
    return utils2.get_a1_indirect():get_a2():test()
end
local a1_test2_1 = test2_1()

-- test table
local tt = {
    a = 1,
}
function tt:get_tt_a()
    return self.a
end

local function test_tt_1()
    return tt:get_tt_a()
end

local a1_test_tt_1 = test_tt_1()

local function test2_2()
    return tt.a
end

function test_g()
    return tt
end

local a1_test2_2 = test2_2()




local function test3()
    local a1 = utils.get_a1_indirect()
    return a1:get_a2()
end

---@type ClassA2
local a1_test3 = test3()
a1_test3:Say()