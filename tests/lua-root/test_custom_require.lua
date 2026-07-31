
---
-- 自定义 require 函数
---@customrequire param=module_name mgr_abc  module_abc
---@param module_name string 模块名称
---@return any
function utils.custom_require(module_name)
end


local a = utils.custom_require("mgr_abc.abc_mgr")

a.test_print(a.version)
