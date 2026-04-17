-- FEATURE: 被主工作区 require("shared.logger")
--   * 模块内部 @class + 方法
--   * @overload：info 支持 string 或 (fmt, ...) 两种形态
--   * @vararg

---@class Logger
---@field prefix string
local Logger = {}
Logger.__index = Logger

---@param prefix string
---@return Logger
function Logger.new(prefix)
    return setmetatable({ prefix = prefix or "app" }, Logger)
end

---@param msg string
local function write(msg)
    io.write(msg, "\n")
end

local default = Logger.new("app")

local M = {}

--- 信息日志（支持 string 或 printf 风格）
---@overload fun(fmt: string, ...: any): nil
---@param msg string
function M.info(msg, ...)
    if select("#", ...) > 0 then
        write("[INFO] " .. string.format(msg, ...))
    else
        write("[INFO] " .. tostring(msg))
    end
end

---@param msg string
function M.debug(msg)
    write("[DEBUG] " .. tostring(msg))
end

---@param msg string
function M.warn(msg)
    write("[WARN] " .. tostring(msg))
end

---@param msg string
function M.error(msg)
    write("[ERROR] " .. tostring(msg))
end

--- 返回默认 logger 实例
---@return Logger
function M.default()
    return default
end

return M
