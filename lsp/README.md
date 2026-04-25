# lsp

语言服务器：**标准 LSP**（JSON-RPC），基于同仓 `../grammar` 的 Tree-sitter parser，提供解析、索引、语义、诊断、**semantic tokens** 等；**不依赖 VS Code API**，可被任意 LSP 客户端使用。

## 实现方案说明

### Tree-sitter 官方到底「推荐」什么？

- **官方核心**：文法用 `grammar.js` 描述，`generate` 产出的是 **C 语言 parser**（+ 可选手写 `scanner.c`）；运行时以 **C 库** 形式链接。
- **宿主语言**：官方提供 **多语言绑定**（Rust、Node、Python、Go、…），方便你在 **应用里** 调 parser；**并未规定**「做 LSP 必须用某一种语言」。选 Rust / Go / Node / C++ 封装同一套 C 代码都可以。

### 本仓库当前倾向：用 Rust 写 LSP（项目选型，非 Tree-sitter 背书）

在下面约束下，我们把 **Rust 作为默认优先项** 写进文档，便于落地 Monorepo 与性能目标；**若团队更熟 Go / 其他语言，可替换，只要链上同一 grammar 产物即可。**

| 倾向 Rust 的理由（本仓库视角） |
|------------------------------|
| **tree-sitter** Rust crate 与各 grammar 的 **build.rs + cc** 集成在社区里很常见，编进生成好的 **C parser** 路径成熟。 |
| **单二进制** 分发，扩展 `spawn`、Neovim 等起进程简单。 |
| **全工作区索引**、大体量文件场景下，对 **内存与 CPU** 的可控性通常优于「纯 Node 主线程 + 大 JS 堆」的典型形态（非绝对，取决于实现）。 |

**其它常见选项（同等合法）**：**Go**（`go-tree-sitter` 等）、**Node/TypeScript**（`web-tree-sitter` 或 native 绑定，需注意大索引与事件循环）、**C/C++** 直接与 `libtree-sitter` 同进程链接等。

若坚持用 **TypeScript** 写 LSP：需单独评估 Tree-sitter **node-addon** / WASM / 子进程隔离及 **5 万文件索引** 内存画像；见 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md) §4。

### 技术栈（已定）

| 层级 | 选定 | 版本 |
|------|------|------|
| **异步运行时** | `tokio` | 1 |
| **LSP 框架** | `tower-lsp-server`（tower-lsp 社区活跃 fork） | 0.23 |
| **协议类型** | `ls-types`（tower-lsp-server 内置 re-export） | — |
| **解析** | `tree-sitter` crate + `tree-sitter-mylua`（同仓 path 依赖） | 0.26 |
| **序列化** | `serde` / `serde_json` | 1 |

### 工程结构（已实现）

```
lsp/
  Cargo.toml                        # workspace root
  crates/
    tree-sitter-mylua/              # 包装 crate：build.rs 编译 grammar/src/ 的 C parser
      Cargo.toml / build.rs / src/lib.rs
    mylua-lsp/                      # LSP server：library + thin binary
      Cargo.toml
      src/
        lib.rs                      # 核心库：导出所有模块，供集成测试直接调用
        main.rs                     # 薄入口：仅启动 stdio LSP server
      tests/                        # Rust 集成测试
        test_helpers.rs             # 测试工具：parse_doc / setup_single_file / setup_workspace_from_dir
        test_parse.rs               # 解析测试
        test_hover.rs               # Hover 测试
        test_completion.rs          # 补全测试
        test_goto.rs                # 跳转定义测试
        test_diagnostics.rs         # 诊断测试
        test_symbols.rs             # 文档符号测试
        test_references.rs          # 引用查找测试
        test_workspace.rs           # 多文件工作区集成测试
```

- **产物**：`mylua-lsp` 单一可执行文件，默认 **stdio** 通信。
- **构建**：`cd lsp && cargo build`（需先 `cd grammar && npx tree-sitter generate` 确保 `parser.c` 存在）。
- **测试**：`cd lsp && cargo test --tests`（见下方「独立测试框架」章节）。
- **`vscode-extension`** 通过 `spawn` 启动 `target/debug/mylua-lsp`（开发）或打包后的二进制（发布）。

### 当前实现（阶段 C 完成）

| LSP 能力 | 状态 |
|----------|------|
| `initialize` / `initialized` / `shutdown` | 已实现 |
| `textDocument/didOpen` / `didChange` / `didClose` | 已实现（Full sync + Tree-sitter 解析） |
| `textDocument/publishDiagnostics` | 已实现（ERROR / MISSING 节点转诊断） |
| `textDocument/documentSymbol` | 已实现（顶层 function / local / assignment 提取） |
| `textDocument/definition` | 已实现（local 作用域解析 + 全局符号表 + require 跳转） |
| `textDocument/hover` | 已实现（定义源码 + EmmyLua 注解 + 文档注释） |
| `textDocument/references` | 已实现（单文件 local scope + 全工作区全局引用） |
| `textDocument/completion` | 已实现（局部变量 + 全局名 + 关键字） |
| `textDocument/rename` | 已实现（prepare + 单文件 local + 全工作区全局） |
| `workspace/symbol` | 已实现（全局函数/变量模糊搜索） |
| `textDocument/semanticTokens/full` | 已实现（函数/变量/参数/关键字/字符串/数字/注释/运算符 + declaration/definition 修饰符） |
| `textDocument/publishDiagnostics`（语义） | 已实现（未定义全局变量 warning） |

**架构**：`lib.rs` 导出所有核心模块（library crate），`main.rs` 为薄入口（binary crate）。这种 lib + bin 拆分使核心逻辑可被集成测试直接调用，无需启动完整 LSP 通信。

**模块结构**（24 个模块）：`lib.rs`（库入口 + Backend 定义）、`main.rs`（binary 入口）、`scope.rs`（作用域解析）、`goto.rs`（跳转）、`hover.rs`（悬浮）、`references.rs`（引用查找）、`workspace_symbol.rs`（全库符号搜索）、`emmy.rs`（EmmyLua 注解解析）、`aggregation.rs`（工作区聚合索引）、`workspace_scanner.rs`（工作区文件扫描 + require 路径解析）、`completion.rs`（自动补全）、`rename.rs`（重命名）、`semantic_tokens.rs`（语义着色）、`diagnostics.rs`（语法 + 语义诊断）、`symbols.rs`、`types.rs`、`type_system.rs`、`table_shape.rs`、`resolver.rs`（跨文件类型解析）、`summary.rs`（文件摘要结构）、`summary_builder.rs`（AST → 摘要构建）、`config.rs`（配置体系）、`util.rs`、`document.rs`、`logger.rs`。

**工作区扫描**：`initialized` 时自动递归扫描所有 `.lua` 文件，构建 `require_map`（模块名 -> 文件 URI）和全局符号表。`didChangeWatchedFiles` 处理增量变更。

详见 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md)。

### CLI 工具：`lua-perf`（性能分析）

独立的命令行工具，用于分析 Lua 文件在各解析阶段的耗时，帮助定位性能瓶颈。

**构建**

```bash
cd lsp && cargo build --release --bin lua-perf
```

**使用**

```bash
# 分析单个文件
cargo run --release --bin lua-perf -- /path/to/file.lua

# 分析多个文件
cargo run --release --bin lua-perf -- file1.lua file2.lua file3.lua

# 或直接运行编译产物
./target/release/lua-perf /path/to/file.lua
```

> ⚠️ 请使用 `--release` 模式，debug 模式下的耗时数据没有参考意义。

**输出示例**

```
=== Performance breakdown for MoeGameCore-annotation.lua ===
  Path: /path/to/MoeGameCore-annotation.lua
  File size: 1234567 bytes, 45678 lines

[Phase 1] tree-sitter parse:   120 ms
  root node children: 12345
  root named children: 6789
  has_error: false
[Phase 2] build_summary:       350 ms
  global_contributions: 100
  type_definitions: 200
  table_shapes: 50
  call_sites: 30
  function_summaries: 80
[Phase 3] build_scope_tree:    80 ms

[Total]                        550 ms
  parse:   21.8%
  summary: 63.6%
  scope:   14.5%
=== End ===
```

**三个阶段说明**

| 阶段 | 说明 |
|------|------|
| Phase 1: tree-sitter parse | 将源码解析为 AST |
| Phase 2: build_summary | 从 AST 提取全局贡献、类型定义、函数摘要等索引信息 |
| Phase 3: build_scope_tree | 构建作用域树，用于局部变量解析 |

### 与 `grammar/` 的边界

- **grammar** 只更新句法树；**lsp** 负责 **ANN（注解绑定）**、**索引**、**LSP 行为**。
- grammar 版本 bump 时，此处跑 **集成测试 / golden**，避免静默树形变更。
