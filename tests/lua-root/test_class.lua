---@class XMod.ClassX1
---@field public name string
---@field public age number
---@field parent XMod.ClassX1

---@type XMod.ClassX1
XMod.ClassX1 = {}


XMod = {
    lala = 1,
}

local xx = XMod.ClassX1.parent.name


---@class PartClass
---@field public name string
---@field public age number
local PartClass = {}

function PartClass:get_name()
    return self.name
end

---@type PartClass
local part = PartClass:New()

part.name = "123"
part.age = 123


---@class TestCls1
TestCls1 = {}

local xx = nil ---@type TestCls1
local yy = xx -- just write TestCls1 name here
