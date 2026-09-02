
-- ============================================================================
-- 测试说明
-- 本文件专门用于验证【诊断抑制】能力，核心场景是「判空逻辑」。
--
-- 即：在 if / and / or 等条件表达式里对一个可能未定义的全局变量做是否为 nil 的
-- 判断，一旦判断通过（变量非 nil），则该分支内变量应被视为「存在」，从而抑制
-- Undefined global 之类的诊断，不再报错。
--
-- 实现策略：低成本，尽量抑制。
--   1. 只做结构性、语法层面的简单判断，不做复杂的流分析，也不做完整类型推断。
--   2. 宁可少报（漏报真实错误），也不要因为拿不准而误报 —— 尽量抑制。
--
-- 本文件中的 gg_cpp_define_some / jit 等均为手写用例中「假定存在」的全局变量，
-- 用于验证抑制逻辑，不代表真实代码里一定存在定义。
-- ============================================================================

-- 这儿的 gg_cpp_define_some 假设是 C++ 里可能定义注册进入 lua 全局表的。
-- 虽然找不到 gg_cpp_define_some 的定义，但是这是再 if 里做逻辑判断，判断通过后，then 的语句块里，gg_cpp_define_some 就有定义了。
-- 这种情况下 then 分支 gg_cpp_define_some 的类型可能用 any 来表示，或者用一个特殊的类型来表示 "可能存在的全局变量"。
if gg_cpp_define_some then
    print(gg_cpp_define_some)
    gg_cpp_define_some.some_func()
end

gg_cpp_define_some.some_func() -- 预期诊断 Undefined global 'gg_cpp_define_some'

-- 与上面其实一样
if gg_cpp_define_some ~= nil then
    print(gg_cpp_define_some)
end

-- 在 else 分支里，gg_cpp_define_some 也有值，类型可以当作 any。
if gg_cpp_define_some == nil then
else
    print(gg_cpp_define_some)
end

-- 这种写法也很常见，if 分支里没有定义，else 分支里有定义。
if not gg_cpp_define_some then
else
    print(gg_cpp_define_some)
end

-- 这种 and 写法也非常常见。
-- 和 if 类似 gg_cpp_define_some 在 and 的分支里有定义，可以当作 any。
-- 其实可以推断 xx 的类型是 string，因为 or 的分支返回了 string。
local xx = gg_cpp_define_some and gg_cpp_define_some.some_func() or ""
print(xx)

---@class SomeClsForIf

---@type SomeClsForIf
local x = {}

if x.m_some then
    print(x.m_some)
end


if jit and jit.version then
    print(jit.version) -- 预期：不报 Undefined global 'jit'
end

local sock = lua_extension and lua_extension.luasocket and lua_extension.luasocket().tcp();-- 预期：不报 Undefined global 'lua_extension'
local ttt = lua_extension -- 预期：报 Undefined global 'lua_extension'
local ttt = lua_extension and lua_extension.luasocket -- 预期：不报 Undefined global 'lua_extension'