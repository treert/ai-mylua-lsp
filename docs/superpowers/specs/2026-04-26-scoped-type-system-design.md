# Scoped Type System Design

> 合并 ScopeTree 与 summary_builder，将 `local_type_facts` 从 `DocumentSummary` 移入带类型的 `ScopeTree`，并实现 class anchor binding 使 `function ABC:method()` 和 `self.field = X` 能将字段写回 `@class TypeDefinition`。

**日期**：2026-04-26
**关联文档**：`docs/index-architecture.md`、`docs/future-work.md`

---

## 1. 问题陈述

当前有两套独立系统遍历 AST，各管一半：

| 系统 | 构建 | 用途 | 作用域 |
|------|------|------|--------|
| `ScopeTree`（`scope.rs`） | 查询时从 AST 构建 | 定位声明位置 | 有层级 |
| `local_type_facts`（`DocumentSummary`） | build_summary 时构建 | 存类型信息 | 无——flat HashMap |

**问题 1：同名变量遮蔽错误。** `local_type_facts` 是 `HashMap<String, LocalTypeFact>`，同名 local 后写覆盖前写。不同作用域的同名变量取到错误的类型。

**问题 2：`local_type_facts` 不应暴露在 `DocumentSummary` 中。** 局部变量信息是文件内部的查询结构，不应参与跨文件聚合。当前有 2 处跨文件访问实际在做 class anchor → table shape 的跳板查找，可用更直接的机制替代。

**问题 3：缺少 class anchor binding。** `---@class ABCCls` + `local ABC = class()` 时，`function ABC:method()` 和 `self.field = X` 无法将字段写回 `ABCCls` 的 `TypeDefinition`。当前只处理 anchor 为 table literal（`local ABC = {}`）的情况。

**问题 4：两遍 AST 遍历浪费。** `build_scope_tree` 和 `build_summary` 各自完整遍历一次 AST。

---

## 2. 设计概览

分两个 Phase 实施：

- **Phase 1**：ScopeTree 带类型 + 合并遍历 + 消费方迁移 + 删除 `local_type_facts`
- **Phase 2**：Class anchor binding + 字段写回 `@class`

Phase 2 依赖 Phase 1 的基础设施（带类型的 ScopeTree 提供 `self` 类型查询）。

---

## 3. Phase 1：带类型的 ScopeTree

### 3.1 ScopeDecl 扩展

```rust
pub struct ScopeDecl {
    pub name: String,
    pub kind: DefKind,
    pub decl_byte: usize,
    pub visible_after_byte: usize,
    pub range: ByteRange,
    pub selection_range: ByteRange,
    // 新增
    pub type_fact: Option<TypeFact>,
    pub bound_class: Option<String>,  // Phase 2 使用，Phase 1 先加字段
}
```

`type_fact` 为 `Option`：某些声明（for 变量等）在注册时可能类型未知。

同一 scope 允许同名声明（Lua 语义）。不同声明通过 `visible_after_byte` 区分生效范围，已有的 `find_decl_in_scope` 正确处理此情况。

### 3.2 BuildContext 内嵌 scope stack

```rust
pub(crate) struct BuildContext<'a> {
    // ... 已有字段保留 ...

    // 新增：scope building
    pub(crate) scopes: Vec<Scope>,
    pub(crate) scope_stack: Vec<usize>,
}
```

封装方法：

- `push_scope(kind, start, end)` — 创建新 scope，设 parent 为栈顶，push 到栈
- `pop_scope()` — pop scope stack
- `current_scope_id() -> usize` — 栈顶
- `add_scoped_decl(ScopeDecl)` — 向当前 scope 添加声明
- `resolve_in_build_scopes(name) -> Option<&ScopeDecl>` — 沿 scope stack 从内到外查找，用于 build 阶段替代 `local_type_facts.get(name)`

### 3.3 遍历范围：覆盖函数体内部

当前 summary_builder 不深入函数体内部（只通过 `collect_return_types` 浅扫 return 语句）。合并后扩展为完整遍历：

```
visit_top_level(root)                          // push File scope
  ├── local_declaration         → add_scoped_decl + type_infer
  ├── local_function_declaration → add_scoped_decl + build_function_summary
  ├── function_declaration       → build_function_summary + global/shape
  ├── assignment_statement       → global contribution / shape write
  ├── emmy_comment              → type_definitions
  ├── if/for/do/while/repeat    → visit_nested_block (push block scope)
  │     └── 递归同上
  └── function_body 内部        → visit_function_body (新增)
        ├── push FunctionBody scope
        ├── 注册 parameters（Emmy + AST）
        ├── 注册 implicit self（冒号方法）
        ├── 递归遍历 body 内所有语句（同 visit_nested_block 的节点类型）
        │     包括嵌套的 function_declaration / local_function_declaration
        │     → 递归进入其 function_body（push/pop 新的 FunctionBody scope）
        └── pop FunctionBody scope
```

`collect_return_types` 不再单独调用，改为在遍历函数体时遇到 `return_statement` 顺便收集。

scope push/pop 对应的 AST 节点与当前 `scope.rs::TreeBuilder::visit_node` 完全一致：

| AST 节点 | ScopeKind |
|----------|-----------|
| root | File |
| function_body | FunctionBody |
| do_statement | DoBlock |
| while_statement | WhileBlock |
| repeat_statement | RepeatBlock |
| if_statement | IfThenBlock |
| elseif_clause | ElseIfBlock |
| else_clause | ElseBlock |
| for_numeric_statement | ForNumeric |
| for_generic_statement | ForGeneric |

### 3.4 local_type_facts 写入改为 scope 写入

当前：
```rust
ctx.local_type_facts.insert(name.clone(), LocalTypeFact { name, type_fact, source, range });
```

改为：
```rust
ctx.add_scoped_decl(ScopeDecl {
    name, kind, type_fact: Some(type_fact),
    decl_byte, visible_after_byte, range, selection_range,
    bound_class: None,
});
```

build 阶段查找变量（`type_infer.rs:101` 的 `ctx.local_type_facts.get(text)`）改为：
```rust
ctx.resolve_in_build_scopes(text).and_then(|decl| decl.type_fact.as_ref())
```

### 3.5 函数签名变更

```rust
pub fn build_file_analysis(
    uri: &Uri,
    tree: &tree_sitter::Tree,
    source: &[u8],
    line_index: &LineIndex,
) -> (DocumentSummary, ScopeTree)
```

替代 `build_summary`（返回 `DocumentSummary`）和 `build_scope_tree`（返回 `ScopeTree`）。

调用方（`lib.rs` × 3、`indexing.rs` × 2）统一改为调用 `build_file_analysis`。包括 cache hit 路径——丢弃返回的 summary 用 cached 的，保证 ScopeTree 始终带类型。

### 3.6 ScopeTree 新增查询 API

```rust
impl ScopeTree {
    // 已有
    pub fn resolve_decl(&self, byte_offset: usize, name: &str) -> Option<&ScopeDecl>;
    pub fn visible_locals(&self, byte_offset: usize) -> Vec<&ScopeDecl>;
    pub fn all_declarations(&self) -> impl Iterator<Item = &ScopeDecl>;

    // 新增
    pub fn resolve_type(&self, byte_offset: usize, name: &str) -> Option<&TypeFact>;
    pub fn resolve_bound_class(&self, byte_offset: usize, name: &str) -> Option<&str>;
}
```

### 3.7 消费方迁移

**同文件消费（10 处）→ 查 ScopeTree：**

| 文件 | 当前 | 迁移后 |
|------|------|--------|
| `hover.rs` | `local_type_facts[name]` | `scope_tree.resolve_type(byte_offset, name)` |
| `goto.rs` | `local_type_facts.get(local_name)` | `scope_tree.resolve_type(byte_offset, name)` |
| `completion.rs` | `local_type_facts.get(name)` | `scope_tree.resolve_type(byte_offset, name)` |
| `type_inference.rs` (×2) | `summary.local_type_facts.get(text)` | `scope_tree.resolve_type(byte_offset, text)` |
| `inlay_hint.rs` | `local_type_facts.get(name)` | `scope_tree.resolve_type(byte_offset, name)` |
| `diagnostics/type_compat.rs` (×2) | `local_type_facts.get(text)` | `scope_tree.resolve_type(byte_offset, text)` |
| `diagnostics/type_mismatch.rs` (×2) | `local_type_facts.values()` / `.get(name)` | `scope_tree.resolve_decl` + `.type_fact` |

消费方之前不需要 byte_offset（flat HashMap 按名字查），迁移后需要传入光标/节点的 `node.start_byte()`。

**跨文件消费（2 处 resolver.rs:819,973）→ TypeDefinition.anchor_shape_id：**

新增字段：
```rust
pub struct TypeDefinition {
    // ... 已有字段 ...
    #[serde(default)]
    pub anchor_shape_id: Option<TableShapeId>,
}
```

在 `flush_pending_class` 时，如果紧邻的 local 声明的 type_fact 是 `Table(shape_id)`，记录到 `TypeDefinition.anchor_shape_id`。resolver.rs 直接用 `td.anchor_shape_id` 查 shape。

**aggregation.rs（1 处）→ build 时提取：**

在 `build_file_analysis` 末尾顺便收集引用的类型名，存到 `DocumentSummary.referenced_type_names: HashSet<String>`。

### 3.8 FunctionRef hover 修复

当 `resolve_type` 返回 `FunctionRef(id)` 时，从同文件 `summary.function_summaries[id]` 取出签名格式化，替代当前的 `Display` 输出 `function<func_0>`。

### 3.9 删除

- `DocumentSummary.local_type_facts` 字段
- `LocalTypeFact` struct
- `TypeFactSource` enum
- `scope.rs` 中的 `TreeBuilder` 及 `build_scope_tree` 函数
- `BuildContext.local_type_facts` 字段

---

## 4. Phase 2：Class anchor binding + 字段写回

### 4.1 绑定记录在变量侧

class anchor 在变量侧记录，不在 TypeDefinition 侧：

- **局部变量**：`ScopeDecl.bound_class: Option<String>`
- **全局变量**：`BuildContext.global_class_bindings: HashMap<String, String>`

**`pending_class_name` 机制：**

`flush_pending_class` 把 `@class` 写入 `type_definitions` 后，暂存 class 名到 `ctx.pending_class_name: Option<String>`。

紧邻的下一条语句消费它：

- `visit_local_declaration`：`pending_class_name` 有值 → 第一个变量的 `ScopeDecl.bound_class = Some(class_name)`
- `visit_assignment`（简单标识符 LHS 如 `ABC = class()`）：→ `global_class_bindings.insert(name, class_name)`
- 其他语句 → `pending_class_name` 被丢弃

### 4.2 变量查找严格分层

```
resolve_in_build_scopes("ABC")
  ├── 找到 ScopeDecl → 是局部变量
  │     └── 读 decl.bound_class → 有就有，没有就没有，结束
  │
  └── 找不到 → 是全局变量
        └── 查 global_class_bindings["ABC"] → 有就有，没有就没有，结束
```

局部变量的 `bound_class` 为 `None` 时**不会**回退查全局绑定。这是 Lua 变量解析语义——scope 找到即为 local。

### 4.3 `function ABC:method()` 写回 @class

`visit_function_declaration` 中对 `function ABC:method()`：

1. `resolve_in_build_scopes("ABC")` → 找到 → 读 `decl.bound_class`
2. 未找到 → 查 `global_class_bindings["ABC"]`
3. 得到 `class_name` → 在 `type_definitions` 中找到该 class，追加 `TypeFieldDef`（`@field` 已声明同名则跳过）
4. 都查不到 → 退到现有逻辑（Table shape 写入或全局贡献）

### 4.4 `self` 类型注册

push FunctionBody scope 时，若外层是冒号方法声明（`function ABC:method()`）：

1. 查 ABC 的 `bound_class`（scope 或 `global_class_bindings`）
2. 有值 → 注册 `self` 到 FunctionBody scope：`type_fact = Some(EmmyType(class_name))`，`bound_class = Some(class_name)`
3. 无值 → 按现有逻辑（self 类型来自 ABC 的 `type_fact`，可能是 `Table(shape_id)`）

### 4.5 `self.field = XXX` 写回 @class

函数体内遇到 `self.field = expr`：

1. `resolve_in_build_scopes("self")` → 读 `bound_class`
2. 有 `class_name` → 在 `type_definitions` 中找到该 class，追加字段（`@field` 已声明同名则跳过）
3. 无 → 按现有 Table shape 逻辑

### 4.6 去重规则

追加字段到 `TypeDefinition.fields` 前检查：
- `@field` 已声明同名 → 跳过（Emmy 注解优先）
- 已被其他 `function X:other()` 或 `self.field = ...` 追加过同名 → 跳过

---

## 5. 风险与兜底

- **461 个测试**是安全网。每步都应 `cargo build && cargo test --tests`。
- **Phase 1 最大风险**：函数体内部完整遍历可能触发之前未覆盖的 type_infer 路径。兜底：新注册的 ScopeDecl 如果 type_infer 返回 Unknown，记 `type_fact = None`，不影响已有功能。
- **Phase 2 写回 @class** 只做追加，不修改 `@field` 已声明的字段，不破坏现有行为。
- **性能**：两遍遍历合为一遍，虽然新的一遍做的事更多（函数体内也做 type_infer），但省掉了 `build_scope_tree` 的完整遍历。整体应该更快或持平。
- **cache hit 路径**：统一走 `build_file_analysis`，丢弃返回的 summary 用 cached 的。保证逻辑统一。

---

## 6. 不包含在此次重构范围内

- `local_type_facts` 从 `DocumentSummary` 移除后，序列化格式变更需要兼容旧缓存（`#[serde(default)]` 已保护）。
- 进一步的 scope 增强（如 type narrowing / control flow analysis）不在此次范围。
- `TypeDefinition` 的 `fields` 从 `Vec<TypeFieldDef>` 改为 `HashMap` 不在此次范围（可作为后续优化，但不阻塞本次实施）。
