# LSP 语义能力需求（讨论稿）

本文档定义 **Lua/EmmyLua 的语义约定**，以及 `goto` / `hover` / `references` / `diagnostics` / `workspace/symbol` 五个 LSP 能力的 **协议层需求细节**。与 [`architecture.md`](architecture.md) 总览拆分、单独迭代定案。需求级约定见 [`requirements.md`](requirements.md) §3.2.1。

- **索引内部架构**：数据模型（`DocumentSummary` / 聚合层）、类型推断规则、Table Shape 建模、增量更新与级联失效策略详见 [`index-architecture.md`](index-architecture.md)。本文档的 LSP 能力需求建立在该索引架构之上。

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

## 2. LSP 能力消费索引

本章是文档核心——每个 LSP 功能的协议层需求。索引数据模型与查询机制见 [`index-architecture.md`](index-architecture.md)。

### 2.1 `goto definition` / `hover`

**热路径查询**：

| 场景 | 查询路径 | 复杂度 |
|------|---------|--------|
| 局部变量 | 当前文件摘要 + 必要 AST | O(1) |
| `local` + `require` 绑定 | `RequireByReturn` 绑定表 | O(1) |
| 全局名 / 类型名 | `GlobalShard` / `TypeShard` 一次分片查找 | O(1) |
| **链式字段**（`obj.pos.x`） | 按"表达式 → 基础类型 → 字段类型 → 下一段字段类型"逐段解析；优先命中 `FunctionSummary`、`LocalTypeFacts`、`TableShape` / Emmy 字段信息 | O(链长) |

`goto definition` 在存在多个候选时：有明显最佳候选则直接跳转，否则展示候选列表。最佳候选按 **显式类型来源优先** 排序（Emmy 定义 / 显式注解 > 纯 shape 推断）。

### 2.2 `textDocument/references`

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

### 2.3 `workspace/symbol`

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

### 2.4 诊断

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

## 3. 候选决议与配置项

### 3.1 全局同名冲突策略

当同一全局名存在多个候选定义时：

- 默认按综合打分选最佳候选，但内部保留完整候选列表。
- 打分核心因素：
  - **显式定义强度**：Emmy 绑定、显式全局创建/赋值链优先于弱推断
  - **来源稳定性**：声明更完整、候选更少、语义更确定者优先
  - **路径/工程规则**：配置指定的优先目录等
- 分数接近时：`goto definition` 尽量选一个；`hover` / `references` 倾向回退到候选列表。

整体原则：**内部保留完整候选信息；默认给出一个可用答案；高歧义时优先保守。**

### 3.2 建议配置项

| 设置项 | 建议值 | 说明 |
|--------|--------|------|
| `mylua.gotoDefinition.strategy` | `auto \| single \| list` | `auto`：有明显最佳候选则单跳，否则多目标 |
| `mylua.references.strategy` | `best \| merge \| select` | `best`：只查最佳候选；`merge`：多候选并集；`select`：先选再查 |
| `mylua.globalConflict.strategy` | `auto \| single \| list` | 全局同名冲突决议策略 |
| `mylua.index.cacheMode` | `summary \| memory` | 默认 `summary` |
| `mylua.index.cacheLocation` | `user \| workspace` | 默认 `user` |
| `mylua.workspace.indexMode` | `merged \| isolated` | 多根工作区默认 `merged` |
| `mylua.workspaceSymbol.grouping` | `flat \| grouped` | 默认 `flat` |
| `mylua.diagnostics.emmyTypeMismatchSeverity` | `error` | Emmy 字段赋值不兼容 |
| `mylua.diagnostics.emmyUnknownFieldSeverity` | `error` | Emmy 类型下不存在字段 |
| `mylua.diagnostics.luaErrorSeverity` | `error` | Lua 路径高确定性错误 |
| `mylua.diagnostics.luaWarningSeverity` | `warning` | Lua 路径保守提示 |

---

## 4. 文档维护

- 本文件定案或重大变更时，同步 [`architecture.md`](architecture.md) §3.4 概要一节；需求变更回写 [`requirements.md`](requirements.md)。
- 索引数据模型与构建策略变更维护在 [`index-architecture.md`](index-architecture.md)。
