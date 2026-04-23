# Keyword Scanner Design

> **Status**: Implemented and stable.
> **Scope**: `grammar/grammar.js`, `grammar/src/scanner.c`, `grammar/lua.bnf`

---

## 1. Background: Why a Custom Keyword Scanner

### 1.1 The `_col0_block_end` Problem (Removed)

The original grammar included a `_col0_block_end` mechanism: the scanner emitted a zero-width token at column 0 when it encountered what looked like a new statement, and the grammar accepted this token as a valid block-end alternative to `end`. This effectively **faked block closure** when `end` was missing.

**Why it was removed:**

- It turned missing-`end` errors into **legitimate parse paths**, silently swallowing errors
- It **polluted the syntax tree** — the CST no longer purely represented valid Lua
- It **weakened diagnostics** — `ERROR` / `MISSING` nodes were not generated for genuinely broken code
- It introduced a **non-standard constraint** ("nested code must be indented") baked into the grammar itself
- The design was a **layer violation**: error recovery logic does not belong in the grammar

**Conclusion**: `_col0_block_end` was removed entirely. It is not to be reintroduced.

### 1.2 The Top-Level Keyword Split Idea

To replace `_col0_block_end`, a new approach was adopted: **distinguish top-level (column 0) keyword statements from nested ones** at the grammar/scanner level. This makes missing-`end` errors surface earlier — at the next top-level keyword — without faking block closure.

### 1.3 Evolution to Full Scanner Ownership

During implementation, several issues were discovered:

1. **`end` as identifier**: When `end` was an inline grammar string (`'end'`), tree-sitter's error recovery could demote it to `identifier` in certain states. Moving `end` to the external scanner (as `block_end`) fixed this.

2. **Hidden tokens don't get MISSING nodes**: When `_block_end` was a hidden external token (prefixed with `_`), tree-sitter would **not** insert `MISSING` nodes during error recovery. Renaming it to `block_end` (visible) fixed this — tree-sitter now correctly inserts `MISSING block_end`.

3. **Partial keyword matching corrupts lexer state**: The original `scan_top_keyword` used `try_match_keyword` which advanced the lexer character-by-character. If matching failed partway (e.g., trying to match `repeat` on `return`), the lexer position was corrupted for subsequent matching attempts in the same `scan()` call.

4. **Inline keyword strings allow error-recovery fallback**: When keywords like `local` existed both as external tokens (`TOP_LOCAL`) and inline strings (`'local'`), tree-sitter could bypass the scanner's column-0 enforcement by falling back to the inline string during error recovery.

These issues led to the final design: **all keywords and identifiers are owned by the external scanner**.

---

## 2. Current Architecture

### 2.1 Core Principle

> Every token starting with `[a-zA-Z_]` is produced by the external scanner.
> The scanner emits either a keyword token (`word_*` / `top_word_*`) or `identifier`.
> The grammar contains **no inline keyword strings** and **no identifier regex**.

### 2.2 Scanner: `scan_word` Function

The unified `scan_word` function in `scanner.c`:

1. Checks that the current character is `[a-zA-Z_]`
2. Reads the **full identifier** into a buffer (up to 63 chars)
3. Calls `mark_end` to commit the consumed text
4. Looks up the buffer in the keyword table:
   - If it matches a keyword **at column 0** with a top variant → emit `TOP_WORD_*`
   - If it matches a keyword at other columns (or no top variant) → emit `WORD_*`
   - If no match → emit `IDENTIFIER`

This "read-then-match" approach avoids the partial-matching bug entirely.

### 2.3 Keyword Table

All 22 Lua keywords are in the scanner's keyword table:

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

8 keywords have top-level variants: `if`, `while`, `repeat`, `for`, `function`, `goto`, `do`, `local`.

### 2.4 Grammar Structure

```
source_file  →  _top_block
_top_block   →  { _top_statement | _statement } [ return_statement ]
_block       →  { _statement } [ return_statement ]
```

- `_top_block` is used **only** at the `source_file` level. It accepts both `_top_statement` (column-0 keyword variants) and `_statement` (everything else).
- `_block` is used in **all nested positions** (do, while, if, for, repeat, function body). It accepts only `_statement`.

Each affected keyword has two internal rules:

```js
_top_if_statement  →  $.top_word_if ...   // aliased to $.if_statement
_if_statement      →  $.word_if ...       // aliased to $.if_statement
```

The `alias()` ensures the final CST node name is always `if_statement` — no `top_*` leaks.

### 2.5 Token Naming Convention

| Category | Naming | Example | Visibility |
|----------|--------|---------|------------|
| Top-level keyword | `$.top_word_*` | `$.top_word_if` | Visible in CST |
| Normal keyword | `$.word_*` | `$.word_if` | Visible in CST |
| Identifier | `$.identifier` | `$.identifier` | Visible in CST |

All keyword tokens are **visible** (no `_` prefix). This is critical because:
- Tree-sitter generates `MISSING` nodes for visible tokens during error recovery
- Hidden tokens (`_` prefix) do **not** get `MISSING` nodes, breaking diagnostics

### 2.6 Scanner Execution Order

```
1. Shebang (only at file start, column 0)
2. Skip whitespace
3. Short string content (inside "..." or '...')
4. Word scanning: scan_word() — keywords + identifiers
5. EmmyLua line / Comment (--- or --)
6. Long string content ([=*[...]=*])
```

Word scanning comes **after** short-string content (to avoid matching keywords inside strings) and **before** comment/emmy (to avoid consuming `-` as part of a word).

---

## 3. Behavior

### 3.1 Column 0 Enforcement

When a keyword with a top variant appears at column 0, the scanner **unconditionally** emits `TOP_WORD_*`, regardless of the parser state (`valid_symbols`). This means:

- **At the top level**: `TOP_WORD_*` is accepted by `_top_statement` in `_top_block` → normal parsing
- **Inside a nested block**: `_block` does not accept `_top_statement`, so `TOP_WORD_*` is unexpected → parse error

**Consequence**: Nested code **must be indented**. A column-0 keyword inside a nested block will trigger a parse error. This is a deliberate design trade-off.

### 3.2 Error Front-Loading

When `end` is missing from a block, the next column-0 keyword forces the parser to close the current block early (via `MISSING word_end`), then start a new top-level statement. This moves the error **forward** to near the actual problem, rather than deferring it to the end of the file.

Example:
```lua
function foo()
    if a then
        bar()
function other() end   -- column 0 → TOP_WORD_FUNCTION
```

Result:
```
(source_file
  (ERROR ...)                    -- foo's broken body
  (function_declaration ...))    -- other() parsed correctly
```

### 3.3 MISSING Node Generation

Because all keyword tokens are **visible** (not hidden), tree-sitter correctly generates `MISSING` nodes when a required keyword is absent:

```lua
function foo()
local y = 2    -- column 0 → TOP_WORD_LOCAL → forces foo() to close
end            -- orphaned → ERROR
```

Result:
```
(source_file
  (function_declaration
    ...
    (function_body
      ...
      (MISSING word_end)))    -- ← MISSING node generated!
  (local_declaration ...)
  (ERROR ...))
```

### 3.4 `word` Property

The grammar retains `word: $ => $.identifier`. Although `identifier` is now an external token, tree-sitter accepts this. The `word` property enables tree-sitter's keyword extraction optimization for any remaining inline string tokens (currently only punctuation operators).

---

## 4. Non-Goals and Limitations

### 4.1 Not Standard Lua

This design introduces a non-standard constraint: **nested code must be indented**. Column-0 keywords inside nested blocks will trigger parse errors. This is an intentional trade-off for better error localization.

### 4.2 Not All Errors Are Front-Loaded

Only the 8 keywords with top variants (`if`, `while`, `repeat`, `for`, `function`, `goto`, `do`, `local`) trigger error front-loading. Missing `end` followed by:
- Assignment statements
- Function call statements
- `return` statements
- Other non-top-keyword constructs

...will **not** be front-loaded. The error may still defer to the end of the file in these cases.

### 4.3 `return` Is Not a Top Keyword

`return` is deliberately excluded from top-level splitting. It does not start a block and is not a useful synchronization point.

### 4.4 Expression-Level `function_definition` Is Not Affected

Only statement-level `function_declaration` has a top variant. Expression-level `function_definition` (e.g., `local f = function() end`) uses `$.word_function` at any column.

---

## 5. Scanner Directives

### 5.1 `---#disable top_keyword`

Disables top-level keyword emission from the point of the directive to the end of the file (or until a corresponding `---#enable top_keyword` is encountered). When disabled, all keywords at column 0 emit their normal `WORD_*` tokens instead of `TOP_WORD_*`.

**Syntax:**
```lua
---#disable top_keyword
```

**Effect:** After this directive, the scanner treats all column-0 keywords as if they were indented — they emit `WORD_*` instead of `TOP_WORD_*`. This means the parser will not force block closure at column-0 keywords.

### 5.2 `---#enable top_keyword`

Re-enables top-level keyword emission. This restores the default behavior where column-0 keywords emit `TOP_WORD_*`.

**Syntax:**
```lua
---#enable top_keyword
```

### 5.3 Use Case

Some Lua files have deeply nested code at column 0 (e.g., code inside a `function()` expression assigned to a local). In such files, the top-keyword mechanism incorrectly forces block closure. The directive allows these files to opt out:

```lua
---#disable top_keyword
local createJson = function ()
local math = require('math')    -- column 0, but no TOP_WORD_LOCAL
local string = require("string")
-- ... rest of file ...
end
return createJson()
```

### 5.4 Implementation Details

- **Scanner state**: A `ScannerState` struct with a `top_keyword_disabled` boolean flag
- **Serialization**: The flag is serialized/deserialized as 1 byte, ensuring correct behavior with tree-sitter's incremental parsing
- **Detection**: The directive is detected during EmmyLua line scanning (`---` prefix). After consuming the third dash, the scanner checks for `#disable top_keyword` or `#enable top_keyword`
- **Directive must be at column 0**: Since `---` comments are typically at column 0, and the directive is part of an EmmyLua line, it naturally requires column 0 placement
- **The directive is still emitted as `EMMY_LINE`**: It is a valid EmmyLua comment that happens to have side effects on the scanner state

### 5.5 Constraints

- The directive only affects `TOP_WORD_*` emission. Normal `WORD_*` tokens and `IDENTIFIER` tokens are unaffected
- The directive is file-scoped: it persists from the point of occurrence until overridden or until end of file
- The directive format is strict: `---#disable top_keyword` or `---#enable top_keyword` (optional whitespace between `---` and `#`, and between the command and `top_keyword`)

---

## 6. Future Possibilities

The scanner-owned keyword architecture enables several future extensions:

1. **Context-sensitive keywords**: e.g., `t.end = 1` where `end` after `.` is treated as `identifier` instead of `word_end`. This can be implemented by checking the previous token or maintaining scanner state.

2. **Additional top keywords**: If needed, more keywords can be given top variants by adding entries to the keyword table.

3. **Custom syntax extensions**: The scanner can be extended to support non-standard Lua syntax (e.g., new keywords, modified keyword semantics) without changing the grammar's core structure.

---

## 7. File Reference

| File | Role |
|------|------|
| `grammar/grammar.js` | Grammar rules, externals, alias mappings |
| `grammar/src/scanner.c` | External scanner: `scan_word`, keyword table, all token scanning |
| `grammar/lua.bnf` | BNF documentation (human-readable, not machine-consumed) |
| `grammar/test/corpus/col0_error_recovery.txt` | Error recovery test cases |
| `grammar/test/corpus/top_level_keyword_split.txt` | Top-level keyword split test cases |
| `grammar/test/corpus/statements.txt` | Statement parsing test cases |
| `grammar/test/corpus/expressions.txt` | Expression parsing test cases |
