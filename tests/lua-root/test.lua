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

local XX = UE4.Class()

function encodeString(s)
    local s = tostring(s)
    return s:gsub(".", function(c) return escapeList[c] end)
end

print(utils.test_const.A, utils.test_const.B, utils.test_const.C)
