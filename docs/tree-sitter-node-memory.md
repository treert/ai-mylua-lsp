# Tree-sitter 节点内存剖析

> 基于 tree-sitter **0.26.8** 源码（`src/subtree.h`、`src/length.h`、`src/node.c`、
> `src/subtree.c`、`src/alloc.c`）逐字段分析，结合 `lua-perf --mem`（RSS 口径）
> 与 `ts_set_allocator` 劫持测量（分配器口径）对本仓库文法（tree-sitter-mylua）
> 的实测数据。结论适用于 64 位平台。

---

## 概要

- tree-sitter 的"节点"（`Subtree`）不是统一大小的结构，而是 **tagged union**，三种形态：
  - **inline 叶子**：8 字节，无独立分配（住在父节点的 children 数组里）；
    本仓库文法 symbol 总数 147 < 256，**named token（identifier/number/关键字）同样 inline**
  - **堆分配节点**：80 字节头 + 子槽位数组，一体 malloc（父节点、外部 token、错误节点）
  - **外部 token**：80 字节头，无子槽位（注释、字符串内容等 7 类）
- **节点不存储任何文本**。变量名、关键字、字符串内容一个字节都不复制，
  节点里只有结构、相对偏移和文法符号 id；文本永远只有源码那一份数据。
- **隐藏规则节点是内存大头之一**：`_expression`/`_primary_expression` 等
  `_` 前缀规则（本文法 29 个）在树里**物理存在**（每个 88–96 B 堆块），
  但 `descendant_count` **一个都不数**——可见节点数只是冰山一角
  （实测数据表：18 万可见节点背后约 10.3 万个额外堆块）。
- Windows LFH 堆粒度把分配请求再放大 **1.3–2 倍**（88/96 B 请求实付 ~128 B）。
- 实测（RSS 口径）：**88.9 – 154.0 B/可见节点**，工作数字 **~100 B**；
  分配器请求口径 **70.0 – 139.6 B/可见节点**（见 §7）。

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

## 5. 一行代码的账（实证）

以合成文件 `return 1,1,1,…`（400 个 number + 400 个逗号，806 个可见节点）
为例，通过 `ts_set_allocator` 劫持 tree-sitter 的 C 侧分配器逐笔统计
（测量手段见附录），**每对 (number, `,`) 的物理堆块**：

| 精确尺寸 | 块数/对 | 结构解释 |
|---------|--------|---------|
| 88 B（80 头 + 1 槽） | 1 | 单子隐藏包装（`_expression` 或 `_primary_expression`） |
| 96 B（80 头 + 2 槽） | 2 | 双子节点（另一层隐藏包装 / 表达式列表链节点） |
| **合计** | **280 B/对** | number 与逗号本身均 inline（各 8 B 槽位，已含在父块内） |

即：**806 个可见节点 ↔ 约 1,205 个堆块**，每对 token 的包装成本
280 B（分配器请求），Windows LFH 实付约 **2 倍**（RSS 口径 293.5 B/可见节点）。

对照真实语料的 `descendant_count` 可见节点数与堆块数：

| 样本 | 可见节点/树 | 堆块/树（88/96 B 为主） | 分配器口径 |
|------|------------|------------------------|-----------|
| 数据表 `table_BackpackItem_IndexTable.lua` | 180,280 | ~103,700 | 70.0 B/可见节点 |
| 注释密集 `emmy_types.lua` | 493 | ~560 | 103.6 B/可见节点 |

数据表结构浅（隐藏包装层少），代码/注解文件包装层深——这与 §6 的
结论互相印证。

---

## 6. 隐藏节点：`descendant_count` 的盲区（关键）

`descendant_count` 的实现是：

```c
return ts_subtree_visible_descendant_count(self) + 1;   // node.c
```

**只数可见节点**。而 grammar.js 中的隐藏规则（`_` 前缀）**物理上是货真价实的
堆节点**（80 B 头 + 槽位），却因为 `visible = false`：
- 不计入 `descendant_count`
- 不出现在 `to_sexp()` / named 遍历里（**corpus 测试的 S 表达式因此"看不见"
  它们，容易误以为隐藏规则不产生节点——这是本剖析一度踩过的坑**）
- 不出现在普通 `child` 遍历里

即：**隐藏节点占足内存，但所有基于节点数的统计都对它失明**。

本仓库文法共 **29 个隐藏规则**，逐层包装的主力：

```
_statement / _expression / _primary_expression / _prefix_expression /
_block / _top_statement / _top_block / _function_declaration /
_local_declaration / _if_statement / …（完整清单见 grammar/grammar.js）
```

一条 `local x = a.b.c` 的 value 侧物理包装（named 视图只能看到
`local_declaration → variable → identifier`）：

```
local_declaration
└── values: expression_list
    └── _expression            ← 88 B 堆块，不可见
        └── _primary_expression  ← 88 B 堆块，不可见
            └── _prefix_expression? ← 命中 prefix 时再加一层
                └── variable（可见）
```

**表达式嵌套越深、choice 层级越多，隐藏包装越多**——这解释了实测数据里
"普通代码/注解文件（103.6 B 分配器口径）明显贵于数据表（70.0 B）"的现象：
数据表结构浅，代码的表达式链深。token 本身（inline 8 B）反而是最便宜的部分。

因此实测的"bytes per node"准确说是：

```
实测 B/可见节点 = 堆块实付(88–104 B 请求 × LFH 1.3–2 倍) × 堆块数/可见节点数 + 槽位摊销
```

---

## 7. 实测数据

### 7.1 测试背景（`profile-memory.py`，全工作区）

一个大型真实 UE 游戏项目工作区（2026-08 实测，Windows，tree-sitter 0.26.8）。
全工作区总量数据由 [`profile-memory.py`](../.cursor/scripts/profile-memory.py)
采集——该脚本通过 Extension Development Host 启动完整 LSP，等待索引 Ready
后汇总内存普查与 RSS 采样：

| 项目 | 数值 |
|------|------|
| 文件总数 | 23,690 个（`.lua`） |
| 文件总大小 | 246 MB（246.6 MiB 源码常驻） |
| 全工作区可见节点总数 | 52,939,615（约 5,290 万） |
| tree 相关内存（≈ 可见节点数 × ~100 B） | ~5 GB |
| 全量保留 tree 时 LSP 进程 RSS | ~7.5 GB |

> 注意：默认配置下冷启动不保留 tree（见 §8），profile-memory.py 的
> `tree_nodes` 等指标会全部归零；需将
> `mylua.performance.slowParseKeepTreeThresholdMs` 设为小于 15
> （如 `0`）强制全保留后才能得到上表数据。

语料形态构成：UE 导出数据表（`Export/pbin/lua/`、`Config/`，节点多、
结构浅）+ UE 注解生成文件（`UEAnnotation.LuaComment/`，注释密度极高）+
常规业务代码。

### 7.2 单文件测量（`lua-perf --mem`）

每节点成本系数由 lua-perf 对单文件独立测量，样本文件取自上述工作区
（`emmy_types.lua` 除外，取自本仓库 `tests/lua-root/`）。

测量方法：warmup 解析（丢弃，让瞬时堆块就位）→ 连续 N 次解析并保留全部
tree → 进程 RSS 差分 ÷ (N−1) 棵树 ÷ 可见节点数。命令：

```bash
cargo run --release --bin lua-perf -- --mem --mem-repeats 8 /path/to/file.lua
```

四组样本结果（RSS 口径 = `--mem` 输出；分配器口径 = `ts_set_allocator`
劫持 C 侧 malloc 逐笔统计，不含堆粒度开销；两者之比即 LFH 放大系数）：

| 样本 | 可见节点数 | RSS 口径 B/可见节点 | 分配器口径 B/可见节点 | LFH 放大 |
|------|-----------|--------------------|---------------------|---------|
| `PBMessageMap.lua`（PB 导出 map） | 632,152 | **100.7** | — | — |
| `table_BackpackItem_IndexTable.lua`（纯数据表） | 180,280 | **88.9** | **70.0** | 1.27× |
| `Feature_SP-annotation.lua`（UE 注解生成） | 216,695 | **150.9** | — | — |
| `tests/lua-root/emmy_types.lua`（普通代码） | 493 | **154.0** | **103.6** | 1.49× |

小文件差分易被采样噪声淹没（delta < 8 MiB 会告警），务必加大 `--mem-repeats`。

---

## 8. 工作结论

1. **~100 B/可见节点**（RSS 口径）是本仓库文法的稳健估算系数；精确预算按
   语料形态取 88–155 B 区间。其构成为：
   `隐藏包装与父节点的堆块（88–104 B 请求）× 堆块/可见节点比 + LFH 粒度放大（1.3–2 倍）`。
2. 全工作区保留 tree 的内存 ≈ `总可见节点数 × ~100 B`。
   §7.1 测试背景的语料按此折算 ≈ **5.3 GB**——实测全量保留时进程
   RSS 7.5 GB，与"tree 5.3 GB + 基线 ~1.4 GB + 解析期水位"基本吻合。
   折算到项目 5 万文件目标 ≈ **~11 GB**，全量常驻不可行，印证懒重建路线。
3. 对本项目的直接推论：
   - "冷启动不保留 tree、按需懒重建"（见
     [`performance-analysis.md`](performance-analysis.md) §3）是内存的
     决定性策略，保住它就是省 5 GB 量级；
   - 自动生成的数据表 / 注解导出文件节点最多、每节点不便宜、又几乎
     不需要语义跳转——是最值得被 `slowParseKeepTreeThresholdMs` 淘汰、
     或被 `mylua.workspace.exclude` 排除的语料；
   - 若未来要压缩 tree 内存，文法侧减少 `_expression → _primary_expression`
     这类多层 choice 包装（每层 88 B 堆块）是最大的可操作杠杆；
     tree-sitter 运行时的 80 B 头与 LFH 粒度则不在本仓库控制范围内。

---

## 附录：信息来源与测量手段

- 字段布局：tree-sitter 0.26.8 `src/subtree.h`（`SubtreeInlineData` /
  `SubtreeHeapData` / `ExternalScannerState`）、`src/length.h`（`Length`/`TSPoint`）、
  `src/node.c`（`ts_node_descendant_count`）、`src/subtree.c`
  （`ts_subtree_new_leaf` 的 inline 判定、`ts_subtree_new_node` 的一体分配）、
  `src/alloc.c`（`ts_set_allocator` 分配器覆盖点）
- 外部 token 清单：`grammar/grammar.js` `externals`；scanner 状态大小：
  `grammar/src/scanner.c`（`ScannerState` 1 字节）；symbol 编号：
  生成的 `grammar/src/parser.c`（`SYMBOL_COUNT 147`，全部 < 256，
  named token 与匿名 token 混合编号）
- 隐藏规则清单：`grammar/grammar.js` 中 `_` 前缀规则（29 个）
- 实测：单文件每节点成本由 `lua-perf --mem` 测量（实现见
  `lsp/crates/mylua-lsp/src/bin/lua_perf.rs`，采样依赖 `memory-stats` crate）；
  全工作区总量（§7.1）由 `.cursor/scripts/profile-memory.py` 采集
  （需配合 `slowParseKeepTreeThresholdMs < 15` 的全保留配置）
- **分配器口径测量**（§5/§7 精确尺寸数据）：通过 `ts_set_allocator`
  劫持 tree-sitter 的 C 侧 malloc/calloc/realloc/free，用
  `ptr → size` 活跃表 + 精确尺寸直方图统计。**注意**：tree-sitter 的
  C 分配不走 Rust 全局分配器（默认链到 CRT 的独立堆），因此
  `#[global_allocator]` 计数与 `HeapWalk` 对它均无效；
  `to_sexp()`/corpus S 表达式是 named 视图，看不到隐藏节点。
