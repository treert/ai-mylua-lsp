---@class abc_mgr
local abc_mgr = {}

abc_mgr.name = "abc_mgr"
abc_mgr.version = "1.0.0"

function abc_mgr.init()
    print("init")
end

function abc_mgr.update()
    print("update")
end

function abc_mgr.destroy()
    print("destroy")
end

function abc_mgr.get_name()
    return abc_mgr.name
end

function abc_mgr.test_print(...)
    print("abc_mgr", ...)
end

return abc_mgr
