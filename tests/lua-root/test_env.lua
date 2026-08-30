local print = print

g1 = 123
print(g1)

local xx = _ENV
local x2 = _G._ENV

print(gg_undefined)
print(_G.gg_undefined)
print(_G.g1)

_ENV = {}
print(g1) -- 应该有诊断警告

g1 = 321
print(g1)
print(_G.g1)

g2 = g1 + 1000
print(g2)