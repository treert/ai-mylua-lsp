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
print(utils.test_const.A, utils.test_const.B, utils.test_const.C)

utils.test_const_str_map = require('test_const_str_map')

print(utils.test_const_str_map.A3)

--- 2222
-- utils.test_const = {}

---@class MiscManager
---@field m_misc_id number
---@field miscFunc fun():number

local mgrs = {
    --- 222 head
    ---@type MiscManager @ 222 mid
    MiscMgr2 = nil, -- 222 tail
    MiscMgr3 = nil,---@type MiscManager @ 333 tail
}

local _ = mgrs.MiscMgr2.m_misc_id
local _ = mgrs.MiscMgr2:miscFunc()

local _ = mgrs.MiscMgr3.m_misc_id
local _ = mgrs.MiscMgr3:miscFunc()


utils.mgrs = {
    --- 444 head
    ---@type MiscManager @ 444 mid
    MiscMgr4 = nil, -- 444 tail
    MiscMgr5 = nil,---@type MiscManager @ 555 tail
}

local _ = utils.mgrs.MiscMgr4.m_misc_id
local _ = utils.mgrs.MiscMgr4:miscFunc()



---@class UtilsLocals
---@field MiscManager MiscManager


---@type UtilsLocals
utils.locals = {}





local MiscManager = utils.locals.MiscManager

local ret1 = MiscManager.m_misc_id
local ret2 = MiscManager:miscFunc(MiscManager.m_misc_id)
