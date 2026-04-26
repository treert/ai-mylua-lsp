# Function Signature ID 化设计

> 对齐 `TableShapeId` 的间接引用模式，消除函数签名内联重复与按名字直接访问 `function_summaries` 的模式。
>
> 对应 `docs/future-work.md` §1.7。

---

## 1. 动机

当前 `TableShape` 通过 `TableShapeId(u32)` 唯一定位，`TypeFact::Known(Table(id))` 指向 `summary.table_shapes[id]`。但函数签名直接内联在 `TypeFact::Known(Function(FunctionSignature))` 中，导致：

- 同一函数被多处引用时签名数据重复存储
- 无法通过 TypeFact 回查函数的完整元信息（overload、Emmy 注解、定义位置）
- local function 赋值给 table field / Emmy class member 时，外部只能拿到内联签名副本，丢失与原始 FunctionSummary 的关联
- 消费方（hover、signature_help、completion、call_hierarchy、resolver）被迫用函数名字符串直接查 `function_summaries`，绕过了类型系统的间接引用

## 2. 设计决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| ID 作用域 | per-file（与 TableShapeId 对齐） | 无需全局协调，模式一致 |
| 存储方案 | 双 map：`HashMap<FunctionSummaryId, FunctionSummary>` + `HashMap<String, FunctionSummaryId>` | ID 是主键，名字是索引 |
| TypeFact variant | 并存 `Function(FunctionSignature)` + `FunctionRef(FunctionSummaryId)` | Emmy inline 函数类型无文件归属，不走 ID |
| name_index 范围 | 仅全局函数，colon 统一成 dot | local function 走 `local_type_facts → FunctionRef(id)` |
| 迁移策略 | 一次性完成 | 460+ 测试覆盖，避免半吐子状态 |

## 3. 数据模型

### 3.1 新增 `FunctionSummaryId`

```rust
// summary.rs（或独立文件，与 TableShapeId 对齐）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FunctionSummaryId(pub u32);
```

Per-file 唯一，summary 构建阶段递增分配。

### 3.2 `KnownType` 新增 variant

```rust
pub enum KnownType {
    Nil,
    Boolean,
    Number,
    Integer,
    String,
    Table(TableShapeId),
    Function(FunctionSignature),       // 保留：Emmy inline 函数类型
    FunctionRef(FunctionSummaryId),    // 新增：引用 summary 里的完整签名
    EmmyType(String),
    EmmyGeneric(String, Vec<TypeFact>),
}
```

- `Function(FunctionSignature)` — Emmy 注解直接声明的函数类型（`fun(a: string): number`），无文件归属，不走 ID
- `FunctionRef(FunctionSummaryId)` — 有定义位置的函数，通过 `source_uri + id` 查完整 `FunctionSummary`

### 3.3 `DocumentSummary` 变更

```rust
pub struct DocumentSummary {
    // 旧: pub function_summaries: HashMap<String, FunctionSummary>,
    
    /// ID → 完整 FunctionSummary（主存储）
    pub function_summaries: HashMap<FunctionSummaryId, FunctionSummary>,
    /// 函数名 → ID（仅全局函数，colon 统一成 dot）
    /// 例如 "Player.new" → id=0, "M.add" → id=1
    pub function_name_index: HashMap<String, FunctionSummaryId>,
    
    // ... 其余字段不变
}
```

### 3.4 `FunctionSummary` 新增 `id` 字段

```rust
pub struct FunctionSummary {
    pub id: FunctionSummaryId,          // 新增
    pub name: String,
    pub signature: FunctionSignature,
    pub range: ByteRange,
    pub signature_fingerprint: u64,
    pub emmy_annotated: bool,
    pub overloads: Vec<FunctionSignature>,
    pub generic_params: Vec<String>,
}
```

## 4. 生产侧（summary_builder）

### 4.1 ID 分配

`BuildContext` 新增：

```rust
pub(crate) next_function_id: u32,

pub(crate) fn alloc_function_id(&mut self) -> FunctionSummaryId {
    let id = FunctionSummaryId(self.next_function_id);
    self.next_function_id += 1;
    id
}
```

初始化 `next_function_id: 0`。

### 4.2 `visit_local_function` 改造

```rust
fn visit_local_function(ctx: &mut BuildContext, node: Node) {
    let name = ...;
    let id = ctx.alloc_function_id();
    let fs = build_function_summary(ctx, &name, node, body, id);
    
    // 主存储
    ctx.function_summaries.insert(id, fs);
    // 不写 function_name_index（local 函数不是全局的）
    
    // 写入 local_type_facts，让消费端走 type_inference 路径
    ctx.local_type_facts.insert(name.clone(), LocalTypeFact {
        name: name.clone(),
        type_fact: TypeFact::Known(KnownType::FunctionRef(id)),
        source: TypeFactSource::Assignment,
        range: ctx.line_index.ts_node_to_byte_range(node, ctx.source),
    });
}
```

### 4.3 `visit_function_declaration` 改造

```rust
fn visit_function_declaration(ctx: &mut BuildContext, node: Node) {
    let name = ...;
    let id = ctx.alloc_function_id();
    let fs = build_function_summary(ctx, &name, node, body, id);
    let sig_for_global = fs.signature.clone();
    
    // 主存储
    ctx.function_summaries.insert(id, fs);
    
    // ... wrote_to_shape 逻辑 ...
    // table shape field 的 type_fact 改为 FunctionRef(id)
    
    if !wrote_to_shape {
        // 全局函数：写入 name_index（colon→dot）
        let normalized = name.replace(':', ".");
        ctx.function_name_index.insert(normalized, id);
        
        // GlobalContribution 也用 FunctionRef(id)
        ctx.global_contributions.push(GlobalContribution {
            name: name.clone(),
            kind: GlobalContributionKind::Function,
            type_fact: TypeFact::Known(KnownType::FunctionRef(id)),
            range: ...,
            selection_range: ...,
        });
    }
}
```

### 4.4 `build_function_summary` 接收 `id` 参数

```rust
fn build_function_summary(
    ctx: &mut BuildContext,
    name: &str,
    decl_node: Node,
    body: Option<Node>,
    id: FunctionSummaryId,       // 新增
) -> FunctionSummary {
    // ... 现有逻辑不变 ...
    FunctionSummary {
        id,                       // 新增
        name: name.to_string(),
        signature,
        range,
        signature_fingerprint,
        emmy_annotated,
        overloads,
        generic_params,
    }
}
```

### 4.5 summary_builder 内部的函数查找

`type_infer.rs` 和 `visitors.rs` 中 build 阶段内的 `ctx.function_summaries.get(name)` 调用需要改造。由于 build 阶段 `function_summaries` key 已经是 ID，这些地方需要：

- 新增 `BuildContext` 内部的 `function_id_by_name: HashMap<String, FunctionSummaryId>`（build 阶段临时反查表，包含 local + global，保留原始名字含 colon）
- build 完成后不导出到 DocumentSummary

## 5. 消费侧改造

### 5.1 全局函数查找（跨文件 qualified name）

所有消费端从：
```rust
summary.function_summaries.get(name)
```
改为：
```rust
let normalized = name.replace(':', ".");
summary.function_name_index.get(&normalized)
    .and_then(|id| summary.function_summaries.get(id))
```

涉及文件与位置：

| 文件 | 行（约） | 场景 |
|------|----------|------|
| `resolver.rs` | 515, 522 | `resolve_call_return` 3-tier fallback |
| `resolver.rs` | 1063-1064, 1072 | `resolve_method_return_with_generics` |
| `hover.rs` | 224 | overload 查找 |
| `completion.rs` | 440 | completion detail |
| `signature_help.rs` | 130, 187, 274 | 跨文件签名查找 |
| `call_hierarchy.rs` | 251, 334, 364 | caller/callee item 构建 |
| `type_inference.rs` | 283 | 同文件全局函数返回类型 |

注意 colon 统一成 dot 后，原来分别查 `"Player:new"` 和 `"Player.new"` 的逻辑简化为只查 `"Player.new"`。

### 5.2 Local function 查找路径改造

消费端不再直接查 `function_summaries.get(bare_name)`，而是走 type_inference 路径：

**`signature_help.rs:111-115`**（删除 local function 快速路径）：
```rust
// 旧：先查 function_summaries.get(&name)，再走 type_inference
// 新：统一走 type_inference，resolve 后通过 FunctionRef(id) 拿到 FunctionSummary
```

具体：`infer_node_type` → `local_type_facts[name]` → `TypeFact::Known(FunctionRef(id))` → resolver 通过 `(uri, id)` 查 `FunctionSummary`，拿到完整签名 + overloads。

**`call_hierarchy.rs:70`**：类似改造。

### 5.3 `FunctionRef(id)` 的 resolver 支持

resolver.rs 中凡是 `match KnownType::Function(sig)` 的地方，新增 `KnownType::FunctionRef(id)` 分支：

```rust
TypeFact::Known(KnownType::FunctionRef(id)) => {
    // 需要 source_uri 上下文（与 TableShapeId 对称）
    if let Some(uri) = source_uri {
        if let Some(summary) = agg.summaries.get(uri) {
            if let Some(fs) = summary.function_summaries.get(&id) {
                // 使用 fs.signature, fs.overloads, etc.
            }
        }
    }
}
```

涉及位置：
- `resolve_recursive` 中 `CallReturn` 分支取返回类型
- `resolve_field_chain_inner` 中函数返回值解析
- `collect_fields` 中 `is_function` 判断
- `substitute_in_fact` 泛型替换

### 5.4 纯 TypeFact 变换函数

`substitute_self`、`substitute_in_fact` 等：`FunctionRef(id)` 直接透传（clone），因为：
- `substitute_self` 在 summary_builder 阶段调用，此时函数用的是 `Function(sig)` 而非 `FunctionRef`
- `substitute_in_fact` 是泛型替换，`FunctionRef` 指向的签名在 summary 里不随泛型实参变化

### 5.5 Display

```rust
impl fmt::Display for KnownType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            // ...
            Self::FunctionRef(id) => write!(f, "function<{}>", id.0),
            // ...
        }
    }
}
```

### 5.6 其他

- `aggregation.rs:860`：`collect_type_names` 遍历 `function_summaries.values()` — 不变（只是 key 从 String 变成 ID）
- `fingerprint.rs:78-84`：指纹计算改为遍历 `function_summaries` 的 values，按 `fs.name` 排序
- `bin/lua_perf.rs:86`：`.len()` 调用不变
- `diagnostics/type_compat.rs`：`KnownType::Function(_)` 的匹配需新增 `KnownType::FunctionRef(_)` 分支

## 6. 序列化兼容

bump `CACHE_SCHEMA_VERSION`，旧缓存自动失效重建。

## 7. 验收标准

1. `local function helper(); M.process = helper`，外部 hover `obj.process` 显示 helper 的完整签名和 overload
2. `function_summaries` key 为 `FunctionSummaryId`，无名字冲突
3. 消费方不再按名字直接查 `function_summaries`
4. 全部 460+ 测试通过，零新增 warning
5. `function_name_index` 仅含全局函数，colon 统一为 dot
