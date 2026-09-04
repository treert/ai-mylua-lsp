local t;

local function test()
    print(t.name)
    print(t.data[1])
    print(t.id)
    print(t.extra_id)
end

function create_table()
    return {
        name = "test",
        data = {1, 2, 3},
        id = 1,
    }
end

t = create_table()

t.extra_id = 11;

test()

local t2 = create_table()

print(t2.name)

-- t2 没有 extra_id，但是前面的 t 修改了 create_table 内部的 TableShape，导致 lsp 看到了 extra_id，这种情况只能接受了。
-- 设计上来说 同文件的 TableShape 保持 open 状态。
print(t2.extra_id) 
