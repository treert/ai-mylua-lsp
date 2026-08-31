-- _ENV / _G 语义手工测试用例
--
-- 注意：顶层的 `_ENV = {}` 会影响其后**所有**顶层语句，因此需要真全局
-- 环境的用例必须放在它之前，或包在 do...end 里。文件末尾的第 5 节故意
-- 使用顶层重定向，所以它必须是最后一节。

local print = print
local tostring = tostring
local setmetatable = setmetatable
local rawset = rawset

-- ============================================================
-- 1. 基本全局 + _G 别名
-- ============================================================

g1 = 123
print(g1)

-- _ENV 是 upvalue（闭包 upvalue），不是全局变量：
--   - 不应被高亮为 global（不带 global 语义修饰符）
--   - _G._ENV 在 Lua 中恒为 nil
local xx = _ENV        -- 可解析，其值即全局表；xx.g1 应能跳转
local x2 = _G._ENV     -- 应报 unknown field —— _ENV 不是全局表的字段

print(xx.g1)           -- 经 _ENV 捕获值访问全局，应可跳转到第 16 行

-- _G._G == _G，所以下面几种拼写与裸 g1 是同一个全局
local x3 = _G.g1
local x4 = _G._G.g1        -- 应可解析并跳转，不应报 unknown field
local x5 = _G._G._G.g1     -- 多重 _G 折叠，同理
local x6 = _ENV.g1         -- _ENV.X 等价于 X
local x7 = _ENV._G.g1      -- 先剥 _ENV. 再剥 _G.，仍等价于 g1

-- 反向：这两个运行时是 index nil，不做归一，应保持无法解析
local x8 = _G._ENV
local x9 = _ENV._ENV

print(gg_undefined)          -- 应报 undefinedGlobal
print(_G.gg_undefined)       -- 应报 unknown field on type '_G'

-- ============================================================
-- 2. setmetatable 沙箱：按 `{__index=_G}` 约定处理
--
--    语言服务**不追踪** __index 实际指向哪里。凡带元表（或类型推不出）
--    的环境一律假定为 `{__index=_G}`，即：
--      - 读：先查沙箱表自身字段，查不到回退全局表
--      - 写：落在沙箱表上，不进全局索引
--      - 两处都查不到 → 报 Undefined global（不再静默）
-- ============================================================

GlobalForSandbox = 42

-- 2a. 内联写法
do
    local _ENV = setmetatable({}, { __index = _G })
    -- 经 __index 实际可达真全局，应能跳转到 GlobalForSandbox 定义处，无诊断
    local v = GlobalForSandbox
    print(v)
    -- 沙箱内写入的名字落在沙箱表上，应能跳转到下面那行的写入处
    sandbox_own = 1
    local w = sandbox_own
    print(w)
    -- 沙箱表没有、全局也没有 → 运行时是 nil，应报 Undefined global
    local z = not_anywhere_at_all
    print(z)
end

-- 2b. 分离写法：shape 存在（就是 {} 字面量），但被 setmetatable 标为非穷尽
do
    local t = {}
    setmetatable(t, { __index = _G })
    local _ENV = t
    local v = GlobalForSandbox   -- 应与 2a 行为一致：可跳转、无诊断
    print(v)
end

-- 2c. rawset 同样让 shape 失去穷尽性
do
    local t = {}
    rawset(t, "injected", 1)
    local _ENV = t
    local v = GlobalForSandbox   -- 同样回退到全局
    print(tostring(v))
end

-- 2d. 负向控制：元表装在**别的**表上，不影响当前环境
do
    local other = {}
    setmetatable(other, { __index = _G })
    local _ENV = {}              -- 这张表字段集合已知且为空
    local v = GlobalForSandbox   -- 应报 envUnknownField，不得回退到全局
    print(v)
end

-- ============================================================
-- 3. 环境的字段集合是否"穷尽"决定读的行为（易误解的边界）
--
--    判据不是"写法看起来复杂不复杂"，而是 _ENV 能否解析出一个
--    **穷尽**（无元表、无 rawset）的 TableShape：
--      - 能 → 只查该表，缺失字段报 envUnknownField
--      - 不能 → 按 {__index=_G} 约定，查不到就回退全局，
--               两处皆无则报 Undefined global
-- ============================================================

GlobalForParam = 99

-- 3a. 形参 _ENV 无类型标注 → 非穷尽 → 回退
--     调用方完全可能就是传了真环境
local function run_in_env(_ENV)
    local v = GlobalForParam     -- 应可跳转到 GlobalForParam 定义处，无诊断
    return v
end
print(run_in_env(_G))

-- 3b. 工厂函数**无返回值** → 非穷尽 → 回退
local function make_nothing() end
do
    local _ENV = make_nothing()
    local v = GlobalForParam     -- 应可跳转，无诊断
    print(v)
    local bad = nope_not_here    -- 两处皆无 → 应报 Undefined global
    print(bad)
end

-- 3c. 工厂函数返回 setmetatable(...) → 泛型返回未回填 → 非穷尽 → 回退
local function make_sandbox() return setmetatable({}, { __index = _G }) end
do
    local _ENV = make_sandbox()
    local v = GlobalForParam     -- 应可跳转，无诊断
    print(v)
end

-- 3d. 工厂函数返回 **{} 字面量** → 返回类型推断为穷尽 shape → 不回退
--     这与直接写 `local _ENV = {}` 语义等价：运行时就是一张没有元表的
--     空表，自由名确实全是 nil。所以 goto 无结果是**正确**的。
local function make_empty() return {} end
do
    local _ENV = make_empty()
    local v = GlobalForParam     -- 不应跳转（环境字段集合已知且为空）
    print(v)
end
-- 注意：上面块内的读**不报** envUnknownField —— 该诊断按设计只检查 chunk
-- 顶层直线执行流，do...end 块内不在其中（见 §1.3.1 双侧围栏）。因此这里
-- 既无跳转也无提示。同样的代码放在顶层就会报（见第 5 节）。

-- 3e. 穷尽环境的自有字段
do
    local _ENV = { only_this = 1 }
    print(only_this)             -- 环境自有字段，应可跳转到上一行
end

-- 3f. 补全分层（把光标放到 `= ow` / `= ca` 的末尾手动触发补全观察）
--     判据与导航/诊断同源，因此补全给出的名字不会被诊断立刻标红。
GlobalForCompletion = 1
do
    -- 穷尽环境：只应提示 own_a / own_b，**不应**出现 GlobalForCompletion
    local _ENV = { own_a = 1, own_b = 2 }
    local pick = own_a
    print(pick)
end
do
    -- 非穷尽环境：own_c 与 GlobalForCompletion 都应出现
    local t = { own_c = 3 }
    setmetatable(t, { __index = _G })
    local _ENV = t
    local pick = own_c
    print(pick)
end
do
    -- local 是词法名，不受环境影响，任何沙箱内都应照常提示
    local captured_local = 1
    local _ENV = {}
    local pick = captured_local
    print(pick)
end

-- ============================================================
-- 4. local _ENV = _G 与默认环境完全等价
-- ============================================================

do
    local _ENV = _G              -- 重述默认环境，不算重定向
    still_a_real_global = 1      -- 应正常进入全局索引
    print(still_a_real_global)                    -- 应可跳转，无诊断
end
print(still_a_real_global)                    -- 应可跳转，无诊断


-- ============================================================
-- 5. 顶层 _ENV = {}：字段集合完全已知 → 严格检查
--    （必须放最后：会影响其后所有顶层语句）
-- ============================================================

_ENV = {}
print(g1)      -- 应报 envUnknownField: 'g1' is not a field of the current _ENV
print(_G.g1)   -- _G 本身在空环境里也是 nil，应报 _G 缺失而非 _G 的字段问题

g1 = 321       -- 写入新环境，不得进入全局索引
print(g1)      -- 已在环境中，无诊断

g2 = g1 + 1000
print(g2)

-- 位置敏感性：重定向前后的同名自由名是不同符号
-- （此处 boundary_g 的两次写入都在 _ENV = {} 之后，属同一符号）
boundary_g = "first"
print(boundary_g)

print(read_before_write)   -- 应报 'read_before_write' is read before it is assigned
read_before_write = 1
