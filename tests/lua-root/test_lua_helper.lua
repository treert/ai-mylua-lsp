-- 使用
---@class ClassA1:BaseCls
ClassA1 = class("ClassA1")
function ClassA1:Say()
    print(self.__class_name .. " say")
end

---@return string
function ClassA1:test()
    return "test"
end

function ClassA1:get_a2()
    return ClassA2:new()
end

---@class ClassA2:ClassA1
ClassA2 = class("ClassA2",ClassA1)
 
local a1 = ClassA1:new()
local a2 = ClassA2:new()
a1:Say() -- ClassA1 say
a2:Say() -- ClassA2 say

---@class ClassB1:BaseCls
local ClassB1 = class("ClassB1")

ClassB1.s_bbb = 123

--- bbb
---@param bb number
function ClassB1:bbb(bb)
    self.m_bbb = bb
    print(self.__class_name .. " bbb")
end

function ClassB1:test_bbb()
    self:bbb(456)
    print(self.__class_name .. self.m_bbb, self.s_bbb)
end

local tt_tail = {} ---@type ClassA2
tt_tail:Say()
