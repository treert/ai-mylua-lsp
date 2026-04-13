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
    mylua-lsp/                      # LSP server 可执行文件
      Cargo.toml / src/main.rs
```

- **产物**：`mylua-lsp` 单一可执行文件，默认 **stdio** 通信。
- **构建**：`cd lsp && cargo build`（需先 `cd grammar && npx tree-sitter generate` 确保 `parser.c` 存在）。
- **`vscode-extension`** 通过 `spawn` 启动 `target/debug/mylua-lsp`（开发）或打包后的二进制（发布）。

### 当前实现（阶段 A 完成）

| LSP 能力 | 状态 |
|----------|------|
| `initialize` / `initialized` / `shutdown` | 已实现 |
| `textDocument/didOpen` / `didChange` / `didClose` | 已实现（Full sync + Tree-sitter 解析） |
| `textDocument/publishDiagnostics` | 已实现（ERROR / MISSING 节点转诊断） |
| `textDocument/documentSymbol` | 已实现（顶层 function / local / assignment 提取） |
| `textDocument/semanticTokens/full` | 占位（capability 已声明，返回空 tokens） |

### 后续路线

1. **阶段 B**：Emmy 注解绑定、跨文件定义、Hover、轻量全库索引。
2. **阶段 C**：全工作区 **references**、**workspace/symbol**、规模与增量硬化。

详见 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md)。

### 与 `grammar/` 的边界

- **grammar** 只更新句法树；**lsp** 负责 **ANN（注解绑定）**、**索引**、**LSP 行为**。
- grammar 版本 bump 时，此处跑 **集成测试 / golden**，避免静默树形变更。
