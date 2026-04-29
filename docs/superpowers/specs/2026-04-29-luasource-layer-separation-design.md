# LuaSource 独立数据层设计

> **日期**：2026-04-29
> **范围**：将 `LuaSource` 从 `Document` 中拆出为独立的常驻内存基础层，为后续 `Tree` / `ScopeTree` LRU 驱逐做铺垫。
> **不含**：LRU 驱逐逻辑本身（后续独立任务）。

---

## 1. 动机

当前 `Document` 同时持有 `LuaSource`、`tree_sitter::Tree`、`ScopeTree`，三者生命周期绑定。5 万文件时 RSS 约 1.5–3GB（源码文本 ~250MB，Tree ~500MB–1GB，ScopeTree ~100–300MB）。

目标：
1. **降低内存**：为后续 LRU 驱逐 Tree / ScopeTree 建立结构基础。
2. **分层清晰**：LuaSource 作为最基础的"内存文件库"，Document 和 Summary 建立在它之上。
3. **坐标转换保障**：当 Document 未来被 LRU 驱逐后，仍可通过 sources 层完成 ByteRange → LSP Range 转换。

---

## 2. 数据分层

```
┌──────────────────────────────────────────────────┐
│  sources: Mutex<HashMap<Uri, Arc<LuaSource>>>    │  ← 基础层，常驻内存
│  纯存储，独立锁，不参与任何锁顺序链                 │
└──────────────────────┬───────────────────────────┘
                       │ Arc<LuaSource> clone
                       ▼
        Document { lua_source: Arc<LuaSource>, tree, scope_tree }
                       │
                       │ 构建 Summary 时通过 Document 获取 source/line_index
                       ▼
                    Summary
```

**关键语义**：
- 每个 `Arc<LuaSource>` 构造后不可变（immutable snapshot）。
- `did_change` 时 sources 放入新的 Arc，旧的 Arc 仍被 Document/Summary 持有，引用计数归零后自然释放。
- sources 与 documents 之间通过 Arc 共享，**无锁耦合**。

---

## 3. 结构变更

### 3.1 `LuaSource`（`util.rs`）

不变。保留 `into_text()` 方法。

### 3.2 `Document`（`document.rs`）

```rust
// 改前
pub struct Document {
    pub lua_source: LuaSource,
    pub tree: tree_sitter::Tree,
    pub scope_tree: ScopeTree,
}

// 改后
pub struct Document {
    pub lua_source: Arc<LuaSource>,
    pub tree: tree_sitter::Tree,
    pub scope_tree: ScopeTree,
}
```

`source()`、`text()`、`line_index()` 方法签名不变，内部走 Arc deref。所有 handler 零改动。

### 3.3 `ParsedFile`（`lib.rs`）

```rust
// 改前
pub(crate) struct ParsedFile {
    pub(crate) uri: Uri,
    pub(crate) lua_source: LuaSource,
    ...
}

// 改后
pub(crate) struct ParsedFile {
    pub(crate) uri: Uri,
    pub(crate) lua_source: Arc<LuaSource>,
    ...
}
```

### 3.4 `Backend`（`lib.rs`）

```rust
pub struct Backend {
    // 新增
    pub(crate) sources: Arc<Mutex<HashMap<Uri, Arc<LuaSource>>>>,
    // 现有字段不变
    pub(crate) documents: Arc<Mutex<HashMap<Uri, Document>>>,
    ...
}
```

初始化时 `sources: Arc::new(Mutex::new(HashMap::new()))`。

---

## 4. 流程变更

### 4.1 `did_open`

```
did_open → new_arc = Arc::new(LuaSource::new(text))
         → sources.lock().insert(uri, new_arc.clone())
         → parse → documents.insert(Document { lua_source: new_arc, ... })
```

### 4.2 `did_change`

```
did_change → edit_lock 串行化（per-URI，现有机制）
           → docs.remove(&uri)
           → 从旧 Arc<LuaSource> clone text（额外一次 String clone）
           → 应用增量 edits 得到 final_text
           → new_arc = Arc::new(LuaSource::new(final_text))
           → sources.lock().insert(uri, new_arc.clone())
           → parse(source, Some(old_tree))
           → documents.insert(Document { lua_source: new_arc, tree, scope_tree })
```

**已知代价**：`did_change` 时多一次 `text.clone()`（平均 5KB），后续可考虑优化。

### 4.3 `did_close`

sources **不移除**（常驻语义）。现有逻辑不变。

### 4.4 `did_change_watched_files(DELETED)`

```
watched_files(DEL) → sources.lock().remove(&uri)
                   → documents.lock().remove(&uri)（现有逻辑）
```

### 4.5 冷启动（`indexing.rs`）

**Phase 2（rayon 并行 parse）**：
```rust
let lua_source = Arc::new(util::LuaSource::new(text));
// parse、build_file_analysis 不变，传入 lua_source.source()、lua_source.line_index()
Some(ParsedFile { uri, lua_source, tree, summary, scope_tree })
```

无额外 IO。原本就在 Phase 2 读文件构造 LuaSource，只是多了 Arc::new() 包装。

**Phase 3（merge）**：
```rust
let mut srcs = sources.lock().unwrap();
for pf in parsed {
    if open_held.contains(&pf.uri) { continue; }
    srcs.insert(pf.uri.clone(), pf.lua_source.clone());
    docs.insert(pf.uri, Document { lua_source: pf.lua_source, tree, scope_tree });
    summaries_to_merge.push(pf.summary);
}
drop(srcs);
```

sources 锁独立，可尽早释放。

### 4.6 `parse_and_store_with_old_tree`（`lib.rs`）

当前该函数接收 `text: String`，内部构造 `LuaSource::new(text)`。改为接收 `Arc<LuaSource>`（由调用方构造），同时写入 sources 和 documents。

---

## 5. 锁策略

sources 使用独立的 `Mutex`，**不参与**现有锁顺序链 `edit_locks → open_uris → documents → index`。

理由：sources 的使用模式是"lock → get/insert Arc → unlock"，不会在持有 sources 锁的同时去锁其他资源。Arc 的引用计数天然处理新旧版本共存。

---

## 6. 改动文件清单

| 文件 | 改动内容 |
|------|----------|
| `document.rs` | `lua_source` 类型改为 `Arc<LuaSource>` |
| `lib.rs` | Backend 新增 `sources` 字段；`ParsedFile.lua_source` 改为 `Arc<LuaSource>`；`parse_and_store_with_old_tree` 适配 |
| `handlers.rs` | `did_open` / `did_change` / `watched_files(DEL)` 中加入 sources 操作；`did_change` 中 `into_text()` 改为 clone |
| `indexing.rs` | Phase 2 构造 `Arc<LuaSource>`；Phase 3 merge 写入 sources |
| `util.rs` | 不变 |
| 其余 handler 文件 | 零改动（通过 `doc.source()` / `doc.line_index()` 访问，签名不变） |

---

## 7. 文档同步

| 文档 | 更新内容 |
|------|----------|
| `performance-analysis.md` §3 | "全内存驻留"描述改为"源码层常驻，分析层为将来 LRU 铺路" |
| `ai-readme.md` | 架构特性中补充 sources 层描述 |

---

## 8. 后续优化（不在本次范围）

- `did_change` 中 text clone 的优化（可考虑 `Arc::try_unwrap` 或 `Cow`）
- Tree / ScopeTree 的 LRU 驱逐（基于本次分层）
- Summary 中 ByteRange → LSP Range 转换直接从 sources 取（Document 驱逐后的 fallback）
