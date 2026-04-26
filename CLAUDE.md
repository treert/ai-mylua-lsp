# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 必读文档（强制）

在回答本仓库相关的实现、排错、重构或规划之前，请先阅读：

1. [`ai-readme.md`](ai-readme.md) — 项目全貌：目标、仓库结构、开发进度、LSP 能力清单、架构特性、文档索引
2. [`docs/README.md`](docs/README.md) — 按主题进入 `docs/` 下的详细文档

这两份文件回答了 90% 关于「为什么这么设计」「当前实现到哪一步」的问题，不读它们直接动手会重复已被否决或已落地的讨论。

## 文档同步（强制）

代码改动涉及以下情况时，**必须在同一次提交中**更新对应文档：

- **新增/删除/重构 LSP 能力** → 更新 `ai-readme.md` 的「已实现 LSP 能力」列表
- **改变架构或数据流**（索引策略、模块边界、数据模型）→ 更新 `docs/architecture.md` 或 `docs/index-architecture.md`

纯 bug 修复、纯重构（对外行为不变）、配置微调不需要更新文档。

## 常用命令（Quick Ref）

### Grammar

```bash
cd grammar
npx tree-sitter generate  # grammar.js → src/parser.c（改 grammar.js 后必跑）
npx tree-sitter test      # 回归测试
```

**`lsp/crates/tree-sitter-mylua/build.rs` 编译的是 `grammar/src/parser.c`**，所以改 `grammar.js` 后要先 `tree-sitter generate` 再 `cargo build`。

### LSP（Rust）

```bash
cd lsp
cargo build               # debug
cargo test --tests         # 全量集成测试
cargo test --test test_hover -- test_hover_dotted_chain_middle --exact --nocapture  # 单个用例
```

### VS Code Extension

```bash
cd vscode-extension
npm run compile            # tsc
npm run build:local        # cargo build --release + vsce package → .vsix
```

### 一键冒烟

```bash
.cursor/scripts/test-extension.sh [--skip-build|--skip-lsp|--skip-ext]   # macOS/Linux
.cursor/scripts/test-extension.ps1 [-SkipBuild|-SkipLsp|-SkipExt]       # Windows
```

## 完成代码修改后的自动流程

每次完成一组**功能性代码修改**后、向用户确认完成之前：

1. **编译验证**
   - Rust：`cd lsp && cargo build`（零新增 error；warning 尽量消除）
   - TypeScript：`cd vscode-extension && npm run compile`
2. **调用 code-reviewer 子代理**（`.cursor/agents/code-reviewer.md`）：提供变更文件列表与改动目标
3. BLOCKING 问题全部修复后再向用户告知完成

跳过条件：单行修改、注释/文档变更、配置文件微调。

## 平台 / 位置编码细节

- LSP **Position 按 UTF-16 code unit** 语义处理（LSP 规范要求）。tree-sitter 提供字节列，`util::byte_col_to_utf16_col` / `utf16_col_to_byte_col` 做转换
- 平台二进制名：`win32` 用 `mylua-lsp.exe`，其他 `mylua-lsp`
- 跨 OS 打包不可行；多平台发布走 `.github/workflows/release.yml`（5 target 矩阵）
