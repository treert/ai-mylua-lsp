--[[
Misc System Library
]]
---@class UMiscSystemLibrary: UBlueprintFunctionLibrary
UMiscSystemLibrary = {}

--- first define
---@type UMiscSystemLibrary
UE4.UMiscSystemLibrary = nil

--[[
Get Ability System Component from Actor
]]
---@param Actor AActor
---@return UAbilitySystemComponent
function UMiscSystemLibrary.GetAbilitySystemComponentFromActor(Actor) end


---@enum ERangeBoundTypes
UE4.ERangeBoundTypes = {
  Exclusive = 0, -- The range excludes the bound.
  Inclusive = 1, -- The range includes the bound.
  Open = 2, -- The bound is open.
}


local x = UE4.ERangeBoundTypes.Inclusive