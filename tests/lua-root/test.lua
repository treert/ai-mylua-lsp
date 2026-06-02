print('hello world')

json = require('json')
print("json", json.decode('{"a":1,"b":2}'))

---@class MyClass
my_m = {}

function my_m:hello_b()
    print("hello")
end

local function my_Print(self, ...)
    print("my_print", ...)
end

local function my_Print(self, ...)
    print("my_print")
end

function encodeString(s)
    local s = tostring(s)
    return s:gsub(".", function(c) return c end)
end

print(utils.test_const.A, utils.test_const.B, utils.test_const.C)

if utils.test_const.ON_Evt_LALA then
    print(utils.test_const.ON_Evt_HAHA)
end

print(utils.test_const_str_map.A3)

---@return boolean, number?
local function test_ret1()
    return false
end

mm = require("test_create_module")
-- local mm = require("test_create_module")
mm.hi()

local tt = test_g()
print(tt.a)

local tt = ClassA1 and ClassA1:new()
local tt = ClassA1 and ClassA1:new() or nil
local tt = utils.mgrs and utils.mgrs.MiscMgr4 and utils.mgrs.MiscMgr4:miscFunc()
local tt = utils.mgrs and utils.mgrs.MiscMgr4 and utils.mgrs.MiscMgr4:miscFunc() or 0

local module1 = require('test_module1')

module1.test()
print(module1.Config_Id)
print(module1.internat)
module1.internat.test_internat()
print(module1.internat.Config_Internat_Id)


local _ = utils.mgrs.MiscMgr4.m_misc_id
local _ = utils.mgrs.MiscMgr4:miscFunc()

---@type PartClass
local PartClass

local part = PartClass:New()
part.name = "123"
part.age = 123
part.id = 123
