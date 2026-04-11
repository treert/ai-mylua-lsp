# 索引结构与类型推断策略

本文档定义 **工作区索引的内部架构——数据模型、类型推断规则、`DocumentSummary` 如何生成、跨文件类型如何解析、增量更新与级联失效如何运作**。是 LSP 实现层面的指导文档，侧重"索引怎么建、怎么查、怎么更新"。

- **上层消费者**：[`lsp-semantic-spec.md`](lsp-semantic-spec.md) 定义 LSP 能力如何消费本文档描述的索引结构，以及 Lua/EmmyLua 的语义约定。
- **架构总览**：[`architecture.md`](architecture.md) §3.2–§3.4。
- **需求约定**：[`requirements.md`](requirements.md) §3.2.1。

---

## 1. 核心思想：两层推断，惰性解析

### 1.1 问题

构建全局索引需要知道每个变量和函数返回值的类型，这几乎等同于遍历工作区所有赋值表达式和函数体。在 5 万文件级工作区中不可能对每次修改做全量分析。

### 1.2 方案

将类型推断分为 **两层**，以 `DocumentSummary` 为界：

| 层 | 范围 | 时机 | 依赖 |
|----|------|------|------|
| **单文件推断** | 遍历单文件 AST，产出 `DocumentSummary` | 文件变更时立即执行 | **零跨文件依赖** |
| **跨文件解析** | 沿 Summary 中的符号引用桩链式追踪 | goto/hover 请求时按需 / 诊断后台批量 | 其他文件的 Summary |

`DocumentSummary` 存储的是 **"类型事实的食谱"（stub + 本地事实）**，不是 **"做好的菜"（完全解析后的类型）**。解析是惰性的、按需的、可缓存的。

---

## 2. 索引数据模型

### 2.1 每文件摘要 `DocumentSummary`

对每个 URI，与 **版本或内容哈希** 绑定。除名字索引外，还承载表达式类型传播所需事实：

| 数据 | 说明 |
|------|------|
| **全局贡献** | 名称、类属、定义区间 |
| **局部层** | `local` / 形参 / 块；含 `require` 绑定 `(局部名 → 目标 URI + return 指纹)` |
| **类型表** | Emmy 类型定义 + 类型名出现区间 |
| **`FunctionSummary`** | 参数、返回类型、返回值形状、关联 Emmy 注解、惰性重算标记 |
| **`LocalTypeFacts`** | 关键局部变量的类型候选及其来源（赋值、调用返回、字段访问等） |
| **`TableShape` / `MemberFacts`** | 字段名 → 字段类型、注解、定义位置；用于成员访问链逐段解析 |
| **可选：引用出现列表** | 值标识符出现位点（供 `references`）；可按名分桶 |

**AST**：LRU 缓存；跳转优先走摘要。

### 2.2 工作区聚合层

聚合层是连接"单文件 Summary"和"跨文件查询"的桥梁：

```
┌─────────────┐   ┌─────────────┐   ┌─────────────┐
│ file_a.lua  │   │ file_b.lua  │   │ file_c.lua  │
│ Summary     │   │ Summary     │   │ Summary     │
└──────┬──────┘   └──────┬──────┘   └──────┬──────┘
       │                 │                 │
       ▼                 ▼                 ▼
┌──────────────────────────────────────────────────┐
│              工作区聚合层                           │
│  ┌──────────────┐  ┌──────────────┐              │
│  │ GlobalShard  │  │ TypeShard    │              │
│  │ name→[候选]  │  │ name→[候选]  │              │
│  └──────────────┘  └──────────────┘              │
│  ┌──────────────────┐  ┌───────────────────────┐ │
│  │ RequireByReturn  │  │ 解析缓存              │ │
│  │ URI→[依赖方]     │  │ stub→resolved_type    │ │
│  └──────────────────┘  └───────────────────────┘ │
└──────────────────────────────────────────────────┘
       │
       ▼
  LSP 请求处理（goto / hover / references / diagnostics）
```

| 分片 | 键 → 值 | 更新方式 |
|------|---------|---------|
| **`GlobalShard`** | 全局名 → [候选定义…] | 按名差分：删旧文件贡献、插新 |
| **`TypeShard`** | 类型裸名 → [候选定义…] | 同上 |
| **`RequireByReturn`** | 目标 URI → [(来源文件, 局部名)…] | 绑定增删；目标 `return` / Emmy 变更时失效依赖方缓存 |
| **`FunctionReturnIndex`** | 函数符号 → 返回摘要 | 可并入类型/符号分片 |
| **可选：`Postings` / `symbolId → refs`** | 引用索引 | 见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §2.2 |

分片可按名字哈希/前缀切段，利于锁与并行。更新粒度是 **名字级**（不是文件级）：一个文件修改只影响它贡献的那些名字槽，其余名字槽不变。

---

## 3. 单文件推断：`DocumentSummary` 生成

### 3.1 遍历范围

对单个文件，遍历 AST 产出以下事实：

| AST 节点类型 | 产出 |
|-------------|------|
| `local x = expr` | `LocalTypeFacts[x]`：推断 `expr` 的类型 |
| `local x = require("mod")` | `RequireBinding("mod")`：符号引用桩，不立即解析 |
| `local x = func()` | `LocalTypeFacts[x]` = `CallReturn(func_ref)`：引用函数返回摘要 |
| `function foo(...) return expr end` | `FunctionSummary[foo]`：参数、返回类型、Emmy 注解 |
| `t.field = expr` | `TableShape[t]` 追加/更新字段 |
| `t = { k = v, ... }` | 新建 `TableShape`，按字面值节点建立身份 |
| `G = G or {}` / `G.sub = ...` | `GlobalContributions`：全局名贡献 |
| `---@class` / `---@type` / ... | `TypeTable`：Emmy 类型定义 |
| 标识符读取 | 可选：`References` 出现列表 |

### 3.2 单文件内的类型推断规则

在单文件内，能确定的类型直接记录；需要跨文件的用 **符号引用桩（symbolic stub）** 占位：

```
已知类型（直接记录）          符号引用桩（延迟解析）
─────────────────────       ──────────────────────
字面量：string, number, …    require 返回值：RequireRef("mod")
本地函数调用返回（函数体在     跨文件函数调用：CallReturn(RequireRef("mod"), "func_name")
  同一文件且已分析）
table 字面值构造              全局变量引用：GlobalRef("Mgr")
Emmy 注解声明的类型           Emmy 类型名引用：TypeRef("PlayerData")
```

### 3.3 示例

```lua
-- game/player.lua
local proto = require("protocol")
local cfg   = require("config.player")

local function create()
    local p = proto.new_player()
    p.name  = "test"
    p.level = cfg.DEFAULT_LEVEL
    return p
end

Mgr = Mgr or {}
Mgr.create = create
```

产出的 `DocumentSummary`：

```
RequireBindings:
  proto → RequireRef("protocol")
  cfg   → RequireRef("config.player")

FunctionSummary[create]:
  locals:
    p → CallReturn(RequireRef("protocol"), "new_player")
  table_shape(p):
    name:  string                                         -- 直接确定
    level: FieldOf(RequireRef("config.player"), "DEFAULT_LEVEL")  -- 符号桩
  return: Ref(p)

GlobalContributions:
  "Mgr"        → TableShape (open)
  "Mgr.create" → FuncRef(create)
```

**关键**：整个 Summary 的生成不需要读取 `protocol.lua` 或 `config/player.lua`。

---

## 4. 类型推断与 Table Shape

本章定义 `DocumentSummary` 中类型维度的信息如何产生——直接决定 `hover` 和 `diagnostics` 的能力上限。

### 4.1 链式字段推断

以下能力为硬性需求：

```lua
local obj = some_outer_func()
local px = obj.pos.x   -- hover 在 x 上需要沿链推出字段类型
```

采用 **摘要驱动的强静态推断**：

- `DocumentSummary` 不仅记录"定义了什么"，还记录"值/函数/表达式可推出什么类型事实"。
- `hover` / `goto` 遇到成员访问链时，执行 **表达式类型传播**，而非仅查名字表。
- 类型来源优先级：**局部事实 → 函数返回摘要 / 模块 `return` 摘要 → Emmy 类型表 / 字段注解 → 结构推断 → `unknown`**。
- 第一阶段支持"明显可静态分析"的强推断；动态 `__index`、不可静态解析调用等显式降级为 `unknown` 或候选集合。

这意味着：`DocumentSummary`、聚合层、热路径查询都必须把 **函数返回值、局部变量类型传播、字段形状** 视为一等数据。

### 4.2 第一阶段类型传播边界

- **分支合并**：`if/else` 对可达分支做候选并集；合并前做轻量常量传播与死分支裁剪。
- **循环**：保守策略；不追求复杂循环收敛分析，必要时返回 `unknown`。
- **函数返回**：`---@return` 等显式注解优先于函数体推断。
- **元表**：第一阶段不支持 `metatable.__index` 参与字段链解析。
- **联合类型字段访问**：对每个候选分别解析字段，再合并结果；不因某个候选失败就整体降为 `unknown`。
- **多返回值函数**：`FunctionSummary` 保留候选列表与合并后的总摘要。
- **Hover 展示**：以最终字段结果为主，不默认展开详细传播链。

### 4.3 Lua Table Shape

#### 核心建模

- 每个 Lua table 字面值都是一等语义对象，生成稳定的 `TableShapeId`。
- 主身份按字面值 AST 节点建模；局部变量、字段、返回值都只是引用该节点。
- `FunctionSummary` 对返回的 table 值同时保留：
  - **导出 shape 摘要**：供查询热路径与跨文件消费。
  - **回指到源 table 字面值节点的引用**：供单文件内追踪。
- 普通 table shape 在单文件内可读写；出了文件后只暴露只读摘要。

#### 单文件内 shape 规则

- 空表 + 后续补字段属于正常 shape 构建路径（`local t = {}; t.pos = {}; t.pos.x = 1`）。
- 两个局部变量引用同一个 table 节点时，后续写入作用在同一个 shape 上。
- 直接可判定的重绑定需精确更新别名关系。

#### 字段写入与 shape 更新

- 以下写入并入精确字段集：`t.x = v`、`t["x"] = v`、`t[expr] = v`（仅限 `expr` 可无歧义静态折叠）。
- 数组风格字段：固定整数索引记录到 `indexedFields`；不可静态确定的索引只更新 `arrayElementType`。
- 同一字段多次赋值默认合并成联合类型。
- `t.x = nil` 不删除字段，只标记为可为 `nil`（第一阶段仅内部保留）。

#### 递归 shape 与截断

- 支持递归记录嵌套字段 shape，以支撑 `obj.pos.x` 链式 Hover。
- 第一阶段限制最大递归深度；达到上限后保留 `truncated shape` 标记，而非退化为 `unknown`。

#### Open / Closed Shape

- **closed shape**：字段集合相对封闭可枚举；未知字段访问接近"字段不存在"的强语义。
- **open shape**：对象仍可能动态扩展；未知字段访问仅表示"静态信息未覆盖"。
- 初始由纯字面量表构建且仅有静态字段写入时默认 closed；出现动态 key 写入、不可建模的逃逸等情况转为 open。
- 一旦转为 open，第一阶段不自动回到 closed。
- 此状态直接影响诊断分级（见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §2.4）。

### 4.4 Emmy 与 Table Shape 优先级

**统一规则**：只要某个节点已绑定明确 Emmy 类型，就从该节点开始 **完全切换到 Emmy 语义**；table shape 不再参与解析，也不作为补充展示。

适用范围：全局 table 链路、普通局部变量、函数返回值、字段访问链的中间节点。

对全局 table 及其链式语句，只认 **显式注解** 触发 Emmy 绑定：

- `---@type`
- 可明确把节点声明为 class / 实例的注解关系
- `---@return` 经由显式赋值链落到目标节点

#### `---@class` 字段来源约束

`---@class` 的 field 仅认以下来源：

- 注释声明的字段
- 成员函数
- 可明确归属的 `self.name = expr`（接收者须可明确归属到某个 class 或 table 节点）

### 4.5 全局 Table 合并

全局 table 作为跨文件共同定义的结构，需要特殊支持。

- 跨文件合并时同时保留两层身份：
  - **逐段节点树**：`_G`、`_G.A`、`_G.A.B`
  - **完整路径索引**：`_G.A.B.C`
- 多个文件对同一全局路径补字段时：做工作区级结构合并，保留来源文件/语句，同一字段冲突保留多候选来源。
- 冲突结果同时保留合并后的联合类型/候选集合与候选来源列表。
- "创建语句"采用宽松识别：只要能静态确认是创建或延续同一全局节点，即可并入。
- 一旦全局链路某节点已显式绑定 Emmy 类型，从该节点开始停止 `GlobalTable` 扩张，改走 Emmy 字段解析。

---

## 5. 跨文件解析：链式追踪

### 5.1 解析流程

当 goto/hover 需要最终类型时，沿 stub 链逐步解析：

```
hover 在 p.name 上
  │
  ├─ 查本文件 Summary → p 的类型 = CallReturn(RequireRef("protocol"), "new_player")
  │
  ├─ 解析 RequireRef("protocol")
  │    └─ 查聚合层 RequireByReturn → protocol.lua
  │    └─ 取 protocol.lua 的 Summary
  │
  ├─ 在 protocol.lua Summary 中查 FunctionSummary["new_player"]
  │    └─ 获取返回类型（可能是 TableShape 或 Emmy 类型）
  │
  ├─ 在返回类型中查字段 "name"
  │    └─ 得到最终类型：string
  │
  └─ 返回 hover 结果
```

每一步都是 **索引查表**（O(1) map lookup），整体复杂度 O(链长)，不需要重新解析任何 AST。

### 5.2 解析缓存

跨文件解析结果可缓存：

| 缓存键 | 缓存值 | 失效条件 |
|--------|--------|---------|
| `(RequireRef("protocol"), "new_player")` | 解析后的返回类型 | protocol.lua Summary 变更 |
| `(GlobalRef("Mgr"), "create")` | 解析后的函数签名 | 贡献 "Mgr.create" 的文件 Summary 变更 |

缓存失效采用 **标记脏 + 惰性重算**，不立即重建。

### 5.3 递归与环路保护

- 类型解析链可能出现循环（A require B, B require A 且互相引用返回值）。
- 解析过程维护 **访问栈**；检测到环路时返回 `unknown`，不无限展开。
- 设置最大解析深度上限（建议 32），超限降级为 `unknown`。

---

## 6. 索引构建与维护

### 6.1 冷启动策略

采用 **摘要驱动的混合式渐进索引**：

1. **并行**扫描工作区文件，生成每文件 `DocumentSummary`。
2. 每份 `DocumentSummary` 产出后，**流式 merge** 到工作区聚合层。
3. 查询层显式感知 **indexing state**；首轮全库完成前允许渐进可用但可能不完整，完成后进入稳定工作区语义。
4. 热路径以摘要查询优先，避免在查询时做整库级即时分析。

查询层状态分为：

- **`Initializing`**：摘要与聚合层正在构建；`goto`/`hover` 可返回部分结果（接受漏报），`workspace/symbol` 提供渐进结果，`references` 应向用户标明结果可能不完整。
- **`Ready`**：首轮摘要与一致性 merge 完成；查询结果视为完整工作区语义。

冷启动与编辑期增量保持同一心智模型：冷启动 = 从空状态对全部文件做大批量差量导入；编辑期 = 基于已有索引对少量文件做小批量差量更新。

### 6.2 编辑期增量更新

```
用户编辑 foo.lua 并保存
  │
  ├─ 1. 重新解析 foo.lua AST
  ├─ 2. 生成新的 Summary S'
  ├─ 3. 与旧 Summary S 做 diff
  │     ├─ GlobalContributions 变化 → 更新 GlobalShard（删旧插新）
  │     ├─ TypeTable 变化 → 更新 TypeShard
  │     ├─ RequireBindings 变化 → 更新 RequireByReturn
  │     └─ FunctionSummary / 导出类型签名变化 → 标记依赖方解析缓存为脏
  ├─ 4. 用 S' 替换 S
  └─ 5. 调度后续工作（诊断等）
```

批量文件变更：合并、去抖、可取消。

### 6.3 级联失效控制：签名指纹

**核心优化**：绝大多数文件修改不改变其对外暴露的类型签名。

为每个对外可见的类型事实计算 **签名指纹（signature fingerprint）**：

- `FunctionSummary` 的参数类型列表 + 返回类型描述 → 哈希
- 模块 `return` 值的类型描述 → 哈希
- `---@class` 的字段集合 + 类型 → 哈希
- 全局贡献的字段 shape → 哈希

**Diff 时比较指纹**：

| 指纹变化？ | 动作 |
|-----------|------|
| **未变** | 仅更新本文件 Summary，**不触发任何级联** |
| **变了** | 沿反向依赖标记脏：`RequireByReturn` 中的依赖方、引用该全局/类型的文件 |

实践中，函数内部逻辑修改（重构实现、改注释、调整局部变量名）**大多不改变签名指纹**，因此不会触发级联。只有接口签名变化（参数/返回类型/字段增删）才会级联。

### 6.4 协议文件（高扇出）的级联策略

协议/类型定义文件（如 `protocol.lua`、`types.lua`）可能被几乎所有文件依赖。其签名变更时：

1. 通过 `RequireByReturn` 和 `TypeShard` 反向索引找到所有依赖方
2. **不立即重建依赖方 Summary**（它们的 Summary 没变，变的是解析缓存）
3. 将依赖方的解析缓存标记为脏
4. **goto/hover**：下次请求时惰性重算（单次请求只触及一条链，延迟可控）
5. **诊断**：按优先级排入后台队列：
   - **最高**：当前编辑器可见的文件
   - **高**：当前打开的文件
   - **普通**：其余受影响文件
   - 支持 `$/cancelRequest`，新的编辑可取消排队中的旧任务

### 6.5 新文件加入

新文件加入等价于 "从空 Summary 到有内容的 Summary" 的增量更新：

1. 生成新文件 Summary
2. 其 GlobalContributions / TypeTable 等 merge 到聚合层
3. 如果新文件定义了已有全局名 / 类型名 → 候选列表扩充，相关解析缓存标记脏
4. 如果新文件 `require` 了已有模块 → 在 `RequireByReturn` 中添加反向条目

新文件**不会导致已有文件的 Summary 重建**——它们的 Summary 内容不因新文件存在而改变。影响只体现在聚合层（候选列表可能多了一项）和解析缓存（部分可能需要重算）。

### 6.6 持久化缓存

第一阶段将磁盘持久化缓存做成配置项控制：

- 默认启用 `DocumentSummary` 级别缓存（`summary` 模式）。
- 用户可关闭为纯内存索引（`memory` 模式）。
- 第一阶段不默认缓存聚合分片；聚合层启动时重建或渐进 merge。

#### 缓存失效规则

`DocumentSummary` 缓存失效绑定以下维度：

| 维度 | 失效处理 |
|------|---------|
| **文件内容哈希** | 对应文件 `DocumentSummary` 失效，重建该摘要并差量更新聚合层 |
| **grammar / LSP schema 版本** | 强失效，放弃旧缓存批次，整体重建 |
| **关键配置指纹** | 按强失效处理；第一阶段直接清空受影响缓存并重建 |

关键配置指纹仅纳入会影响语义结果的配置（`require` 解析规则、包含/排除模式、语义功能开关）；纯 UI 展示配置不进入该指纹。

#### 磁盘格式与缓存目录

- 每文件一个 `DocumentSummary` 缓存单元 + 独立元信息文件（schema 版本、配置指纹、URI 映射等）。
- 默认放在用户缓存目录（避免污染仓库）；可选放在工作区内。

### 6.7 多根工作区与 `exclude`

**多根工作区**：默认多个 root 合并为统一工作区索引；可配置为按 root 隔离。

**`exclude` 文件**：

- 默认不进入工作区索引。
- 用户当前打开该文件时，允许临时单文件分析（局部跳转、基础 Hover）。
- 打开中的 `exclude` 文件可读取工作区索引结果用于跨文件解析，但自身分析结果默认不回写工作区索引。

---

## 7. goto/hover 与诊断：两种消费模式

### 7.1 goto/hover（低延迟，按需解析）

```
请求到达
  → 定位光标所在的符号
  → 在 DocumentSummary 中找到符号的类型描述（可能含 stub）
  → 链式追踪 stub，每步查索引表（§5.1）
  → 命中解析缓存则直接返回；否则惰性解析并写入缓存
  → 返回结果
```

**延迟特征**：绝大多数情况 < 毫秒级（缓存命中）；缓存未命中时为 O(链长) 次 map 查找，通常仍在个位数毫秒。

### 7.2 诊断（高延迟可接受，后台批量）

```
触发条件：文件保存 / 聚合层变更 / 后台定时扫描
  → 加载目标文件的 Summary
  → 遍历所有表达式和赋值
  → 对每个表达式做完整类型解析（复用 §5 的链式追踪）
  → 对比赋值两端类型、检查字段访问合法性等
  → 产出诊断列表
  → publishDiagnostics
```

诊断与 goto/hover **共享同一套类型解析基础设施**，区别在于：

| | goto/hover | 诊断 |
|--|-----------|------|
| **解析范围** | 单个表达式链 | 整个文件所有表达式 |
| **延迟要求** | < 数毫秒 | 秒级可接受 |
| **触发方式** | 用户请求 | 后台队列 |
| **可取消** | 是（用户移动光标） | 是（新编辑取消旧任务） |

### 7.3 诊断调度

```
文件 F 被修改
  │
  ├─ 立即：重建 F 的 Summary，更新聚合层
  │
  ├─ 去抖后（~300ms）：对 F 做全量诊断
  │
  └─ 若 F 的签名指纹变化：
       ├─ 查反向依赖，找到受影响文件集合 D
       └─ 将 D 中文件按优先级排入诊断队列
            ├─ 当前可见文件 → 高优先级
            ├─ 已打开文件 → 中优先级
            └─ 其余文件 → 低优先级（可被新任务抢占）
```

---

## 8. 依赖关系的三种通道

索引中的跨文件依赖通过以下三种通道建立：

| 通道 | 键类型 | 正向查询 | 反向查询 |
|------|--------|---------|---------|
| **require 绑定** | 模块路径字符串 | `require("x")` → 目标 URI → 目标 Summary 的 return 类型 | `RequireByReturn[URI]` → 所有 require 该模块的文件 |
| **全局名引用** | 全局名字符串 | `GlobalShard["Mgr"]` → 候选定义列表 | 候选定义列表中的来源文件变更时 → 所有引用该名字的文件 |
| **Emmy 类型名** | 类型名字符串 | `TypeShard["PlayerData"]` → 类型定义 | 类型定义变更 → 所有引用该类型名的文件 |

所有三种通道的键都是 **字符串**。这是刻意为之——字符串键使得 Summary 可以在不知道目标文件是否存在的情况下生成，实现了单文件推断的完全独立性。

---

## 9. 文档维护

- 本文件定案或重大变更时，同步 [`architecture.md`](architecture.md) §3.4 概要一节。
- LSP 能力消费索引的协议层需求维护在 [`lsp-semantic-spec.md`](lsp-semantic-spec.md)。
- 需求变更回写 [`requirements.md`](requirements.md)。
