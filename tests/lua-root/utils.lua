--- utils define
utils = {}

---@return ClassA1
function utils.get_a1()
    return ClassA1:new()
end

function utils.get_a1_indirect()
    return utils.get_a1()
end

function utils.get_a2()
    return ClassA2:new()
end

function utils:empty_func(arg1, arg2) end

--- 0000
-- utils.test_const = {}

--- 1111
utils.test_const = require('test_const')

--- 2222
-- utils.test_const = {}