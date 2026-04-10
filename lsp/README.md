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

### 若采用 Rust：建议技术栈（可随调研微调 crate 名）

| 层级 | 建议 |
|------|------|
| **异步运行时** | **tokio**（stdio 上读写字节流、可取消任务）。 |
| **LSP 框架** | **tower-lsp**（或同等：封装 `initialize`、按需实现 `textDocument/*`、`workspace/*`）。 |
| **协议类型** | **lsp-types**（与框架配套）。 |
| **解析** | **tree-sitter** crate + **同仓 grammar 包装 crate**（path 指向 `../grammar` 生成源码，由 `build.rs` 调用 `cc` 编译 **C parser**）。 |
| **序列化** | **serde** / **serde_json**。 |

### 工程形态（Monorepo）

- 本目录可为 **单 package** `lsp/`（根上 `Cargo.toml`）或 `lsp/crates/mylua-lsp` + `lsp/crates/tree-sitter-mylua`，便于 **path 依赖** grammar；选定后更新 `Cargo.toml`，并同步 [docs/architecture.md](../docs/architecture.md)。
- **产物**：一个可执行文件（如 `mylua-lsp`），默认 **stdio**；保留日后 **socket** 的扩展空间。
- **`vscode-extension`**：配置中指定该二进制路径（开发时指向 `cargo run` / `target/debug`，发布时随扩展打包或文档约定安装路径）。

### 实现顺序（与路线图一致）

1. **阶段 A**：`initialize`、文档同步、Tree-sitter 解析 + 基础语法诊断、`documentSymbol`；协商 **semantic tokens**（可先占位）。
2. **阶段 B**：Emmy 注解绑定、跨文件定义、Hover、轻量全库索引。
3. **阶段 C**：全工作区 **references**、**workspace/symbol**、规模与增量硬化。

详见 [docs/implementation-roadmap.md](../docs/implementation-roadmap.md)。

### 与 `grammar/` 的边界

- **grammar** 只更新句法树；**lsp** 负责 **ANN（注解绑定）**、**索引**、**LSP 行为**。
- grammar 版本 bump 时，此处跑 **集成测试 / golden**，避免静默树形变更。
