-- ---------------------------------------------------------------------------
-- 取舍说明：跨文件给表加字段，LSP 不予支持（刻意为之，不是待修的 bug）
-- ---------------------------------------------------------------------------
--
-- 现象：下面第 3 行的写入不报错，第 7 行的读取却报 Unknown field。
--
-- 原因分两层：
--   1. 写入侧被诊断显式跳过 —— Lua 里 `t.x = v` 本就是「新建键」的合法语义，
--      `local M = {}` 之后逐行 `M.foo = ...` 是最主流的模块写法，对写入报警会
--      让这类代码全行飘红。见 diagnostics/field_access.rs 的 is_assignment_target 早退。
--   2. 这次写入确实没落进 TableShape —— t3 的 shape 属于 test_table.lua，
--      而 summary 构建是每文件独立、rayon 并行、全程无锁的（见 index-architecture.md
--      的 Parse 阶段），TableShapeId 也只在文件内唯一。跨文件既无法寻址，也不允许
--      共享可变状态；何况单文件重新解析会整体替换 summary，外部写入必然被抹掉。
--
-- 于是「同文件 shape 可持续增长、跨文件不可」并非一条显式规则，而是
-- 「shape 只归它所属文件的 summary 构建过程所有」这一所有权模型的自然结果。
--
-- 曾评估过的方案：把跨文件的 TableShape 当作原型，本文件派生一个新 shape 承接
-- 动态字段，读取时 miss 再回溯原型。技术上可行（原型存 TypeFact 而非 shape id，
-- 可绕开并行与 per-file id 的约束），但会把 t3 的类型身份从「create_table 的返回值」
-- 替换成一个本地派生壳，require 模块等依赖原始身份的能力都要跟着穿透处理。
--
-- 最终结论：不做。跨文件往别人的表上挂字段本身就不是值得鼓励的 Lua 写法，
-- 为它增加类型系统复杂度得不偿失。需要扩展字段时，请选择：
--   * 在表的定义处（本例是 test_table.lua 的 create_table）补上该字段；
--   * 或用 ---@class 显式声明契约，走 EmmyLua 路径；
--   * 确有必要时，就地抑制：---@diagnostic disable-next-line: unknown-field

local t3 = create_table()

t3.another_id = 123   -- TableShape 跨文件 close，这一行并不能修改 TableShape

print(t3.name)
print(t3.extra_id)
print(t3.another_id)  -- 这儿会警告 Unknown field 'another_id' on table