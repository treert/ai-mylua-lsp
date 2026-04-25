# Keyword Scanner Design

> **Status**: Implemented and stable.
> **Scope**: `grammar/grammar.js`, `grammar/src/scanner.c`, `grammar/lua.bnf`

---

## 1. 设计动机

### 为什么所有关键字由外部扫描器管理

将所有关键字和标识符交由 `scanner.c` 统一产出，解决了以下问题：

1. **`end` 被降级为 identifier**：当 `end` 是内联字符串时，tree-sitter 错误恢复可能将其降级
2. **隐藏 token 无 MISSING 节点**：`_` 前缀的外部 token 不会生成 `MISSING` 节点，影响诊断
3. **部分匹配破坏词法器状态**：逐字符匹配关键字失败时，词法器位置被污染
4. **内联字符串绕过列 0 约束**：关键字同时存在外部 token 和内联字符串时，错误恢复可绕过扫描器

### 顶层关键字分裂

区分列 0（顶层）和嵌套位置的关键字，使缺失 `end` 的错误在下一个顶层关键字处提前暴露，而非延迟到文件末尾。

---

## 2. 当前架构

### 核心原则

> 每个以 `[a-zA-Z_]` 开头的 token 都由外部扫描器产出。
> 扫描器发射关键字 token（`word_*` / `top_word_*`）或 `identifier`。
> grammar 中**无内联关键字字符串**，**无 identifier 正则**。

### `scan_word` 函数

`scanner.c` 中的统一入口：

1. 检查当前字符是否为 `[a-zA-Z_]`
2. 读取**完整标识符**到缓冲区（最多 63 字符）
3. 调用 `mark_end` 提交已消费文本
4. 在关键字表中查找：
   - 匹配关键字 + 列 0 + 有 top 变体 → 发射 `TOP_WORD_*`
   - 匹配关键字但非列 0（或无 top 变体）→ 发射 `WORD_*`
   - 无匹配 → 发射 `IDENTIFIER`

"先读后匹配"避免了部分匹配 bug。

### 关键字表

全部 22 个 Lua 关键字，其中 8 个有顶层变体（`if`, `while`, `repeat`, `for`, `function`, `goto`, `do`, `local`）：

| Keyword    | Normal Token    | Top Token (col 0) |
|------------|-----------------|---------------------|
| `and`      | `WORD_AND`      | —                   |
| `break`    | `WORD_BREAK`    | —                   |
| `do`       | `WORD_DO`       | `TOP_WORD_DO`       |
| `else`     | `WORD_ELSE`     | —                   |
| `elseif`   | `WORD_ELSEIF`   | —                   |
| `end`      | `WORD_END`      | —                   |
| `false`    | `WORD_FALSE`    | —                   |
| `for`      | `WORD_FOR`      | `TOP_WORD_FOR`      |
| `function` | `WORD_FUNCTION` | `TOP_WORD_FUNCTION` |
| `goto`     | `WORD_GOTO`     | `TOP_WORD_GOTO`     |
| `if`       | `WORD_IF`       | `TOP_WORD_IF`       |
| `in`       | `WORD_IN`       | —                   |
| `local`    | `WORD_LOCAL`    | `TOP_WORD_LOCAL`    |
| `nil`      | `WORD_NIL`      | —                   |
| `not`      | `WORD_NOT`      | —                   |
| `or`       | `WORD_OR`       | —                   |
| `repeat`   | `WORD_REPEAT`   | `TOP_WORD_REPEAT`   |
| `return`   | `WORD_RETURN`   | —                   |
| `then`     | `WORD_THEN`     | —                   |
| `true`     | `WORD_TRUE`     | —                   |
| `until`    | `WORD_UNTIL`    | —                   |
| `while`    | `WORD_WHILE`    | `TOP_WORD_WHILE`    |

### Grammar 结构

```
source_file  →  _top_block
_top_block   →  { _top_statement | _statement } [ return_statement ]
_block       →  { _statement } [ return_statement ]
```

- `_top_block` 仅用于 `source_file` 层级，接受 `_top_statement`（列 0 关键字变体）和 `_statement`
- `_block` 用于所有嵌套位置，仅接受 `_statement`

每个受影响的关键字有两条内部规则，通过 `alias()` 统一最终 CST 节点名：

```js
_top_if_statement  →  $.top_word_if ...   // aliased to $.if_statement
_if_statement      →  $.word_if ...       // aliased to $.if_statement
```

### Token 可见性

所有关键字 token 均为**可见**（无 `_` 前缀）。这确保 tree-sitter 在错误恢复时生成 `MISSING` 节点。

### 扫描器执行顺序

```
1. Shebang（仅文件开头，列 0）
2. 跳过空白
3. 短字符串内容（"..." 或 '...' 内部）
4. Word 扫描：scan_word() — 关键字 + 标识符
5. EmmyLua 行 / 注释（--- 或 --）
6. 长字符串内容（[=*[...]=*]）
```

---

## 3. 行为

### 列 0 强制

列 0 处有 top 变体的关键字**无条件**发射 `TOP_WORD_*`：

- **顶层**：被 `_top_block` 中的 `_top_statement` 接受 → 正常解析
- **嵌套块内**：`_block` 不接受 `_top_statement` → 解析错误

**约束**：嵌套代码必须缩进。列 0 的关键字在嵌套块内会触发解析错误。

### 错误前置

缺失 `end` 时，下一个列 0 关键字强制关闭当前块（通过 `MISSING word_end`），然后开始新的顶层语句：

```lua
function foo()
    if a then
        bar()
function other() end   -- 列 0 → TOP_WORD_FUNCTION → foo() 被强制关闭
```

### MISSING 节点生成

```lua
function foo()
local y = 2    -- 列 0 → TOP_WORD_LOCAL → foo() 被强制关闭
end            -- 孤立 → ERROR
```

结果中 `foo` 的函数体包含 `(MISSING word_end)` 节点。

### 局限性

- 仅 8 个有 top 变体的关键字触发错误前置；赋值、函数调用、`return` 等不会
- `return` 不作为 top keyword（不开启块，非有效同步点）
- 表达式级 `function_definition`（如 `local f = function() end`）不受影响，始终使用 `$.word_function`

---

## 4. Scanner 指令

### `---#disable top_keyword`

从指令位置到文件末尾（或遇到 `---#enable top_keyword`），禁用顶层关键字发射。列 0 关键字改为发射 `WORD_*`。

```lua
---#disable top_keyword
local createJson = function ()
local math = require('math')    -- 列 0，但不发射 TOP_WORD_LOCAL
local string = require("string")
end
return createJson()
```

### `---#enable top_keyword`

恢复默认行为。

### 实现细节

- `ScannerState` 中的 `top_keyword_disabled` 布尔标志，序列化为 1 字节（支持增量解析）
- 在 EmmyLua 行扫描（`---` 前缀）时检测指令
- 指令本身仍作为 `EMMY_LINE` 发射
- 格式严格：`---#disable top_keyword` 或 `---#enable top_keyword`

---

## 5. 文件索引

| 文件 | 说明 |
|------|------|
| `grammar/grammar.js` | Grammar 规则、externals、alias 映射 |
| `grammar/src/scanner.c` | 外部扫描器：`scan_word`、关键字表、所有 token 扫描 |
| `grammar/lua.bnf` | BNF 文档（人类可读） |
| `grammar/test/corpus/col0_error_recovery.txt` | 错误恢复测试 |
| `grammar/test/corpus/top_level_keyword_split.txt` | 顶层关键字分裂测试 |
