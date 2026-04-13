# grammar

本目录实现 **Tree-sitter** 文法：**Lua 5.3+** 可执行语法 + **注释内的 EmmyLua 结构化子语法**，整文件落成 **一棵语法树**（与 [docs/architecture.md](../docs/architecture.md) §1.1 一致）。

## 推荐实现方案（当前最优默认）

### 技术与产物

| 环节 | 说明 |
|------|------|
| **文法定义** | `grammar.js`（Tree-sitter DSL，需 **Node.js** 跑 CLI）；可选手写 `src/scanner.c` 处理难点词法（长注释、`--[[ ]]`、字符串等）。 |
| **生成物** | `tree-sitter generate` 产出 **C 语言** `parser.c` 等（不是 C++）；由运行时 / 绑定编译链入。 |
| **校验** | `tree-sitter test`：维护 `test/corpus/*.txt`（片段 → 期望语法树），作为文法回归基石。 |
| **对外形态** | 独立 **tree-sitter 工程** 留在本目录；`../lsp` 通过 **同仓路径依赖**（如 Rust 的 `tree-sitter-xxx` 包装 crate + `build.rs` 编译生成 C）链接 parser，避免复制粘贴生成代码。 |

### 形式化语法规范

[`lua-emmy.bnf`](lua-emmy.bnf) 是本目录 Tree-sitter `grammar.js` 的 **形式化 EBNF 参考**，包含四部分：

| 段落 | 内容 |
|------|------|
| Part 1 — 词法 | 终结符定义：标识符、关键字、数值、字符串、注释、shebang、运算符 |
| Part 2 — Lua 语法 | 基于 Lua 5.4 §9 的完整可执行语法，表达式按 12 级优先级分层 |
| Part 3 — EmmyLua | `---` 注释内结构化子语法：`@class`/`@field`/`@param`/`@return`/`@type`/`@alias`/`@generic` 等 15+ 注解 + 类型表达式 |
| Part 4 — 实现映射 | 外部 scanner、`prec` 映射、`conflicts`、节点命名等 Tree-sitter 落地说明 |

实现 `grammar.js` 时应以此 BNF 为准；若文法演进，先更新 BNF 再改代码。

### 文法设计要点

- **Lua**：以 5.3+ 为准；与 5.4 差异点用注释或 feature 在文法中标注，便于日后开关。
- **EmmyLua**：在 `---` 文档/语义注释内拆出独立规则（`@class`、`@field`、`@param`、`@return`、`@type`、`@alias` 等），产出可查询节点，供 LSP 注解层绑定；勿把 Emmy 标成「运行时 Lua 关键字」。
- **错误恢复**：依赖 Tree-sitter 默认错误恢复行为；复杂 case 在 corpus 里固定。
- **Column-0 块边界**（定制扩展）：当关键字或标识符出现在行首（column 0）时，强制关闭所有未配对的嵌套块。缺少 `end` 的错误在下一个顶层语句处就能报出，而非等到 EOF。**代价**：嵌套代码必须缩进（至少 1 空白），否则会报错。详见 [`lua-emmy.bnf`](lua-emmy.bnf) §2.1.1。

### 当前实现状态

| 文件 | 说明 |
|------|------|
| `grammar.js` | Lua 5.3+/5.4 完整可执行语法（15 种语句、12 级优先级表达式、table/function/prefix 表达式）；EmmyLua 注解结构已定义（grammar.js 中包含产生式，但当前作为 comment token 统一扫描）。 |
| `src/scanner.c` | 外部扫描器：短字符串（含全部 Lua 5.3+ 转义序列）、长字符串 `[=[...]=]`、所有注释类型（短注释 / 长注释 / `---` 文档注释）、shebang、column-0 块边界检测。 |
| `test/corpus/` | 37 个回归测试（语句 + 表达式 + column-0 边界），100% 通过。 |

已通过以下文件的无错误解析验证：`tests/lua-root/test.lua`、`tests/lua-root/json.lua`、`assets/lua5.4/*.lua`（全部 11 个标准库桩文件）。

### 依赖与命令

```bash
cd grammar
npm install               # 安装 tree-sitter-cli
npx tree-sitter generate  # 从 grammar.js 生成 parser
npx tree-sitter test      # 运行 test/corpus/ 回归测试
npx tree-sitter parse <file.lua>  # 解析单个文件查看 CST
```

### 与 `lsp/` 的边界

- **grammar**：只回答「长什么样、树节点是什么」。
- **lsp**：把树 + 工程信息变成定义、引用、类型、诊断；不在此目录写语义。

更后排期见 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md) 阶段 A。
