# AST LRU 缓存设计

> **状态**：设计完成，暂不实现。等 references 反向索引（future-work 1.4）落地后再决定是否推进。
>
> **对应需求**：`future-work.md` §3.1

---

## 1. 背景

2 万文件工作区，源码总量 200 多 MB，冷启动后 RSS 约 6.5GB。当前 `Document` 同时持有 `LuaSource`（文本 + 行号索引）、`tree_sitter::Tree`（AST）、`ScopeTree`（作用域树），全部常驻内存。

大部分文件的 AST 在冷启动后不再被访问，但仍占用大量内存（tree 约为源码大小的 3~8 倍）。

## 2. 核心思路

**LuaSource 常驻，AST 按需缓存。** `Document` 保持单一结构，`tree` / `scope_tree` 改为 `Option`，可被 LRU 淘汰后置 `None`。外层 `DocumentStore` 管理 LRU 淘汰和 cache miss 恢复。

### 2.1 Document 结构

```rust
pub struct Document {
    pub lua_source: LuaSource,              // 始终有，不过期
    pub tree: Option<tree_sitter::Tree>,    // 可被淘汰
    pub scope_tree: Option<ScopeTree>,      // 跟 tree 同生同灭
    pub is_slow: bool,                      // 冷启动解析慢，不淘汰
    pub is_open: bool,                      // 编辑器打开中，不淘汰
}

impl Document {
    /// AST 是否被钉住（不可淘汰）
    fn is_pinned(&self) -> bool {
        self.is_slow || self.is_open
    }

    /// AST 是否在缓存中
    fn has_ast(&self) -> bool {
        self.tree.is_some()
    }
}
```

### 2.2 DocumentStore 结构

```rust
pub struct DocumentStore {
    documents: HashMap<Uri, Document>,
    /// LRU 访问顺序跟踪，只含非 pinned 且 has_ast 的 URI。
    /// front = 最久未使用，back = 最近使用。
    lru_tracker: VecDeque<Uri>,
    /// 非 pinned 文件的缓存容量：-1 = 不淘汰, 0 = 仅 pinned, >0 = LRU 缓存 N 个
    capacity: i32,
    /// 冷启动解析耗时超过此阈值(ms)的文件标记 is_slow。0 = 禁用
    slow_parse_threshold_ms: u64,
    /// cache miss 时用于重新解析
    parser: tree_sitter::Parser,
}
```

**设计优势**（对比之前的拆分两 HashMap 方案）：
- `HashMap<Uri, Document>` 签名基本不动，消费端改动极小
- pin 信息是 Document 自身属性，不需要外部 HashSet 同步
- 一个 HashMap 一个 Document，插入删除天然一致

## 3. Pin 语义

`is_slow` 和 `is_open` 两个属性独立管理，互不干扰：

| 场景 | is_slow | is_open | 进 tracker | 是否淘汰 |
|------|:-------:|:-------:|:----------:|:--------:|
| 慢文件未打开 | ✓ | ✗ | ✗ | 不淘汰 |
| 慢文件打开中 | ✓ | ✓ | ✗ | 不淘汰 |
| 慢文件关闭 | ✓ | ✗ | ✗ | 仍不淘汰 |
| 普通文件打开中 | ✗ | ✓ | ✗ | 不淘汰 |
| 普通文件关闭 | ✗ | ✗ | ✓ | 可被淘汰 |

## 4. LRU tracker 状态迁移

`lru_tracker` 是 `VecDeque<Uri>`，**只跟踪非 pinned 且 AST 在缓存中的文件**。pinned 文件不进 tracker。

| 事件 | tracker 操作 |
|------|-------------|
| 冷启动 insert（非 slow） | push_back |
| 冷启动 insert（slow） | 不进 tracker |
| did_open | 从 tracker 移除（如果在的话） |
| did_close（非 slow） | push_back（插尾部，刚关闭仍算热数据） |
| did_close（slow） | 不进 tracker |
| get_ast cache hit（非 pinned） | 移到尾部 |
| get_ast cache miss 重建（非 pinned） | push_back |
| 文件删除 | 从 tracker 移除 + document 删除 |
| 淘汰触发 | pop_front，设 `tree=None, scope_tree=None` |

## 5. LRU 淘汰策略

pinned 文件不占 capacity 配额。tracker 里只有可淘汰文件，直接 pop front：

```
insert / get_ast 后：
  如果 capacity > 0 且 lru_tracker.len() > capacity：
    while lru_tracker.len() > capacity:
      uri = lru_tracker.pop_front()
      documents[uri].tree = None
      documents[uri].scope_tree = None
```

因为 tracker 不含 pinned 文件，淘汰时无需检查 pin 状态。

| capacity | 行为 |
|----------|------|
| `-1` | 全部常驻（等同当前行为） |
| `0` | 仅 pinned 文件保留 AST |
| `200` | pinned 文件 + 最多 200 个非 pinned 文件 |

## 6. 核心 API

| 方法 | 用途 |
|------|------|
| `get(&self, uri) → Option<&Document>` | 拿 Document（lua_source 始终有，tree 可能 None） |
| `get_with_ast(&mut self, uri) → Option<&Document>` | 保证 tree/scope_tree 可用，miss 时自动重建 |
| `parse_temp(&mut self, uri) → Option<Tree>` | 只做 tree-sitter parse，**不**写回 Document、**不**进 tracker。给 references/rename 全量扫描用 |
| `insert(uri, lua_source, tree, scope_tree, parse_ms)` | 冷启动/编辑时写入 |
| `remove(uri)` | 文件删除 |
| `set_open(uri, bool)` | did_open / did_close 切换 |
| `iter(&self)` | 遍历所有 documents（兼容现有 `all_docs` 参数） |

### 消费端改动最小化

```rust
// 之前
let doc = docs.get(uri)?;
doc.tree.root_node()  // 直接用

// 之后 —— 大多数 handler
let doc = store.get_with_ast(uri)?;
doc.tree.as_ref()?.root_node()  // 多一层 Option

// references 全量扫描
for (uri, doc) in store.iter() {
    if !doc.lua_source.text().contains(name) { continue; }
    let tree = store.parse_temp(uri)?;  // 临时 parse
    // ...
}
```

## 7. 配置项

在 `IndexConfig` 下新增：

```rust
pub struct IndexConfig {
    pub cache_mode: CacheMode,
    /// AST 缓存容量。默认 200
    pub ast_cache_capacity: i32,        // "astCacheCapacity"
    /// 慢文件阈值(ms)。默认 500
    pub slow_parse_threshold_ms: u64,   // "slowParseThresholdMs"
}
```

VS Code settings.json：

```json
{
  "mylua.index.astCacheCapacity": 200,
  "mylua.index.slowParseThresholdMs": 500
}
```

## 8. 消费路径改造

### 8.1 单文件访问（hover、completion、diagnostics 等）

`docs.get(uri)` → `store.get_with_ast(uri)`，cache miss 自动重建。

### 8.2 全量扫描（references / rename）

文本预筛 + 按需临时解析：

```
// 1. 文本预筛（lua_source 常驻）
for (uri, doc) in store.iter() {
    if !doc.lua_source.text().contains(name) { continue; }
    // 2. 命中的文件按需 parse（不进 LRU）
    let tree = store.parse_temp(uri)?;
    // ... AST 扫描 ...
}
```

### 8.3 冷启动 merge

```
store.insert(uri, lua_source, tree, scope_tree, elapsed_ms);
// insert 内部：
//   elapsed_ms > threshold → doc.is_slow = true
//   非 slow → lru_tracker.push_back(uri)
```

### 8.4 did_open / did_close

```
// did_open
store.set_open(uri, true);   // doc.is_open = true, 从 tracker 移除

// did_close
store.set_open(uri, false);  // doc.is_open = false
                              // 非 slow → tracker.push_back(uri)
```

### 8.5 锁粒度

`Arc<Mutex<HashMap<Uri, Document>>>` → `Arc<Mutex<DocumentStore>>`，粒度不变。

## 9. 已知局限

- **references/rename 全量扫描**：当前不区分 global 和同名 local，是已有缺陷（future-work 1.7），不在本次解决。
- **全量扫描与 LRU 的矛盾**：references 触发时需要对大量文件做 `parse_temp`，削弱了 LRU 的内存收益。等 future-work 1.4（summary 记录引用反向索引）落地后，references 可缩小扫描范围，LRU 收益才能完全发挥。
- **parse_temp 代价**：tree-sitter parse 很快（大多数文件 < 1ms），但 2 万文件全量 parse_temp 仍需数秒。文本预筛可将实际 parse 数量降低一个数量级。
- **VecDeque 更新 O(n)**：`get_with_ast` cache hit 时需要在 VecDeque 中找到 URI 并移到尾部，O(n)。对于 capacity=200 的规模，n 很小，不构成瓶颈。若后续需要优化可换 `IndexMap` 或侵入式链表。

## 10. 内存收益估算

基于 2 万文件、200MB 源码、6.5GB RSS：

- LuaSource 常驻：~250MB
- 可淘汰的 tree + scope_tree：~1.2~1.8GB
- capacity=200 时（保留 pinned + 200 个非 pinned 文件 AST），预估释放 **~1.2~1.8GB**
- 预估 RSS 降至 **~4.7~5.3GB**

## 11. 暂不实现的原因

references/rename 的全量 AST 扫描依赖意味着 LRU 淘汰的文件可能频繁被 `parse_temp` 重建。在 summary 层没有引用反向索引之前，LRU 的收益受限。建议的推进顺序：

1. **先做 future-work 1.4**（collect_affected_names 扩展 + 引用反向索引）
2. 有了反向索引后，references/rename 可缩小扫描范围
3. 此时再实现 LRU，收益最大化
