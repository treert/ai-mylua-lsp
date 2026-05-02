-- local function creat_module()
local creat_module = function ()
    local m = {}
    function m.hi()
        print("hi")
    end
    return m
end

local mm = creat_module()
mm.hi()

return creat_module()