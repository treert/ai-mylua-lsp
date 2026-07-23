-- define a global variable gg
_G.gg = {}


-- gg.a1 = 1
gg.a1 = 1


local mm = _G.gg

-- mm.m1 = 11
mm.m1 = 11

print(mm.a1)
print(mm.m1)

local mm_1 = gg

mm_1.mm1 = 111

print(mm_1.mm1)
print(mm_1.m1)
print(mm_1.a1)

print(mm_1.mm2)