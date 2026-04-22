# Column-0 Block-End 重设计讨论

> **状态**：讨论中（WIP）
> **创建日期**：2026-04-22
> **相关文件**：`grammar/grammar.js`、`grammar/src/scanner.c`、`grammar/lua.bnf`、`grammar/test/corpus/col0_error_recovery.txt`

---

## 1. 背景：当前 `_col0_block_end` 设计

### 1.1 设计目标

mylua-lsp 把**顶行（column 0）的关键字开头的语句**设计成顶层语句，强化了 Lua 的语法限制，方便快速定位缺少 `end` 的错误。

### 1.2 实现方式

- **外部 scanner**（`scanner.c`）：当 `valid_symbols[COL0_BLOCK_END]` 为真、且 `column == 0`、且 lookahead 是 `[a-zA-Z_:]` 时，发射一个零宽度的 `_col0_block_end` token。
- **语法层**（`grammar.js`）：`_block_end` 定义为 `choice('end', $._col0_block_end)`，所有需要 `end` 的语句（`do_statement`、`while_statement`、`if_statement`、`for_numeric_statement`、`for_generic_statement`、`function_body`）都使用 `_block_end`。`repeat_statement` 则用 `choice(seq('until', expr), $._col0_block_end)`。

### 1.3 效果

- 正确缩进的代码正常解析（`end` 照常匹配）。
- 缺少 `end` 的错误在**下一个顶层语句**处就能报出，而非传播到 EOF。
- **代价**：嵌套代码必须缩进（至少 1 空白），否则会报错。

### 1.4 已知问题

当前设计存在 bug，之前尝试过修复但方案过于 trick。具体问题有待进一步记录。

---

## 2. 提议的新方案：`top_empty_statement`

### 2.1 核心思路

修改 `chunk` 的语法定义：

```bnf
chunk ::= [ shebang ] { statement | top_empty_statement } [ return_statement ]
```

- `top_empty_statement` 是顶层关键字前由 scanner 生成的**零宽度 token** 对应的 statement。
- scanner 遇到顶层关键字时**必定生成**一个 `top_empty_statement`。
- 防止无限生成的机制：类似"上一次已经生成过了，这次就不生成"。
- LSP 层不理会 `top_empty_statement`。

### 2.2 期望效果

通过在顶层关键字前插入一个特殊 statement，迫使之前的语句强制规约，从而把解析从嵌套块拉回顶层 `chunk`。

---

## 3. 可行性分析：**方案不可行**（按字面设计）

### 3.1 核心结论

**`top_empty_statement` 如果只挂在 `chunk` 层，无法实现"遇到顶层关键字就收束未闭合嵌套块"的效果。**

### 3.2 原因：LR/Tree-sitter 的 lookahead 机制

#### 关键原理

在 LR parser（Tree-sitter 基于此）中：

- **lookahead token 只是"查表依据"，不是主动控制器**
- 能不能规约，取决于 `ACTION[当前状态, lookahead token]` 表里是否有对应动作
- **不是**"我发明了一个新 token，所以它能一路把前面的东西规约掉"
- **而是**"只有当这个 token 在当前状态可见时，它才可能参与规约"

#### 为什么 `top_empty_statement` 在嵌套块里不可见

- `source_file` 在最外层接一个可选的 `_block`
- `_block` 本质上是 `repeat1($._statement)` 加可选 `return_statement`
- `if_statement`、`while_statement`、`function_body` 都包含**嵌套 block**

当 parser 还在内层 `_block` 里时，`top_empty_statement` 只存在于 `chunk`（即 `source_file`）层的语法定义中，**当前 state 根本看不见它**。

#### 具体例子

```lua
function foo()
    if a then
        bar()
function other() end
```

parser 读到 `function other` 时：
1. 它还在内层 `_block` 里
2. `function` 是合法的 `_statement` 开头（`function_declaration`）
3. **没有出错** → parser 把 `function other() end` 当成嵌套 block 内的下一个 statement
4. `top_empty_statement` 根本没有机会介入

### 3.3 与现有 `_col0_block_end` 的对比

`_col0_block_end` 能工作的原因**不是**因为它"零宽"，**而是**因为它被接到了所有需要同步的闭合位上：

| 方案 | token 出现位置 | 嵌套块内是否可见 | 能否收束未闭合块 |
|------|---------------|-----------------|----------------|
| `_col0_block_end` | 所有 `_block_end` 位置 + `repeat_statement` 闭合位 | ✅ 是 | ✅ 能 |
| `top_empty_statement`（只在 chunk） | 仅 `source_file` 层 | ❌ 否 | ❌ 不能 |

### 3.4 如果把 `top_empty_statement` 也塞进嵌套块闭合位？

技术上可行，但**语义上已经退化回现有 `_col0_block_end` 的同类方案**，不再是"纯顶层 empty statement"。

---

## 4. Tree-sitter 错误恢复机制（知识积累）

### 4.1 正常解析流程

LR parser 对每个 lookahead token 查动作表 `ACTION[state, token]`：

| 动作 | 含义 |
|------|------|
| **shift** | 吃掉 token，进入新状态 |
| **reduce** | 按产生式规约，弹栈再 goto |
| **accept** | 解析成功 |
| **error** | 当前 token 在当前状态下不合法 |

### 4.2 Tree-sitter 遇到意外 token 时的处理

Tree-sitter 不会"一报错就停"，而是做**代价驱动的错误恢复**：

1. **先看当前有没有合法正常解析路径**（可能同时保留多个解析栈版本）
2. **如果有多条路径，保留能继续的**
3. **如果都不行，再做恢复**：
   - **插入缺失 token**：比如假装这里有个 `end`（树中表现为 `missing` 节点）
   - **跳过不合法输入**：把这段内容吞进 `ERROR` 节点
   - **局部收缩后继续**：通过代价更低的恢复动作保持树结构稳定
4. **恢复时优先选代价最低的方案**

### 4.3 关键认知

- **Tree-sitter 不会因为看到"可疑顶层 token"就主动弹栈回 `chunk`**
- **外部 scanner 受 `valid_symbols` 约束**：当前 parser state 只接受特定候选 token，scanner 不能靠一己之力改变全局解析方向
- **"强制规约"只在当前 state 对该 token 有动作时才会发生**

---

## 5. 后续讨论方向

> 以下为待讨论的开放问题，后续对话中继续补充。

- [ ] 当前 `_col0_block_end` 设计的具体 bug 是什么？复现场景？
- [ ] 是否有更好的替代方案？
- [ ] 是否可以通过调整 Tree-sitter 的 `conflicts` 或 `precedence` 来改善错误恢复？
- [ ] 是否考虑在 LSP 层（而非语法层）做更智能的错误定位？
- [ ] 对"嵌套代码必须缩进"这个限制，是否有办法放松？

---

## 6. 参考

- `grammar/lua.bnf` §2.1.1 — Column-0 block boundary 设计说明
- `grammar/grammar.js` — `_block_end`、`source_file` 定义
- `grammar/src/scanner.c` — `COL0_BLOCK_END` 扫描逻辑
- `grammar/test/corpus/col0_error_recovery.txt` — 错误恢复测试用例
- `grammar/test/corpus/col0_boundary.txt` — 正常边界测试用例
