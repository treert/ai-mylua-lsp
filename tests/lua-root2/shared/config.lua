-- FEATURE: 被主工作区（lua-root）通过 `require("shared.config")` 引用
--   * return module 风格
--   * 跨工作区 require 解析（indexMode = merged）
--   * hover / goto：main.lua 中对 config.env / config.port 的访问应跳回这里

---@class AppConfig
---@field env "dev" | "prod" | "test"
---@field port integer
---@field debug boolean
local M = {
    env   = "dev",
    port  = 8080,
    debug = true,
}

---@return AppConfig
function M.current()
    return M
end

return M
