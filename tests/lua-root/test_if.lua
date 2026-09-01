
-- 这儿的 gg_cpp_define_some 假设是 C++ 里可能定义注册进入 lua 全局表的。
-- 虽然找不到 gg_cpp_define_some 的定义，但是这是再 if 里做逻辑判断，判断通过后，then 的语句块里，gg_cpp_define_some 就有定义了。
-- 这种情况下 then 分支 gg_cpp_define_some 的类型可能用 any 来表示，或者用一个特殊的类型来表示 "可能存在的全局变量"。
if gg_cpp_define_some then
    print(gg_cpp_define_some)   -- 
end

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