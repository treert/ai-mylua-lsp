-- FEATURE: 跨 workspace 全局贡献
--   * 不使用 `return M`，纯粹给全局作用域挂符号
--   * 主工作区的 main.lua 会直接 `print(AppName)`，测试：
--       - workspace/symbol 应能搜到 AppName / Audit
--       - main.lua 中 `AppName` 的 goto definition 应跳到本文件
--       - hover AppName 应显示注释

--- 应用名（全局）
---@type string
AppName = "mylua-demo"

---@type integer
AppVersion = 1

--- 审计 helper：全局 class
---@class Audit
---@field enabled boolean @ 是否启用审计 11
Audit = { 
    enabled = true -- 是否启用审计 22
}

---@param action string
function Audit.log(action)
    if Audit.enabled then
        print("[audit] " .. action)
    end
end
