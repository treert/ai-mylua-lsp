---@class PartClass
---@field public id number
local PartClass = {}

function PartClass:getId()
    return self.id
end

---@generic T
---@param self T
---@return T
function PartClass:New() end