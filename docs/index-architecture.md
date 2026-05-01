# 索引架构

工作区索引的内部设计：数据模型、类型推断策略、`DocumentSummary` 生成、跨文件解析、增量更新与级联失效。

- 上层消费：[`lsp-semantic-spec.md`](lsp-semantic-spec.md)
- 架构总览：[`architecture.md`](architecture.md)

---

## 1. 核心思想：两层推断

5 万文件级工作区不可能对每次修改做全量分析。将类型推断分为两层，以 `DocumentSummary` 为界：

| 层 | 范围 | 时机 | 依赖 |
|----|------|------|------|
| **单文件推断** | 遍历单文件 AST，产出 `DocumentSummary` | 文件变更时立即执行 | **零跨文件依赖** |
| **跨文件解析** | 沿 Summary 中的符号引用桩链式追踪 | goto/hover 按需 / 诊断后台批量 | 其他文件的 Summary |

`DocumentSummary` 存储的是**类型事实的食谱（stub + 本地事实）**，不是**做好的菜（完全解析后的类型）**。解析是惰性的、按需的、可缓存的。

---

## 2. 数据模型

### 2.1 每文件摘要 `DocumentSummary`

每个 URI 对应一份 Summary，与版本/内容哈希绑定：

| 数据 | 说明 |
|------|------|
| `GlobalContributions` | 全局名贡献：名称、类属、定义区间 |
| `TypeDefinitions` | Emmy 类型定义（`@class`/`@enum`/`@alias`） |
| `FunctionSummary` | 参数、返回类型、关联 Emmy 注解；以 `FunctionSummaryId(u32)` 为 key 存储（对称于 `TableShapeId`），附带 `function_name_index`（name → ID）反查索引 |
| `TableShape` | 字段名 → 字段类型、注解、定义位置 |
| `CallSites` | 函数调用点信息 |

`TypeFact` 通过 `Known(FunctionRef(FunctionSummaryId))` 间接引用函数签名（同一文件内定义的函数），与 `Known(Table(TableShapeId))` 模式对称。Emmy 注解直接声明的函数类型仍使用内联 `Known(Function(FunctionSignature))` variant。

### 2.2 工作区聚合层

连接"单文件 Summary"和"跨文件查询"的桥梁：

```
┌──────────┐  ┌──────────┐  ┌──────────┐
│ file_a   │  │ file_b   │  │ file_c   │
│ Summary  │  │ Summary  │  │ Summary  │
└────┬─────┘  └────┬─────┘  └────┬─────┘
     │             │             │
     ▼             ▼             ▼
┌────────────────────────────────────────┐
│           工作区聚合层                   │
│  GlobalShard    树结构全局名索引        │
│  TypeShard      name → [候选定义…]      │
│  RequireByReturn  URI → [依赖方…]      │
│  TypeDependants   类型反向依赖          │
│  ResolutionCache  stub → resolved_type │
└────────────────────────────────────────┘
     │
     ▼
  LSP 请求（goto / hover / references / diagnostics）
```

| 分片 | 键 → 值 | 更新方式 |
|------|---------|---------|
| `GlobalShard` | 按 `.` / `:` 切段的树（`roots["UE4"].children["FVector"]`），叶/中间节点可挂 `[候选定义…]`；附带 URI→路径反向索引 | `push_candidate` 插入 / `remove_by_uri` 按 URI 精准删除 |
| `TypeShard` | 类型裸名 → [候选定义…] | 同上 |
| `RequireByReturn` | 目标 URI → [(来源文件, 局部名)…] | 绑定增删 |
| `TypeDependants` | 类型名 → [依赖文件…] | 类型变更时失效依赖方缓存 |

更新粒度是**名字级**（不是文件级）：`GlobalShard` 通过 URI 反向索引精准定位一个文件贡献的所有路径，增量更新只触及这些路径上的节点。

---

## 3. 单文件推断：Summary 生成

### 3.1 AST 遍历产出

| AST 节点类型 | 产出 |
|-------------|------|
| `G = expr` / `G.sub = expr` | `GlobalContributions` |
| `function foo(...) return expr end` | `FunctionSummary`：参数、返回类型、Emmy 注解 |
| `t.field = expr` / `t = { k = v }` | `TableShape` 字段 |
| `---@class` / `---@type` / ... | `TypeDefinitions` |

### 3.2 类型记录规则

能在单文件内确定的类型直接记录；需要跨文件的用**符号引用桩（symbolic stub）**占位：

| 直接记录 | 符号引用桩（延迟解析） |
|---------|---------------------|
| 字面量：string, number, … | require 返回值：`RequireRef("mod")` |
| 本文件内函数调用返回 | 跨文件函数调用：`CallReturn(ref, "func")` |
| table 字面值构造 | 全局变量引用：`GlobalRef("Mgr")` |
| Emmy 注解声明的类型 | Emmy 类型名引用：`TypeRef("PlayerData")` |

**关键**：整个 Summary 的生成不需要读取任何其他文件。

### 3.3 ScopeTree — 位置感知的局部类型

`build_file_analysis` 在单次 AST 遍历中同时产出 `DocumentSummary` 和 `ScopeTree`。`ScopeTree` 将局部变量的类型与其 Lua 词法作用域绑定：

| 概念 | 说明 |
|------|------|
| `ScopeDecl` | 单个声明：名称、类型（`type_fact`）、可见区间、是否 Emmy 注解 |
| `Scope` | 词法块（File / FunctionBody / Do / For / If / …），含父子关系和声明列表 |
| `ScopeTree` | 所有 Scope 组成的扁平数组 + 查询 API |

查询 API：
- `resolve_type(byte_offset, name)` → 在给定位置按 Lua 词法规则查找局部变量类型
- `resolve_decl(byte_offset, name)` → 返回完整 `ScopeDecl`
- `all_declarations()` → 遍历所有声明（供诊断和补全使用）

`build_file_analysis` 替代了之前独立的 `build_summary` + `build_scope_tree` 两阶段流程，避免重复 AST 遍历，且让 scope 声明直接携带类型信息。

---

## 4. 类型推断与 Table Shape

### 4.1 类型来源优先级

局部事实 → 函数返回摘要 / 模块 return 摘要 → Emmy 类型表 / 字段注解 → 结构推断 → `unknown`

### 4.2 Emmy 与 Table Shape 优先级

只要某个节点已绑定明确 Emmy 类型，就**完全切换到 Emmy 语义**；table shape 不再参与解析。

`---@class` 的 field 仅认以下来源：
- 注释声明的字段（`---@field`）
- 成员函数
- 可明确归属的 `self.name = expr`

### 4.3 全局 Table 合并

- `GlobalShard` 以树（trie）存储全局名：根节点对应顶层名（`UE4`、`print`），子节点按 `.` / `:` 切段（`UE4 → FVector → new`）；`.` 和 `:` 统一为同一个 `children` map（colon 只是 self 语法糖，由 `FunctionSignature.params` 区分）
- 每个节点挂 `Vec<GlobalCandidate>`（多文件贡献同一路径时保留多候选），按 URI 优先级排序
- 附带反向索引 `uri_to_paths: HashMap<Uri, Vec<String>>`，`remove_by_uri` 按文件精准删除贡献，O(贡献数)
- 子字段枚举（补全、hover）直接遍历 `node.children`，O(children) 而非 O(全局条目数)
- 全局链路某节点已绑定 Emmy 类型时，停止 GlobalTable 扩张，改走 Emmy 字段解析

---

## 5. 跨文件解析：链式追踪

### 5.1 解析流程

```
hover 在 p.name 上
  → 查本文件 Summary → p 的类型 = CallReturn(RequireRef("protocol"), "new_player")
  → 解析 RequireRef("protocol") → protocol.lua 的 Summary
  → 查 FunctionSummary["new_player"] → 返回类型
  → 在返回类型中查字段 "name" → string
  → 返回结果
```

每步都是索引查表（O(1) map lookup），整体 O(链长)，不需要重新解析 AST。

### 5.2 解析缓存

| 缓存键 | 失效条件 |
|--------|---------|
| `(RequireRef("protocol"), "new_player")` | protocol.lua Summary 变更 |
| `(GlobalRef("Mgr"), "create")` | 贡献 "Mgr.create" 的文件 Summary 变更 |

缓存失效采用**标记脏 + 惰性重算**。解析过程维护访问栈，检测到环路时返回 `unknown`。

---

## 6. 索引生命周期

### 6.1 冷启动：5 阶段流水线

在 `initialized` 中 `tokio::spawn` 后台执行，不阻塞 LSP 请求：

```
Phase 1: Scan → Phase 1.5: Module → Phase 2: Parse → Phase 3: Merge → Phase 4: Ready
```

| 阶段 | 做什么 | 持锁 |
|------|--------|------|
| **Scan** | 发现 .lua 文件列表 + 构建 module_entries | 无 |
| **Module Index** | 填充 module_index，此后 document_link 和 require 补全可用 | 短暂持 index 锁 |
| **Parse** | rayon 全量并行：read → tree-sitter parse → build_summary → build_scope_tree | **不持任何锁** |
| **Merge** | 原子 build_initial 构建全局索引（不清除 module_index） | 持 open_uris → documents → index |
| **Ready** | 设置 IndexState::Ready + seed 诊断队列 | 各锁短暂独立持有 |

**关键设计**：
- Parse 阶段不持锁，`did_open`/`did_change` 正常执行增量更新
- Module Index 仅依赖文件路径，不需要 parse，提前可用
- Merge 阶段 `build_initial` 先插入所有 summaries 再构建 shard，消除时序依赖

**索引状态机**：

| 状态 | 可用能力 |
|------|---------|
| `Initializing` | syntax-only 诊断，部分 goto/hover |
| `ModuleMapReady` | + document_link、require 补全 |
| `Ready` | 完整工作区语义 |

进度通过 `mylua/indexStatus` 通知上报（phase: scanning / module_map_ready / parsing / merging）。

### 6.2 编辑期增量更新

```
用户编辑 foo.lua
  → 重新解析 AST → 生成新 Summary S'
  → 与旧 Summary S 做 diff
    → GlobalContributions 变化 → 更新 GlobalShard
    → TypeTable 变化 → 更新 TypeShard
    → RequireBindings 变化 → 更新 RequireByReturn
    → 签名变化 → 标记依赖方解析缓存为脏
  → 用 S' 替换 S → 调度诊断
```

### 6.3 级联失效：签名指纹

为每个对外可见的类型事实计算**签名指纹**（参数类型 + 返回类型 + 字段集合 → 哈希）。

| 指纹变化？ | 动作 |
|-----------|------|
| **未变** | 仅更新本文件 Summary，**不触发级联** |
| **变了** | 沿反向依赖标记脏 |

实践中，函数内部逻辑修改大多不改变签名指纹，不会触发级联。

### 6.4 持久化缓存

- 默认纯内存索引（`memory` 模式）
- 可启用 `DocumentSummary` 级别磁盘缓存（`summary` 模式）
- 聚合层不缓存，冷启动 Phase 3 从 summaries 原子重建
- 失效维度：文件内容哈希、grammar/schema 版本、可执行文件 mtime

---

## 7. 消费模式

### goto/hover（低延迟，按需）

定位符号 → Summary 查类型 → 链式追踪 stub → 缓存命中直接返回 / 未命中惰性解析

绝大多数 < 毫秒级。

### 诊断（后台批量）

遍历文件所有表达式 → 完整类型解析 → 类型检查 → publishDiagnostics

与 goto/hover **共享同一套类型解析基础设施**，区别在于解析范围（全文件 vs 单表达式）和延迟要求（秒级 vs 毫秒级）。

诊断调度：300ms debounce，按优先级排队（当前可见 > 已打开 > 其余），支持取消。

---

## 8. 跨文件依赖的三种通道

| 通道 | 键类型 | 正向查询 | 反向查询 |
|------|--------|---------|---------|
| **require 绑定** | 模块路径字符串 | `require("x")` → 目标 URI → return 类型 | `RequireByReturn[URI]` → 依赖方 |
| **全局名引用** | 全局名字符串 | `GlobalShard["Mgr"]` → 候选定义 | 来源文件变更 → 引用方 |
| **Emmy 类型名** | 类型名字符串 | `TypeShard["PlayerData"]` → 类型定义 | 定义变更 → 引用方 |

所有通道的键都是**字符串**——使得 Summary 可以在不知道目标文件是否存在的情况下生成，实现单文件推断的完全独立性。
