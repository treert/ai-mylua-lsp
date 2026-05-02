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


---@return boolean, number?
local function test_ret1()
    return false
end

mm = require("test_create_module")
-- local mm = require("test_create_module")
mm.hi()
