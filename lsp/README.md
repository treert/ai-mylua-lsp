# lsp

Rust 实现的 Lua 语言服务器（标准 LSP / JSON-RPC），基于同仓 `../grammar` 的 Tree-sitter parser。不依赖 VS Code API，可被任意 LSP 客户端使用。

## 技术栈

Rust + `tower-lsp-server` 0.23 + `tree-sitter` 0.26 + `tokio`

## 工程结构

```
lsp/
  Cargo.toml                          # workspace root
  crates/
    tree-sitter-mylua/                # Tree-sitter 包装 crate（build.rs 编译 grammar/src/ 的 C parser）
    mylua-lsp/
      src/
        main.rs                       # 薄入口：启动 stdio LSP server
        lib.rs                        # 核心库：Backend 定义 + 所有模块导出
        bin/
          lua_perf.rs                 # CLI 工具：解析性能分析
      tests/                          # 集成测试（31 个测试文件）
```

### 核心模块

| 模块 | 职责 |
|------|------|
| `lib.rs` | Backend 定义、模块导出、核心方法（文档同步 / 索引触发） |
| `handlers.rs` | `impl LanguageServer for Backend`（所有 LSP handler） |
| `indexing.rs` | 冷启动扫描流水线、诊断消费者循环、辅助函数 |
| `scope.rs` | 作用域树构建与局部变量解析 |
| `summary.rs` / `summary_builder/` | 文件摘要结构 / AST → 摘要构建（7 个子模块） |
| `aggregation.rs` | 工作区聚合索引（全局符号表） |
| `workspace_scanner.rs` | 文件扫描 + require 路径解析 + module_index |
| `emmy.rs` | EmmyLua 注解解析（类型表达式、@class/@field/@param 等） |
| `resolver.rs` | 跨文件类型解析与推断 |
| `type_system.rs` / `type_inference.rs` | 类型系统定义 / 类型推断 |
| `diagnostics.rs` / `diagnostic_scheduler.rs` | 语法 + 语义诊断 / 调度（debounce + 双队列） |
| `goto.rs` / `hover.rs` / `references.rs` | 跳转 / 悬浮 / 引用查找 |
| `completion.rs` / `signature_help.rs` | 自动补全 / 签名帮助 |
| `rename.rs` / `call_hierarchy.rs` | 重命名 / 调用层次 |
| `semantic_tokens.rs` / `symbols.rs` | 语义着色 / 文档符号 + 工作区符号 |
| `config.rs` | 配置体系（20 项） |
| `document_link.rs` / `document_highlight.rs` | require 链接 / 高亮 |
| `folding_range.rs` / `selection_range.rs` / `inlay_hint.rs` | 折叠 / 选区 / 内嵌提示 |
| `summary_cache.rs` | 磁盘持久化缓存 |

### 架构要点

- **lib + bin 拆分**：`lib.rs` 导出核心逻辑，`main.rs` 为薄入口；集成测试直接调用 lib，无需启动 LSP 通信
- **索引状态机**：`Initializing` → `ModuleMapReady` → `Ready`，5 阶段冷启动流水线
- **增量解析**：tree-sitter `tree.edit` + `parse(new, Some(old))`
- **并发安全**：per-URI `edit_locks`，固定锁顺序避免死锁

> 详细架构见 [`docs/architecture.md`](../docs/architecture.md)、[`docs/index-architecture.md`](../docs/index-architecture.md)

## 构建与测试

```bash
# 前置：确保 grammar 已生成
cd grammar && npx tree-sitter generate

# 构建
cd lsp && cargo build

# 运行测试
cd lsp && cargo test --tests
```

## CLI 工具：`lua-perf`

独立命令行工具，分析 Lua 文件在各解析阶段（tree-sitter parse → build_file_analysis）的耗时，也可导出单文件 `DocumentSummary` JSON 方便检查索引摘要。

```bash
# 构建（必须 release 模式，debug 下耗时无参考意义）
cd lsp && cargo build --release --bin lua-perf

# 使用
cargo run --release --bin lua-perf -- /path/to/file.lua
cargo run --release --bin lua-perf -- file1.lua file2.lua  # 多文件
./target/release/lua-perf /path/to/file.lua                # 直接运行

# 导出 DocumentSummary
cargo run --release --bin lua-perf -- --summary /path/to/file.lua
cargo run --release --bin lua-perf -- --summary-out target/lua-summary /path/to/file.lua
cargo run --release --bin lua-perf -- --summary-stdout /path/to/file.lua
```

`--summary` 默认写入 `target/lua-summary/`，输出文件名由输入路径转义并追加稳定 hash 得到，例如 `tests/lua-root/diagnostics.lua` 会写为 `target/lua-summary/tests_lua-root_diagnostics.lua.<hash>.summary.json`，避免多文件模式下同名覆盖。`--summary-out <dir>` 可指定目录；`--summary-stdout` 仅支持单个输入文件。

## 与 `grammar/` 的边界

- **grammar** 只负责句法树（BNF + scanner）
- **lsp** 负责注解绑定、索引、语义分析、LSP 行为
- grammar 版本变更时，此处跑集成测试验证，避免静默树形变更
