# 跨文件索引设计（讨论稿）

本文档定义 **工作区级索引的数据模型、构建策略，以及 `goto` / `hover` / `references` / `diagnostics` / `workspace/symbol` 五个 LSP 能力如何消费索引**。与 [`architecture.md`](architecture.md) 总览拆分、单独迭代定案。需求级约定见 [`requirements.md`](requirements.md) §3.2.1。

---

## 1. 语义模型与名字解析

本章定义跨文件索引的"世界观"——静态分析层对 Lua 运行时语义的近似方式。

### 1.1 全局已见

- 工作区内各文件对 **全局环境** 的贡献进入 **合并视图**（遵守 `local` / 块作用域）。
- **不要求**先 `require` 才能看见其它文件的全局符号。
- 同名冲突：保留 **候选列表**（路径优先级、配置等显式策略）；UI 展示歧义，避免静默选错。

### 1.2 `require` 绑定

- **条件**：`local <name> = require(<可静态解析的字符串>)`。
- **路径解析**：模块串 → 目标 URI（`?.lua`、别名等可配置）。
- **语义**：`<name>` 绑定到目标文件 `return` 的模块值近似；不用此边推断"其它文件是否加载"。
- **反向索引**：`(目标 URI) → [(来源文件, 局部名), …]`。
- 非静态 `require`、拼接路径：不建绑定；全局仍靠 §1.1。

### 1.3 Emmy 类型名

- `---@class`、`---@alias` 等进入 **工作区类型表**。
- 解析顺序：本文件类型表 → 工作区类型表（含冲突候选）。

### 1.4 标识符解析流程

对光标下的标识符，按以下顺序决议：

1. **Lua 作用域**（`local`、块、闭包）。
2. 若为 `local` 且属 §1.2 `require` 绑定 → 目标文件 `return` + 模块摘要。
3. 若为全局自由名 → 查全局合并表。
4. Emmy 类型名 → 本文件类型表 → 工作区类型表。

此流程是 `goto`、`hover`、`references` 的共同入口。

### 1.5 与动态现实的边界

- `_ENV`、运行期改全局：配置或 `unknown`。
- 大工作区：摘要 + 分片 + 可取消后台任务。

---

## 2. 索引数据结构

### 2.1 每文件摘要 `DocumentSummary`

对每个 URI，与 **版本或内容哈希** 绑定。除名字索引外，还承载表达式类型传播所需事实：

| 数据 | 说明 |
|------|------|
| **全局贡献** | 名称、类属、定义区间 |
| **局部层** | `local` / 形参 / 块；含 §1.2 `require` 绑定 `(局部名 → 目标 URI + return 指纹)` |
| **类型表** | Emmy 类型定义 + 类型名出现区间 |
| **`FunctionSummary`** | 参数、返回类型、返回值形状、关联 Emmy 注解、惰性重算标记 |
| **`LocalTypeFacts`** | 关键局部变量的类型候选及其来源（赋值、调用返回、字段访问等） |
| **`TableShape` / `MemberFacts`** | 字段名 → 字段类型、注解、定义位置；用于成员访问链逐段解析 |
| **可选：引用出现列表** | 值标识符出现位点（供 `references`）；可按名分桶 |

**AST**：LRU 缓存；跳转优先走摘要。

### 2.2 工作区聚合层

| 分片 | 键 → 值 | 更新方式 |
|------|---------|---------|
| **`GlobalShard`** | 全局名 → [候选定义…] | 按名差分：删旧文件贡献、插新 |
| **`TypeShard`** | 类型裸名 → [候选定义…] | 同上 |
| **`RequireByReturn`** | 目标 URI → [(来源文件, 局部名)…] | 绑定增删；目标 `return` / Emmy 变更时失效依赖方缓存 |
| **`FunctionReturnIndex`** | 函数符号 → 返回摘要 | 可并入类型/符号分片 |
| **可选：`Postings` / `symbolId → refs`** | 引用索引 | 见 §5.2 |

分片可按名字哈希/前缀切段，利于锁与并行。

---

## 3. 类型推断与 Table Shape

本章定义 `DocumentSummary` 中类型维度的信息如何产生——直接决定 `hover` 和 `diagnostics` 的能力上限。

### 3.1 链式字段推断

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

### 3.2 第一阶段类型传播边界

- **分支合并**：`if/else` 对可达分支做候选并集；合并前做轻量常量传播与死分支裁剪。
- **循环**：保守策略；不追求复杂循环收敛分析，必要时返回 `unknown`。
- **函数返回**：`---@return` 等显式注解优先于函数体推断。
- **元表**：第一阶段不支持 `metatable.__index` 参与字段链解析。
- **联合类型字段访问**：对每个候选分别解析字段，再合并结果；不因某个候选失败就整体降为 `unknown`。
- **多返回值函数**：`FunctionSummary` 保留候选列表与合并后的总摘要。
- **Hover 展示**：以最终字段结果为主，不默认展开详细传播链。

### 3.3 Lua Table Shape

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
- 此状态直接影响诊断分级（见 §5.4）。

### 3.4 Emmy 与 Table Shape 优先级

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

### 3.5 全局 Table 合并

全局 table 作为跨文件共同定义的结构，需要特殊支持。

- 跨文件合并时同时保留两层身份：
  - **逐段节点树**：`_G`、`_G.A`、`_G.A.B`
  - **完整路径索引**：`_G.A.B.C`
- 多个文件对同一全局路径补字段时：做工作区级结构合并，保留来源文件/语句，同一字段冲突保留多候选来源。
- 冲突结果同时保留合并后的联合类型/候选集合与候选来源列表。
- "创建语句"采用宽松识别：只要能静态确认是创建或延续同一全局节点，即可并入。
- 一旦全局链路某节点已显式绑定 Emmy 类型，从该节点开始停止 `GlobalTable` 扩张，改走 Emmy 字段解析。

---

## 4. 索引构建与维护

### 4.1 冷启动策略

采用 **摘要驱动的混合式渐进索引**：

1. **并行**扫描工作区文件，生成每文件 `DocumentSummary`。
2. 每份 `DocumentSummary` 产出后，**流式 merge** 到工作区聚合层。
3. 查询层显式感知 **indexing state**；首轮全库完成前允许渐进可用但可能不完整，完成后进入稳定工作区语义。
4. 热路径以摘要查询优先，避免在查询时做整库级即时分析。

查询层状态分为：

- **`Initializing`**：摘要与聚合层正在构建；`goto`/`hover` 可返回部分结果（接受漏报），`workspace/symbol` 提供渐进结果，`references` 应向用户标明结果可能不完整。
- **`Ready`**：首轮摘要与一致性 merge 完成；查询结果视为完整工作区语义。

冷启动与编辑期增量保持同一心智模型：冷启动 = 从空状态对全部文件做大批量差量导入；编辑期 = 基于已有索引对少量文件做小批量差量更新。

### 4.2 编辑期增量更新（`didChange` / 保存）

1. 重解析 → `S'`；与旧 `S` **diff**。
2. `GlobalShard` / `TypeShard`：对涉及名字删旧桩 + 插新桩。
3. `RequireByReturn`：绑定增删；失效相关缓存。
4. 函数返回摘要、局部类型事实、字段形状：按受影响符号/表达式链差量失效。
5. 引用索引：按 postings 或 symbol 脏集更新。
6. 冲突排序：仅对受影响名字重算。

批量文件变更：合并、去抖、可取消。

### 4.3 持久化缓存

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

### 4.4 多根工作区与 `exclude`

**多根工作区**：默认多个 root 合并为统一工作区索引；可配置为按 root 隔离。

**`exclude` 文件**：

- 默认不进入工作区索引。
- 用户当前打开该文件时，允许临时单文件分析（局部跳转、基础 Hover）。
- 打开中的 `exclude` 文件可读取工作区索引结果用于跨文件解析，但自身分析结果默认不回写工作区索引。

---

## 5. LSP 能力消费索引

本章是文档核心——每个 LSP 功能如何查询上述索引结构。

### 5.1 `goto definition` / `hover`

**热路径查询**：

| 场景 | 查询路径 | 复杂度 |
|------|---------|--------|
| 局部变量 | 当前文件摘要 + 必要 AST | O(1) |
| `local` + `require` 绑定 | `RequireByReturn` 绑定表 | O(1) |
| 全局名 / 类型名 | `GlobalShard` / `TypeShard` 一次分片查找 | O(1) |
| **链式字段**（`obj.pos.x`） | 按"表达式 → 基础类型 → 字段类型 → 下一段字段类型"逐段解析；优先命中 `FunctionSummary`、`LocalTypeFacts`、`TableShape` / Emmy 字段信息 | O(链长) |

`goto definition` 在存在多个候选时：有明显最佳候选则直接跳转，否则展示候选列表。最佳候选按 **显式类型来源优先** 排序（Emmy 定义 / 显式注解 > 纯 shape 推断）。

### 5.2 `textDocument/references`

#### 总体语义

- 默认查找 **与当前光标同一决议的语义目标**，而非所有同名文本出现。
- 变量名与字段名都先完成语义决议（通过 §1.4 流程），再基于其身份查找读写引用。
- 标准 LSP 返回位置列表，不直接携带 read/write 元数据；但服务端内部索引保存这些信息，供过滤和未来扩展。

#### 内部引用分类

- 第一阶段显式记录：`read` / `write` / `readwrite` / `unknown`。
- 内部统一收集声明/定义/读/写位点；响应时按 `includeDeclaration` 参数裁剪。

#### 身份模型

| 语义类别 | 主查询身份 | 辅助/简化身份 |
|---------|-----------|-------------|
| 局部变量 | `LocalSymbolId`（闭包捕获沿用） | — |
| 全局变量/全局链路 | `GlobalNodeId` | 全局名 / 完整路径字符串 |
| Emmy 字段 | `TypeId + FieldName + DeclSite` | `TypeId + FieldName` |
| 普通 table shape 字段 | `TableShapeId + FieldKey + OriginSite` | `TableShapeId + FieldKey` |
| 全局 table 字段 | `GlobalNodeId + FieldKey + OriginSite` | `GlobalNodeId + FieldKey` |

#### `OriginSite` / 来源锚点

字段身份内部保留完整来源集合（注解来源、首次写入、其他补充），并选择一个 **主锚点** 用于稳定 identity 与缓存键。主锚点优先级：显式注解/声明来源优先；否则按确定性规则（语法位置稳定排序）选取 canonical source，保证重扫顺序变化不改变字段 identity。

#### 候选策略

- 多候选身份时，默认只查最佳候选；底层索引仍保留完整候选集合。
- 与 `goto definition` 共享同一套候选评分来源（显式类型来源优先）。
- 与 `goto definition` 不同：`references` 在分数接近时更保守，更易回退到多候选策略。

#### 实现建议

- 朴素实现：`Postings[name]` + 语义过滤。
- 混合实现：`symbolId → refs` 懒惰维护；定义变更置脏引用簇；大任务分段 + `$/cancelRequest`。
- 热路径上优先解析光标目标身份，再查引用索引。

### 5.3 `workspace/symbol`

#### 第一阶段收录范围

| 收录 | 不收录 |
|------|-------|
| 全局变量、全局函数 | 局部变量 |
| `---@class`、`---@alias` | 普通 table 内部字段、Emmy 字段 |
| 可明确归属到 `---@class` 的成员函数 | 仅"可能是方法"的动态写法 |

#### 全局路径收录

- 链式全局路径同时收录顶层全局名与完整路径，完整路径优先作为可搜索项。
- 例：`_G.Mgr.HellModel` 可直接被搜到。

#### 展示与排序

- 主显示短名 + 附带完整路径/容器信息（如 主名 `HellModel`，容器 `_G.Mgr`）。
- 默认排序：匹配质量为首要因素，符号类别为次级。
- 默认统一排序；内部保留 `kind`/`container`/`sourceType` 等元信息，以便后续按类别分组。

### 5.4 诊断

诊断采用 **Emmy 路径严格、Lua 路径保守** 的平衡策略。一旦命中明确 Emmy 类型，就按 Emmy 路径处理；否则按 Lua table shape 路径处理。

#### Emmy 路径诊断

| 情况 | 默认 severity |
|------|-------------|
| 字段存在但赋值类型不兼容 | `error`（允许用户降级） |
| 字段不存在（读取或写入） | `error`（允许用户降级） |

一旦进入 Emmy 语义，字段读写不再回退到 table shape。

#### Lua 路径诊断

第一阶段只对高确定性问题出诊断：

| 情况 | 默认 severity |
|------|-------------|
| 显式 `nil` 成员访问或赋值（`local x = nil; x.a = 1`） | `error` |
| 显式非对象值成员访问（`local x = 1; print(x.a)`） | `error` |
| **closed** shape 上明确不存在的字段访问 | `error` |
| 开放/动态结构上的未知字段访问 | `warning` |
| 字段赋值类型与静态 shape 明显冲突 | `warning` |
| 字段置 `nil` 后继续深层访问子字段 | `warning` |
| 联合候选里仍有合法路径支持当前字段 | 不报诊断 |

#### Lua 路径诊断配置

第一阶段先提供两组分组配置（Lua 高确定性错误 severity、Lua 保守提示 severity）；内部保留更细的诊断原因，后续可扩展到按规则单独配置。

---

## 6. 候选决议与配置项

### 6.1 全局同名冲突策略

当同一全局名存在多个候选定义时：

- 默认按综合打分选最佳候选，但内部保留完整候选列表。
- 打分核心因素：
  - **显式定义强度**：Emmy 绑定、显式全局创建/赋值链优先于弱推断
  - **来源稳定性**：声明更完整、候选更少、语义更确定者优先
  - **路径/工程规则**：配置指定的优先目录等
- 分数接近时：`goto definition` 尽量选一个；`hover` / `references` 倾向回退到候选列表。

整体原则：**内部保留完整候选信息；默认给出一个可用答案；高歧义时优先保守。**

### 6.2 建议配置项

| 设置项 | 建议值 | 说明 |
|--------|--------|------|
| `lua.gotoDefinition.strategy` | `auto \| single \| list` | `auto`：有明显最佳候选则单跳，否则多目标 |
| `lua.references.strategy` | `best \| merge \| select` | `best`：只查最佳候选；`merge`：多候选并集；`select`：先选再查 |
| `lua.globalConflict.strategy` | `auto \| single \| list` | 全局同名冲突决议策略 |
| `lua.index.cacheMode` | `summary \| memory` | 默认 `summary` |
| `lua.index.cacheLocation` | `user \| workspace` | 默认 `user` |
| `lua.workspace.indexMode` | `merged \| isolated` | 多根工作区默认 `merged` |
| `lua.workspaceSymbol.grouping` | `flat \| grouped` | 默认 `flat` |
| `lua.diagnostics.emmyTypeMismatchSeverity` | `error` | Emmy 字段赋值不兼容 |
| `lua.diagnostics.emmyUnknownFieldSeverity` | `error` | Emmy 类型下不存在字段 |
| `lua.diagnostics.luaErrorSeverity` | `error` | Lua 路径高确定性错误 |
| `lua.diagnostics.luaWarningSeverity` | `warning` | Lua 路径保守提示 |

---

## 7. 文档维护

- 本文件定案或重大变更时，同步 [`architecture.md`](architecture.md) §3.4 概要一节；需求变更回写 [`requirements.md`](requirements.md)。
