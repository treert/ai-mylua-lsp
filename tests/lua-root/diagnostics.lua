-- FEATURE: 预期诊断触发清单
--   每行 `-- !diag:` 标注这一行应该触发的诊断分类。
--
--   受以下配置控制（见 mylua-tests.code-workspace）：
--     diagnostics.undefinedGlobal    = warning
--     diagnostics.emmyTypeMismatch   = error
--     diagnostics.emmyUnknownField   = error
--     diagnostics.luaFieldError      = error    (closed shape)
--     diagnostics.luaFieldWarning    = warning  (open shape)

---@class Point
---@field x number
---@field y number
local Point = {}

---@type Point
local p = { x = 1, y = 2 }

local _ = p.x
local _ = p.no_such_field                      -- !diag: emmyUnknownField (error)

-- Closed shape：字面量里写全了字段，读取未声明字段报 error
local closed = { a = 1, b = 2 }
local _ = closed.a
local _ = closed.c                             -- !diag: luaFieldError (error)

-- Open shape：使用了动态 bracket key，读取未知字段降为 warning
local open = { a = 1, b = 2 }
open["dyn"] = 3                                -- 触发 open shape
local _ = open.ghost                           -- !diag: luaFieldWarning (warning)

-- 未定义全局
do_something_undefined()                       -- !diag: undefinedGlobal (warning)

-- EmmyLua 声明类型与字面量赋值冲突
---@type number
local n = "not a number"                       -- !diag: emmyTypeMismatch (error)

-- 语法错误（tree-sitter MISSING/ERROR）
local broken = function(                       -- !diag: syntax error（缺少 `)` 与函数体）
;
print(p, closed, open, n, broken)
