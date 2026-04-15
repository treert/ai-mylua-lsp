---@class ABC123
---@field a integer
local ABC = {}

---@type ABC123
local x = 1
x.a = 1
x.no_exist = 2

self = 1

function ABC:f1()
    self.xxxx = print(1)
    -- local x = self.y1
    -- self.yy = self.x
    -- self:f1()
    -- self.f2()
    local x = self.xxxx
    return x
end

function ABC:f2()
    return 4
end

-- ABC.g1 = 5

function A1213:f()
    self.ff = 2
end

---@type T3
local ttt = nil


-- local ttt1 = ttt:get_t1()

-- local _ = ttt1:f3()
---@type ABC
local ttt1 = UE4.UMiscSystemLibrary.GetAbilitySystemComponentFromActor(Character)

local _ = ttt1:f333()
