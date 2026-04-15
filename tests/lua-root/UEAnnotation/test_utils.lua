UE = {}


---@class T1
---@field t1 number
T1 = {}

---@return number
function T1:f1() end

---@class T2
---@field t2 number
T2 = {}

---@return T1
function T2:get_t1() end

---@class T3: T1,T2
---@field t3 string
T3 = {}

--[[
UMiscSystemLibrary_ tips
]]
---@class UMiscSystemLibrary_:UMiscSystemLibrary
UMiscSystemLibrary_ = {}

--- 返回 技能节点
---@param Actor AActor
---@return UAbilitySystemComponent
function UMiscSystemLibrary_.GetAbilitySystemComponentFromActor(Actor) end

UE4 = {}

--- redefine UMiscSystemLibrary
---@type UMiscSystemLibrary_
UE4.UMiscSystemLibrary = nil


