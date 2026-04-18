# 索引架构落地实施计划（历史归档）

> **状态：已全部完成，本文档作为历史参考保留。**
>
> 步骤 1–7 所定义的：配置体系、核心数据模型、单文件 Summary 生成器、聚合层替换、跨文件链式解析、LSP 能力升级、索引生命周期与性能，**全部落地并通过集成测试**。
>
> - 当前实现状态与测试覆盖：见 [`../ai-readme.md`](../ai-readme.md)「LSP — Rust 语言服务器（阶段 C 完成）」章节
> - 设计约束（活文档）：[`index-architecture.md`](index-architecture.md)、[`lsp-semantic-spec.md`](lsp-semantic-spec.md)、[`requirements.md`](requirements.md)
>
> 本文以下内容保留为 **规划过程的完整记录**——为未来需要做类似量级改造时提供可参考的分步模板。若架构边界再次变动，请新建实施计划文档，不要在本文原地编辑。

---

## 0. 现状与差距总览

### 已实现（阶段 C 壳）

- Tree-sitter 解析 + 词法作用域（`scope.rs`：arena-based `ScopeTree`，支持所有块级作用域嵌套）
- 简化 `WorkspaceIndex`：`HashMap<String, Vec<GlobalEntry>>` + `HashMap<String, Uri>` require 映射
- 基础 LSP 能力表面：definition/hover/references/completion/rename/semantic tokens/diagnostics
- 工作区扫描：`initialized` 时全量递归，`didChangeWatchedFiles` 增量
- Emmy 注解：仅解析展示（`emmy.rs`），不驱动类型推断

### 未实现（index-architecture.md 核心）

| 缺失模块 | 影响 |
|----------|------|
| `DocumentSummary` 数据模型与生成管线 | 无法做摘要驱动的增量分析 |
| 类型系统（TypeFact / SymbolicStub） | hover/goto 无法展示推断类型 |
| 聚合层分片（GlobalShard / TypeShard / RequireByReturn） | 名字级增量更新不可用 |
| 跨文件链式解析 | `obj.pos.x` 类链式 hover/goto 不可用 |
| TableShape 建模 | 字段补全/字段诊断不可用 |
| FunctionSummary | 函数返回值类型推断不可用 |
| 签名指纹 + 级联失效 | 任何改动都需全量更新 |
| 解析缓存 | 无法避免重复计算 |
| 索引状态机（Initializing/Ready） | 用户无进度感知 |
| 配置体系（扩展 + LSP） | 无法配置 require 路径、诊断开关等 |
| 诊断调度（优先级、去抖、可取消） | 大工作区下可能卡死 |

---

## 1. 实施原则

1. **增量可验证**：每个步骤产出可编译、可测试的中间状态；不搞大爆炸重写。
2. **向后兼容**：新索引系统与旧路径并行运行，逐步切换消费方；切换前后 LSP 行为不退化。
3. **数据模型先行**：先定义 `DocumentSummary` 等核心类型（即使生成逻辑先用简化版），再渐进补全生成器和消费者。
4. **配置与索引松耦合**：配置体系可独立于索引架构先行落地。

---

## 2. 分步计划

### 步骤 1：配置体系（Extension + LSP）

**目标**：补齐 VSCode 扩展配置项，LSP 能接收并使用配置。

#### 1a. VSCode 扩展 `package.json` 补全

在 `contributes.configuration.properties` 中添加：

| 配置键 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `mylua.runtime.version` | `enum: ["5.3", "5.4"]` | `"5.3"` | Lua 版本假定 |
| `mylua.require.paths` | `array<string>` | `["?.lua", "?/init.lua"]` | 模块搜索路径模式 |
| `mylua.require.aliases` | `object` | `{}` | 路径别名映射，如 `{"@": "src/"}` |
| `mylua.workspace.include` | `array<string>` | `["**/*.lua"]` | 包含路径 glob |
| `mylua.workspace.exclude` | `array<string>` | `["**/.*", "**/node_modules"]` | 排除路径 glob |
| `mylua.workspace.indexMode` | `enum: ["merged", "isolated"]` | `"merged"` | 多根工作区合并策略 |
| `mylua.index.cacheMode` | `enum: ["summary", "memory"]` | `"summary"` | 索引持久化模式 |
| `mylua.diagnostics.enable` | `boolean` | `true` | 全局诊断开关 |
| `mylua.diagnostics.undefinedGlobal` | `enum: ["error", "warning", "hint", "off"]` | `"warning"` | 未定义全局 |
| `mylua.diagnostics.emmyTypeMismatch` | `enum` | `"error"` | Emmy 类型不匹配 |
| `mylua.diagnostics.emmyUnknownField` | `enum` | `"error"` | Emmy 未知字段 |
| `mylua.diagnostics.luaFieldError` | `enum` | `"error"` | Lua 路径高确定性错误 |
| `mylua.diagnostics.luaFieldWarning` | `enum` | `"warning"` | Lua 路径保守提示 |
| `mylua.gotoDefinition.strategy` | `enum: ["auto", "single", "list"]` | `"auto"` | goto 多候选策略 |
| `mylua.references.strategy` | `enum: ["best", "merge", "select"]` | `"best"` | references 多候选策略 |

#### 1b. Extension `extension.ts` 改造

- 将配置通过 `initializationOptions` 下发给 LSP。
- 监听 `onDidChangeConfiguration` 并发送 `workspace/didChangeConfiguration` 通知。

#### 1c. LSP 侧接收配置

- 新增 `config.rs` 模块：定义 `LspConfig` 结构体 + `serde` 反序列化。
- `initialize` 中从 `params.initialization_options` 读取配置。
- 实现 `did_change_configuration` handler。
- `Backend` 持有 `Mutex<LspConfig>`，各模块按需读取。

**验收**：修改 VSCode 配置 → LSP stderr 打印收到的配置值。

---

### 步骤 2：核心数据模型定义

**目标**：定义 `DocumentSummary` 及相关类型，编译通过即可；此步不改变运行时行为。

#### 新增 `summary.rs`

```rust
pub struct DocumentSummary {
    pub uri: Uri,
    pub content_hash: u64,
    pub require_bindings: Vec<RequireBinding>,
    pub global_contributions: Vec<GlobalContribution>,
    pub function_summaries: HashMap<String, FunctionSummary>,
    pub type_definitions: Vec<TypeDefinition>,
    pub local_type_facts: HashMap<String, TypeFact>,
    pub table_shapes: HashMap<TableShapeId, TableShape>,
    pub signature_fingerprint: u64,
}
```

#### 新增 `type_system.rs`

```rust
pub enum TypeFact {
    Known(KnownType),
    Stub(SymbolicStub),
    Union(Vec<TypeFact>),
    Unknown,
}

pub enum KnownType {
    Nil, Boolean, Number, Integer, String,
    Table(TableShapeId),
    Function(FunctionSignature),
    EmmyType(String),
}

pub enum SymbolicStub {
    RequireRef { module_path: String },
    CallReturn { base: Box<SymbolicStub>, func_name: String },
    GlobalRef { name: String },
    TypeRef { name: String },
    FieldOf { base: Box<TypeFact>, field: String },
}
```

#### 新增 `table_shape.rs`

```rust
pub struct TableShape {
    pub id: TableShapeId,
    pub fields: HashMap<String, FieldInfo>,
    pub array_element_type: Option<TypeFact>,
    pub is_closed: bool,
    pub truncated: bool,
}
```

#### 新增 `aggregation.rs`（聚合层接口）

```rust
pub struct WorkspaceAggregation {
    pub global_shard: HashMap<String, Vec<GlobalCandidate>>,
    pub type_shard: HashMap<String, Vec<TypeCandidate>>,
    pub require_by_return: HashMap<Uri, Vec<RequireDependant>>,
    pub resolution_cache: HashMap<CacheKey, ResolvedType>,
}
```

**验收**：`cargo build` 通过；类型定义与 `index-architecture.md` §2 对齐。

---

### 步骤 3：单文件 Summary 生成器（核心）

**目标**：遍历单文件 AST，产出 `DocumentSummary`；替代现有 `scan_globals`。

#### 3a. 基础骨架

- 新增 `summary_builder.rs`：`fn build_summary(uri: &Uri, tree: &Tree, source: &[u8]) -> DocumentSummary`。
- 遍历顶层语句，识别：
  - `local x = require("mod")` → `RequireBinding`
  - `local x = expr` → `LocalTypeFacts`（先支持字面量类型 + 简单 stub）
  - `function foo(...) ... end` → `FunctionSummary`
  - 顶层赋值 → `GlobalContribution`
  - `---@class` / `---@type` → `TypeDefinition`

#### 3b. 函数摘要提取

- 参数列表 + Emmy `@param` 注解 → 参数类型。
- 返回值：`@return` 注解优先；否则分析 `return` 语句产出 stub/已知类型。
- 生成签名指纹（参数类型 + 返回类型描述 → 哈希）。

#### 3c. Table Shape 提取

- `local t = {}` → 新建 shape。
- `t.field = expr` → 字段写入。
- 嵌套字段递归记录（最大深度限制 8）。
- Open/Closed 判定。

#### 3d. Emmy 类型提取

- 复用/重构现有 `emmy.rs` 的解析逻辑。
- `@class` → `TypeDefinition` + 字段列表。
- `@field` → 字段类型。
- `@type` → 变量/表达式类型绑定。

**验收**：对测试 Lua 文件生成 Summary 并序列化到 stderr，人工核对与 `index-architecture.md` §3.3 示例一致。

---

### 步骤 4：聚合层替换 WorkspaceIndex

**目标**：用新的 `WorkspaceAggregation` 替换现有 `WorkspaceIndex`；保持现有 LSP 行为不变。

#### 4a. 适配层

- `WorkspaceAggregation` 提供与旧 `WorkspaceIndex` 相同的查询接口：
  - `globals(name) -> Vec<GlobalEntry>`（从 `global_shard` 适配）
  - `require_map(module) -> Option<Uri>`（从 Summary 的 require_bindings 聚合）
- 现有 `goto.rs`、`hover.rs`、`diagnostics.rs` 等暂不改动，通过适配层消费新索引。

#### 4b. 增量更新

- `update_document`：生成新 Summary → 与旧 Summary diff → 名字级增量更新分片。
- `remove_document`：从各分片中移除该文件贡献。

#### 4c. 签名指纹与级联失效

- Summary diff 时比较 `signature_fingerprint`。
- 指纹变化 → 通过 `require_by_return` 反向索引标记依赖方的解析缓存为脏。
- 指纹不变 → 仅更新本文件 Summary，不触发级联。

**验收**：切换到新索引后，现有 goto/hover/references/completion/diagnostics 行为与之前一致（回归测试）。

---

### 步骤 5：跨文件链式解析引擎

**目标**：实现 stub 链式追踪，使 hover/goto 能展示推断类型。

#### 5a. 解析器核心

- 新增 `resolver.rs`：
  ```rust
  fn resolve_type(stub: &SymbolicStub, aggregation: &WorkspaceAggregation) -> ResolvedType
  ```
- 沿 stub 链逐步解析（`index-architecture.md` §5.1）。
- 维护访问栈，检测环路时返回 `Unknown`。
- 最大深度 32。

#### 5b. 解析缓存

- `resolution_cache: HashMap<CacheKey, ResolvedType>`。
- 缓存命中 → O(1) 返回。
- 缓存脏标记 → 惰性重算。

#### 5c. 表达式类型传播

- 链式字段访问：`obj.pos.x` → 逐段解析基础类型 → 查字段 → 下一段。
- 类型来源优先级：`局部事实 → 函数返回摘要 → Emmy 类型表 → 结构推断 → unknown`。

**验收**：`hover` 在 `local p = require("protocol").new_player(); p.name` 上返回 `string` 类型。

---

### 步骤 6：升级 LSP 能力消费

**目标**：各 LSP handler 切换到使用类型解析结果。

#### 6a. Hover 升级

- 链式字段 hover 展示推断类型。
- `require` 绑定 hover 展示模块返回类型摘要。
- 函数 hover 展示完整签名（参数类型 + 返回类型）。

#### 6b. Goto Definition 升级

- 链式字段跳转到字段定义位置。
- `require` 绑定上的字段跳转到目标文件的定义。
- 多候选策略配置化（`mylua.gotoDefinition.strategy`）。

#### 6c. References 升级

- 基于语义身份（`index-architecture.md` 的 `SymbolId`）查找引用，而非文本匹配。
- 全局引用使用 `GlobalShard` 反向查询。

#### 6d. Completion 升级

- `require` 返回值后的 `.` 触发字段补全。
- 全局 table 的字段补全。
- Emmy 类型的字段补全。

#### 6e. Diagnostics 升级

- Emmy 路径：类型不匹配 / 未知字段 → error。
- Lua 路径：closed shape 未知字段 → error；open shape → warning。
- 诊断 severity 可配置。

**验收**：在典型游戏 Lua 项目上，`hover`/`goto`/`completion` 展示跨文件推断类型。

---

### 步骤 7：索引生命周期与性能

**目标**：支持大工作区（5 万文件）的索引性能与用户体验。

#### 7a. 索引状态机

- `Initializing` → `Ready` 两状态。
- LSP 请求在 `Initializing` 阶段返回部分结果 + 进度通知。
- 使用 `window/workDoneProgress` 协议向客户端报告扫描进度。

#### 7b. 并行冷启动

- 使用 `rayon` 或 `tokio::spawn_blocking` 并行生成 Summary。
- 每份 Summary 产出后立即流式 merge 到聚合层。
- 按 500 文件一批报告进度。

#### 7c. 诊断调度

- 去抖（~300ms）。
- 优先级队列：当前可见文件 > 已打开文件 > 其余。
- 支持 `$/cancelRequest` 取消排队中的旧诊断任务。

#### 7d. 持久化缓存（可选，按配置）

- `mylua.index.cacheMode = "summary"` 时，Summary 序列化到磁盘。
- 缓存失效维度（`CacheMeta` 任一字段不匹配 → 整盘 wipe 重建）：
  - `schema_version`：常量，破坏性 `DocumentSummary` 结构变化时手动 bump。
  - `exe_mtime_ns`：当前 `mylua-lsp` 可执行文件 mtime（纳秒）。`cargo build` 重链接、extension 升级替换 binary 都会更新，开发与发版场景统一覆盖，无需手动 bump schema。
  - `config_fingerprint`：`require.paths` + `require.aliases` 的哈希。
  - 每文件另有 `content_hash` 二级门槛：meta 全匹配但单个 Lua 文件内容变了也会重建该文件 summary。
- **缓存位置**：`<workspace_root>/.vscode/.cache-mylua-lsp/`（与 `mylua-lsp.log` 同驻 `.vscode/`，统一编辑器状态目录）。
  - 随项目搬家/删除自动清理，避免 `~/.cache/` 下孤儿目录累积。
  - 首次 `save_all` 会在缓存目录内自动写入 `.gitignore`（内容 `*` + `!.gitignore`）防止误提交。
  - 双层自索引防护：默认 `workspace.exclude` 的 `**/.*` 已覆盖 `.vscode/` 整棵子树；外加 `workspace_scanner` 内置硬编码 `.vscode/.cache-mylua-lsp` 路径 exclude，用户完全 override 默认配置时仍然生效。

**验收**：5 万文件工作区冷启动 < 30 秒（视硬件）；编辑期增量更新 < 100ms。

---

## 3. 依赖图

```
步骤 1（配置）──────────────┐
                             ▼
步骤 2（数据模型）──→ 步骤 3（Summary 生成）──→ 步骤 4（聚合层替换）
                                                         │
                                              ┌──────────┤
                                              ▼          ▼
                                    步骤 5（解析引擎）  步骤 7a-c（性能）
                                              │
                                              ▼
                                    步骤 6（LSP 升级）
                                              │
                                              ▼
                                    步骤 7d（持久化缓存）
```

步骤 1 与步骤 2 可并行。步骤 3 依赖步骤 2。步骤 4 依赖步骤 3。步骤 5 依赖步骤 4。步骤 6 依赖步骤 5。步骤 7a-c 可与步骤 5 并行。步骤 7d 最后做。

---

## 4. 建议启动顺序

| 优先级 | 步骤 | 预估工作量 | 理由 |
|--------|------|-----------|------|
| **P0** | 步骤 1：配置体系 | 小 | 独立、低风险、对后续步骤有用、用户立即可感知 |
| **P0** | 步骤 2：数据模型 | 小 | 纯类型定义，编译通过即可，后续所有步骤依赖 |
| **P1** | 步骤 3：Summary 生成 | 大 | 核心工作量，决定索引质量 |
| **P1** | 步骤 4：聚合层替换 | 中 | 打通新旧世界的桥梁 |
| **P2** | 步骤 5：解析引擎 | 大 | 实际能力提升的关键 |
| **P2** | 步骤 6：LSP 升级 | 中 | 依赖步骤 5，逐能力推进 |
| **P3** | 步骤 7：性能与缓存 | 中 | 可在功能正确后再优化 |

**建议从步骤 1 + 步骤 2 并行开始。**

---

## 5. 需同步更新的文件

实施过程中需同步更新的文档与配置：

- `lsp/README.md`：更新模块结构与能力表。
- `vscode-extension/package.json`：配置项。
- `docs/implementation-roadmap.md`：阶段状态。
- `ai-readme.md`：当前实现状态。
