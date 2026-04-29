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
