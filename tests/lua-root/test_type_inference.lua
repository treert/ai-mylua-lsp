local mgr;

local function test_mgr()
    mgr.do_something()
    mgr.do_another_thing()
    print(mgr.m_some)
end

local function create_mgr()
    local mgr = {}

    function mgr.do_something()
        
    end

    mgr.do_another_thing = function()
        
    end

    mgr.m_some = 123

    return mgr
end

mgr = create_mgr()
