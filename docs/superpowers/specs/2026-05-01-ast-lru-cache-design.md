# AST LRU 缓存设计

> **状态**：设计完成，暂不实现。等 references 反向索引（future-work 1.4）落地后再决定是否推进。
>
> **对应需求**：`future-work.md` §3.1

---

## 1. 背景

2 万文件工作区，源码总量 200 多 MB，冷启动后 RSS 约 6.5GB。当前 `Document` 同时持有 `LuaSource`（文本 + 行号索引）、`tree_sitter::Tree`（AST）、`ScopeTree`（作用域树），全部常驻内存。

大部分文件的 AST 在冷启动后不再被访问，但仍占用大量内存（tree 约为源码大小的 3~8 倍）。

## 2. 核心思路

**LuaSource 常驻，AST 按需缓存。**

将现有 `HashMap<Uri, Document>` 拆成两层存储，引入 `DocumentStore` 封装：

```rust
pub struct DocumentStore {
    /// 文本 + 行号索引，常驻内存，不过期
    sources: HashMap<Uri, LuaSource>,
    /// AST + 作用域树，LRU 淘汰
    ast_cache: LinkedHashMap<Uri, AstEntry>,

    /// 冷启动时解析慢的文件，自动 pin，仅文件删除时清除
    slow_pinned: HashSet<Uri>,
    /// 编辑器中打开的文件，did_open 加入，did_close 移除
    open_pinned: HashSet<Uri>,

    /// 非 pinned 文件的缓存容量：-1 = 不淘汰, 0 = 仅 pinned, >0 = LRU 缓存 N 个
    capacity: i32,
    /// 冷启动解析耗时超过此阈值(ms)的文件自动 pin。0 = 禁用
    slow_parse_threshold_ms: u64,
    /// cache miss 时用于重新解析
    parser: tree_sitter::Parser,
}

struct AstEntry {
    tree: tree_sitter::Tree,
    scope_tree: ScopeTree,
}
```

## 3. 双 pin 集合

两个 pin 集合独立管理，互不干扰：

| 场景 | slow_pinned | open_pinned | 是否淘汰 |
|------|:-----------:|:-----------:|:--------:|
| 慢文件未打开 | ✓ | ✗ | 不淘汰 |
| 慢文件打开中 | ✓ | ✓ | 不淘汰 |
| 慢文件关闭 | ✓ | ✗ | 仍不淘汰 |
| 普通文件打开中 | ✗ | ✓ | 不淘汰 |
| 普通文件关闭 | ✗ | ✗ | 可被淘汰 |

## 4. LRU 淘汰策略

pinned 文件不占 capacity 配额，只淘汰非 pinned 的后台文件：

```
insert / get_ast 时：
  pinned_count = (slow_pinned ∪ open_pinned) 中在 ast_cache 里的数量
  unpinned_count = ast_cache.len() - pinned_count

  如果 capacity > 0 且 unpinned_count > capacity：
    从 LinkedHashMap 头部（最久未使用）开始扫：
      - pinned → 跳过
      - 否则 → 移除 AstEntry
    直到 unpinned_count <= capacity
```

| capacity | 实际缓存 |
|----------|---------|
| `-1` | 全部常驻（等同当前行为） |
| `0` | 仅 pinned 文件保留 |
| `200` | pinned + 最多 200 个非 pinned |

## 5. 核心 API

| 方法 | 用途 |
|------|------|
| `get_source(&self, uri) → Option<&LuaSource>` | 拿文本，永远有 |
| `get_ast(&mut self, uri) → Option<&AstEntry>` | 查 LRU 缓存，miss 时从 sources 重新 parse + build_scope_tree |
| `parse_temp(&mut self, uri) → Option<Tree>` | 只做 tree-sitter parse，**不**进缓存、**不**建 scope_tree。给 references/rename 全量扫描用 |
| `insert(uri, lua_source, tree, scope_tree, parse_ms)` | 冷启动/编辑时写入 |
| `remove(uri)` | 文件删除，清理 sources + ast_cache + 两个 pin 集合 |

## 6. 配置项

在 `IndexConfig` 下新增：

```rust
pub struct IndexConfig {
    pub cache_mode: CacheMode,
    /// AST 缓存容量。默认 200
    pub ast_cache_capacity: i32,      // "astCacheCapacity"
    /// 慢文件阈值(ms)。默认 500
    pub slow_parse_threshold_ms: u64, // "slowParseThresholdMs"
}
```

VS Code settings.json：

```json
{
  "mylua.index.astCacheCapacity": 200,
  "mylua.index.slowParseThresholdMs": 500
}
```

## 7. 消费路径改造

### 7.1 单文件访问（hover、completion、diagnostics 等）

```
之前: let doc = docs.get(uri)?;
之后: let source = store.get_source(uri)?;
      let ast = store.get_ast(uri)?;
```

跨文件的 hover（`all_docs.get(&candidate.source_uri)`）同理。

### 7.2 全量扫描（references / rename）

文本预筛 + 按需临时解析：

```
// 1. LuaSource 常驻，文本预筛
let candidates: Vec<Uri> = store.sources_iter()
    .filter(|(_, src)| src.text().contains(name))
    .map(|(uri, _)| uri.clone())
    .collect();

// 2. 命中文件按需 parse（不进 LRU）
for uri in &candidates {
    let source = store.get_source(uri).unwrap();
    let tree = store.parse_temp(uri).unwrap();
    // ... AST 扫描 ...
}
```

### 7.3 冷启动 merge

```
store.insert(uri, lua_source, tree, scope_tree, elapsed_ms);
// insert 内部：sources 写入，ast_cache 写入，
// elapsed_ms > threshold 时加入 slow_pinned
```

### 7.4 锁粒度

`Arc<Mutex<HashMap<Uri, Document>>>` → `Arc<Mutex<DocumentStore>>`，粒度不变。

## 8. 已知局限

- **references/rename 全量扫描**：当前不区分 global 和同名 local，是已有缺陷，不在本次解决。
- **全量扫描与 LRU 的矛盾**：references 触发时需要对大量文件做 `parse_temp`，削弱了 LRU 的内存收益。等 future-work 1.4（summary 记录引用反向索引）落地后，references 可缩小扫描范围，LRU 收益才能完全发挥。
- **parse_temp 代价**：tree-sitter parse 很快（大多数文件 < 1ms），但 2 万文件全量 parse_temp 仍需数秒。文本预筛可将实际 parse 数量降低一个数量级。

## 9. 内存收益估算

基于 2 万文件、200MB 源码、6.5GB RSS：

- LuaSource 常驻：~250MB
- 可淘汰的 tree + scope_tree：~1.2~1.8GB
- capacity=200 时（保留 ~300 个文件 AST），预估释放 **~1.2~1.8GB**
- 预估 RSS 降至 **~4.7~5.3GB**

## 10. 暂不实现的原因

references/rename 的全量 AST 扫描依赖意味着 LRU 淘汰的文件可能频繁被 `parse_temp` 重建。在 summary 层没有引用反向索引之前，LRU 的收益受限。建议的推进顺序：

1. **先做 future-work 1.4**（collect_affected_names 扩展 + 引用反向索引）
2. 有了反向索引后，references/rename 可缩小扫描范围
3. 此时再实现 LRU，收益最大化
