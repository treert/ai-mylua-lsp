
---@class Test.TypeA
---@field m_a number = 2 -- 没有匹配的右括号 ( 
---@field m_b number = 3 -- 
local TypeA = class("TypeA")


---@class Test.TypeB
---@field m_bb number
local TypeB = {}

print(TypeB.m_bb)

---@class Test.TypeC : BaseCls
---@field m_cc number
local TypeC = class("TypeC")

print(TypeC.m_cc)
print(TypeC.__class_name)
