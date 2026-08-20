# Tree-sitter 节点内存剖析

> 基于 tree-sitter **0.26.8** 源码（`src/subtree.h`、`src/length.h`、`src/node.c`）逐字段分析，
> 结合 `lua-perf --mem` 对本仓库文法（tree-sitter-mylua）的实测数据。
> 结论适用于 64 位平台。

---

## TL;DR

- tree-sitter 的"节点"（`Subtree`）不是统一大小的结构，而是 **tagged union**，三种形态：
  - **inline 叶子**：8 字节，无独立分配（住在父节点的 children 数组里）
  - **堆分配父节点**：80 字节头 + 子槽位数组，一次 malloc
  - **外部 token**：80 字节头，无子槽位
- **节点不存储任何文本**。变量名、关键字、字符串内容一个字节都不复制，
  节点里只有结构、相对偏移和文法符号 id；文本永远只有源码那一份数据。
- 本仓库实测：**88.9 – 154.0 B/可见节点**，工作数字 **~100 B**。
- 拉高平均值的是两个因素：① 每个独立分配节点实付 ~96–112 B；
  ② **隐藏规则节点不计入 `descendant_count` 但占足内存**（本文法有 29 个隐藏规则）。

---

## 1. 三种节点形态

`Subtree` 的定义是一个 union，用指针最低位做 tag：

```c
typedef union {
  SubtreeInlineData data;        // 最低位 = 1 → inline 叶子
  const SubtreeHeapData *ptr;    // 最低位 = 0 → 堆分配（父节点 / 外部 token / 错误）
} Subtree;
```

选哪种形态由 tree-sitter 在建树时决定：普通小叶子（非外部 token、非错误）
能塞进 8 字节就 inline；父节点、外部 token、错误节点一律堆分配。

---

## 2. 形态一：inline 叶子（8 字节）

小端序下的字段布局（`SubtreeInlineData`）：

| 字段 | 类型 / 位域 | 大小 | 含义 |
|------|------------|------|------|
| `is_inline` | 1 bit | ┐ | tagged-union 标记，恒为 1 |
| `visible` | 1 bit | │ 1 B | 是否出现在节点遍历中 |
| `named` | 1 bit | │ | 是否具名节点（`identifier` 等） |
| `extra` | 1 bit | │ | 是否注释等 extra 节点 |
| `has_changes` | 1 bit | │ | 增量重解析脏标记 |
| `is_missing` | 1 bit | │ | 错误恢复：缺失节点 |
| `is_keyword` | 1 bit | ┘ | 关键字标记 |
| `symbol` | `uint8_t` | 1 B | **文法符号 id**（不是文本！） |
| `parse_state` | `uint16_t` | 2 B | 产生该节点时的解析器状态 |
| `padding_columns` | `uint8_t` | 1 B | 距前一个兄弟的水平距离 |
| `padding_rows` + `lookahead_bytes` | 4 bit + 4 bit | 1 B | 行距 + 超前读取长度 |
| `padding_bytes` | `uint8_t` | 1 B | **距前一兄弟结尾的字节偏移** |
| `size_bytes` | `uint8_t` | 1 B | **本 token 自身的字节长度** |
| **合计** | | **8 B** | |

要点：

- 对 token `id` 来说，节点里只有"符号 id + 距前面 `{` 隔了几列 + 自身长 2 字节"。
  `i`、`d` 两个字符本身**不在树里**，查询时按偏移回源码切片（Rust 侧
  `node.utf8_text(source)`，源码由调用方传入）。
- `padding_bytes` / `size_bytes` 都是 `uint8_t`，单 token 超过 255 字节时
  tree-sitter 自动改用堆分配形态存这个叶子。

---

## 3. 形态二：堆分配父节点（`SubtreeHeapData`，80 字节头）

所有有子节点的节点（以及超大叶子、错误节点）走这个形态：

| 字段 | 类型 | 偏移 | 大小 | 含义 |
|------|------|------|------|------|
| `ref_count` | `uint32_t` | 0 | 4 | 引用计数（GLR 多版本共享 / Rust 端 clone） |
| `padding` | `Length` | 4 | 12 | 距前兄弟的距离：`{bytes u32, extent{row u32, column u32}}` |
| `size` | `Length` | 16 | 12 | 自身跨度，结构同上 |
| `lookahead_bytes` | `uint32_t` | 28 | 4 | 超前读取字节数 |
| `error_cost` | `uint32_t` | 32 | 4 | 错误恢复代价（用于 GLR 版本择优） |
| `child_count` | `uint32_t` | 36 | 4 | 子节点数 |
| `symbol` | `uint16_t` | 40 | 2 | 文法符号 id |
| `parse_state` | `uint16_t` | 42 | 2 | 解析器状态 |
| 11 个 bool 位域 | 位域 | 44 | 2 | `visible/named/extra/fragile_left/fragile_right/has_changes/has_external_tokens/has_external_scanner_state_change/depends_on_column/is_missing/is_keyword` |
| （对齐填充） | | 46 | 2 | |
| `union` | 见下表 | 48 | 32 | 按节点类型三选一 |
| **合计** | | | **80 B** | |

### 3.1 尾部 union（32 字节，由最大分支决定）

| 分支 | 用于 | 字段 | 大小 |
|------|------|------|------|
| 非叶子分支 | `child_count > 0` | `visible_child_count u32` + `named_child_count u32` + `visible_descendant_count u32` + `dynamic_precedence i32` + `repeat_depth u16` + `production_id u16` + `first_leaf{symbol u16, parse_state u16}` | 24 B |
| **外部扫描状态** | `child_count == 0 && has_external_tokens` | `ExternalScannerState { union{char* long_data; char short_data[24]} + length u32 }` | **32 B** ← 决定 union 尺寸 |
| 错误叶子 | `symbol == ERROR` | `lookahead_char i32` | 4 B |

注意：union 的 32 字节上限是**外部扫描状态分支**撑出来的
（24 字节内联缓冲 + 对齐），**所有**父节点都要为此付 32 字节，
即使它们用的是 24 字节的非叶子分支。

### 3.2 子槽位数组与 malloc 尺寸

父节点的一次堆分配 = 头部 + 子槽位数组，**一体分配**：

```
malloc(80 + child_count × 8)        // 每个子节点占一个 8 字节 Subtree 槽位
```

子节点中 inline 叶子直接内嵌在槽位里；子节点若是父节点/外部 token，
槽位里放的是堆指针。所以全树的内存总量可以精确写成：

```
Tree 总内存 ≈ Σ_父节点 (80 + ovh)  +  Σ_外部token (80 + ovh)  +  8 × (N − 1)  +  TSTree(~几十 B)
             └── 独立分配的头部 ──┘   └── 独立分配的外部token ──┘   └── 所有节点的槽位 ──┘
```

（`ovh` = 堆分配开销，Windows LFH 约 8–16 B 且按 16 B 粒度取整；`N` = 全部堆节点 + inline 叶子数。）

---

## 4. 形态三：外部 token

本项目文法（`grammar/grammar.js`）的 external scanner 负责 7 类 token：

```
comment / long_string_content / shebang /
short_string_content_double / short_string_content_single /
emmy_line / top_word_*（列首关键字族）
```

每个外部 token 都是一次 80 字节堆分配（无子槽位），尾部 union 存
scanner 的序列化状态。**本项目 scanner 状态只有 1 字节**
（`top_keyword_disabled` 开关位，见 `grammar/src/scanner.c`），
落在 24 字节内联缓冲内，**无二次分配**。

含义：每行注释、每个字符串的内容 token，成本都是 ~96 B
（80 头 + 堆开销）—— 是普通叶子（8 B）的 **12 倍**。
EmmyLua 注解文件注释密度极高，这是注解文件每节点成本偏高（实测 150.9 B）的直接原因。

---

## 5. 一行代码的账

```lua
{id=12345, name="sword"}
```

| 节点 | 形态 | 实付（含堆开销） |
|------|------|------|
| table_constructor | 父（80 + 2×8 + ovh） | ~112 B |
| field `id` | 父（80 + 3×8 + ovh） | ~112 B |
| `id` / `=` / `12345` | inline 叶子 | 3 × 8 B（槽位，含在父分配里） |
| field `name` | 父（80 + 3×8 + ovh） | ~112 B |
| `name` / `=` | inline 叶子 | 2 × 8 B |
| string | 父（80 + 1×8 + ovh） | ~96 B |
| 字符串内容 | **外部 token** | ~96 B |
| `,` 分隔符的隐藏规则包装 | 父（隐藏，不计入 descendant_count） | ~112 B |
| **合计（10 个可见节点）** | | **~640 B ≈ 64 B/可见节点** |

数据表形态是语料里最便宜的；换成深嵌套表达式代码（每层
`_expression → _prefix_expression → _primary_expression` 都是独立堆分配），
实测可达 154 B/可见节点。

---

## 6. 隐藏节点：`descendant_count` 的盲区（关键）

`descendant_count` 的实现是：

```c
return ts_subtree_visible_descendant_count(self) + 1;   // node.c
```

**只数可见节点**。而 grammar.js 中的隐藏规则（`_` 前缀）在树里是货真价实的
父节点（80 B + 槽位 + 开销），却**一个都不计入** `descendant_count`。

本仓库文法共 **29 个隐藏规则**，逐层包装的主力：

```
_statement / _expression / _primary_expression / _prefix_expression /
_block / _top_statement / _top_block / _function_declaration /
_local_declaration / _if_statement / …（完整清单见 grammar/grammar.js）
```

一条顶层 `local x = a.b.c` 的实际包装链：

```
_top_block → _top_statement → local_declaration → _expression(=)
  → _prefix_expression → _prefix_expression → _primary_expression → identifier
```

可见节点可能只有 `local_declaration` + `identifier` 们，但隐藏的
`_expression/_prefix_expression/_primary_expression` 每层都是一次
~100 B 的堆分配。**表达式嵌套越深、左递归链越长，隐藏包装越多**——
这解释了实测数据里"普通代码/注解文件（154/151 B）明显贵于数据表（89 B）"
的现象：数据表结构浅，代码的表达式链深。

因此实测的"bytes per node"准确说是：

```
实测 B/可见节点 = 每堆节点实付(~96–112 B) × (1 + 隐藏节点/可见节点比) + 槽位摊销
```

---

## 7. 实测数据（`lua-perf --mem`）

测量方法：warmup 解析（丢弃，让瞬时堆块就位）→ 连续 N 次解析并保留全部
tree → 进程 RSS 差分 ÷ (N−1) 棵树 ÷ 可见节点数。命令：

```bash
cargo run --release --bin lua-perf -- --mem --mem-repeats 8 /path/to/file.lua
```

样本取自 23,692 文件的真实工作区（2026-08 实测，Windows，tree-sitter 0.26.8）：

| 样本 | 可见节点数 | 实付 B/可见节点 | 备注 |
|------|-----------|----------------|------|
| `PBMessageMap.lua`（PB 导出 map） | 632,152 | **100.7** | 大体量混合形态 |
| `table_BackpackItem_IndexTable.lua`（纯数据表） | 180,280 | **88.9** | 结构浅，隐藏包装少 |
| `Feature_SP-annotation.lua`（UE 注解生成） | 216,695 | **150.9** | 注释（外部 token）密度极高 |
| `tests/lua-root/emmy_types.lua`（普通代码，400 repeats） | 493 | **154.0** | 表达式嵌套深 |

小文件差分易被采样噪声淹没（delta < 8 MiB 会告警），务必加大 `--mem-repeats`。

---

## 8. 工作结论

1. **~100 B/可见节点**是本仓库文法的稳健估算系数；精确预算按语料形态取
   88–155 B 区间。
2. 全工作区保留 tree 的内存 ≈ `总可见节点数 × ~100 B`。
   5.2 万个文件、5,290 万可见节点的语料 ≈ **5.3 GB**——
   实测 LSP 全量保留时进程 RSS 7.5 GB，与"tree 5.3 GB + 基线 ~1.4 GB +
   解析期水位"基本吻合。
3. 对本项目的直接推论：
   - "冷启动不保留 tree、按需懒重建"（见
     [`performance-analysis.md`](performance-analysis.md) §3）是内存的
     决定性策略，保住它就是省 5 GB 量级；
   - 自动生成的数据表 / 注解导出文件节点最多、每节点不便宜、又几乎
     不需要语义跳转——是最值得被 `slowParseKeepTreeThresholdMs` 淘汰、
     或被 `mylua.workspace.exclude` 排除的语料。

---

## 附录：信息来源

- 字段布局：tree-sitter 0.26.8 `src/subtree.h`（`SubtreeInlineData` /
  `SubtreeHeapData` / `ExternalScannerState`）、`src/length.h`（`Length`/`TSPoint`）、
  `src/node.c`（`ts_node_descendant_count`）
- 外部 token 清单：`grammar/grammar.js` `externals`；scanner 状态大小：
  `grammar/src/scanner.c`（`ScannerState` 1 字节）
- 隐藏规则清单：`grammar/grammar.js` 中 `_` 前缀规则（29 个）
- 实测：`lua-perf --mem`（实现见 `lsp/crates/mylua-lsp/src/bin/lua_perf.rs`，
  采样依赖 `memory-stats` crate）
