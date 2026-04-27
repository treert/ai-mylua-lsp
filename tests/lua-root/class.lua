
---@class BaseCls
---@field __class_name string @ 类名
local BaseCls = {__class_name = "baseCls",super = nil}
BaseCls.__index = BaseCls

function BaseCls:ctor()

end

---@generic T
---@param self T
---@return T
function BaseCls:new()
    local o = {}
    setmetatable(o, self)
    o:ctor()
    return o
end

function class(class_name,super)
    local cls = {
        __class_name = class_name;
        super = super or BaseCls;
    }
    cls.__index = cls
    setmetatable(cls, super) -- 这里设置元表为父类，实现继承
    return cls
end
 
