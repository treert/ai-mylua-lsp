-- gg_test = 2
gg_test = 2

-- gg.a2 = 2
gg.a2 = 2

local mm = _G.gg

-- mm.m2 = 22
mm.m2 = 22

print(mm.a2)
print(mm.m1)
print(mm.mm1)

local mm_1 = gg

-- mm_1.mm2 = 222
mm_1.mm2 = 222

print(mm_1.a2)
print(mm_1.m1)
print(mm_1.mm1)


print(gg_test)

-- global LuaPanda
LuaPanda = {}
local this = LuaPanda

function this.f1()
    print("f1")
end

function this.test()
    this.f1()
end

print(LuaPanda)
print(_G.LuaPanda)

local _G = {}
print(_G.LuaPanda) -- waning

