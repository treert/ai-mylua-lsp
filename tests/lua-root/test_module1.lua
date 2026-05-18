---@class TestModule1
local _M = {}

_M.Config_Id = 123

function _M.test()
    print("test_module.test")
end

_M.internat = {}

_M.internat.Config_Internat_Id = 456

function _M.internat.test_internat()
    print("test_module.internat.test")
end


return _M