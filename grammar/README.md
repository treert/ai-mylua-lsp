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

### 文法设计要点

- **Lua**：以 5.3+ 为准；与 5.4 差异点用注释或 feature 在文法中标注，便于日后开关。
- **EmmyLua**：在 `---` 文档/语义注释内拆出独立规则（`@class`、`@field`、`@param`、`@return`、`@type`、`@alias` 等），产出可查询节点，供 LSP 注解层绑定；勿把 Emmy 标成「运行时 Lua 关键字」。
- **错误恢复**：依赖 Tree-sitter 默认错误恢复行为；复杂 case 在 corpus 里固定。

### 依赖与命令（落地时）

- 安装：`tree-sitter` CLI（[tree-sitter 文档](https://tree-sitter.github.io/tree-sitter/creating-parsers)）。
- 日常：`tree-sitter generate`、`tree-sitter test`；CI 与 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md) 单仓契约一致。

### 与 `lsp/` 的边界

- **grammar**：只回答「长什么样、树节点是什么」。
- **lsp**：把树 + 工程信息变成定义、引用、类型、诊断；不在此目录写语义。

更后排期见 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md) 阶段 A。
