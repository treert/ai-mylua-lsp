print "$ Start\n"

local s = "123"
assert("123 nil" == $"$s $sss")

assert("1+2=3" == $"1+2=${1+2}")

assert("2+3=5" == "2" ..$"+${3}"..$"=${2+3}")

print "$string ok"

local f1 = ${ return "f1" }

local f2 = $(ff){return "f2 " .. ff()}

assert(f2(f1) == "f2 f1")

print "$function ok"

local tb = {
	local = 1,
}
tb.end = 1
function tb:for()
	
end

print "tb.end = ok"

print "start test ??"

local xxxx = nil
assert((xxxx??123) == 123)
assert((xxxx??xxxx??false or 123) == 123)
assert((false??123) == false)
assert((false or 123) == 123)

print "start test ?"
assert(xxx?.xx == nil)
assert(xxx?.xx?.xxx?.xx == nil)
assert(xxx?.xx?.xxx?.xx ?? 21 == 21)
assert(xxx?:xx()?['xxx']?.xx ?? 21 == 21)

local f1 = ${return 1,2,3}
local f2 = $(...){return select("#",...)}

assert(f2(f1?()) == 3)
assert(f2(xxx?()) == 1)

print "test yield concat here"

local count = 0
local t = setmetatable({},{
    __tostring = function ()
        count = count+1
        local msg = coroutine.yield(count)
        return msg
    end
})
local co = coroutine.create(function ()
    assert(tostring(t) == "t1")
    assert($"${t}-111" == 't2-111')
    return 'finish'
end)

local ok,msg = coroutine.resume(co)
if true then
    assert(not ok and string.find(msg, "yield across a C-call",1,true))
else
    assert(ok and msg == 1)

    local ok,msg = coroutine.resume(co, "t1")
    assert(ok and msg == 2)

    local ok,msg = coroutine.resume(co , "t2")
    assert(ok and msg == 'finish')

    local ok,msg = coroutine.resume(co)
    assert(not ok)
end

-- $string inner expr not support multi-line
ok = load([[ a = $'${ 123 }']])

assert(ok)

ok = load([[ a = $'${
     123 }']])

assert(not ok)

--- test 1_000_000

assert(1_000_000 == 1000000)
assert(1._23 == 1.23)
assert(1._23e1_0 == 1.23e10)

print "$ OK\n"