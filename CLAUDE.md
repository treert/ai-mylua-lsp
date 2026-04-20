# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 必读文档（强制）

在回答本仓库相关的实现、排错、重构或规划之前，请先阅读：

1. [`ai-readme.md`](ai-readme.md) — AI 会话入口、关键路径表、已实现 LSP 能力完整清单（该文件非常长且信息密度极高，相当于项目第二份 README）
2. [`docs/README.md`](docs/README.md) — 按主题进入 `docs/` 下的需求 / 架构 / 路线图 / 索引架构 / LSP 语义规范 / 性能分析

这两份文件回答了 90% 关于「为什么这么设计」「当前实现到哪一步」的问题，不读它们直接动手会重复已被否决或已落地的讨论。

## 文档同步（强制）

代码改动涉及以下情况时，**必须在同一次提交中**更新对应文档：

- **新增/删除/重构 LSP 能力** → 更新 `ai-readme.md` 的「已实现 LSP 能力」列表
- **改变架构或数据流**（索引策略、模块边界、数据模型）→ 更新 `docs/architecture.md` 或 `docs/index-architecture.md`
- **改变阶段完成状态或技术路线** → 更新 `docs/implementation-roadmap.md`

纯 bug 修复、纯重构（对外行为不变）、配置微调不需要更新文档。

## 仓库布局（Monorepo）

| 目录 | 角色 | 技术栈 |
|------|------|--------|
| `grammar/` | Tree-sitter 文法（Lua 5.3+） | `grammar.js` + 手写 `src/scanner.c`；Node.js CLI 生成 C parser |
| `lsp/` | 语言服务器（Cargo workspace） | Rust + `tower-lsp-server` 0.23 + `tree-sitter` 0.26 + `tokio` + `rayon` + `globset` |
| `vscode-extension/` | VS Code 扩展 | TypeScript + `vscode-languageclient`；自研 TextMate grammar |
| `tests/` | Lua fixture（非 Rust 代码） | 被 `lsp/` 的集成测试 `setup_workspace_from_dir` 读取；`tests/lua-root/` + `tests/lua-root2/` 是手工 EDH 端到端测试目录 |
| `docs/` | 设计文档 | — |

三个 subproject **可以独立构建与发布**；LSP 不依赖 VS Code API，可被任意 LSP 客户端使用。

## 常用命令

### Grammar（Tree-sitter）

```bash
cd grammar
npm install               # 首次
npx tree-sitter generate  # grammar.js → src/parser.c（改 grammar.js 后必跑）
npx tree-sitter test      # 跑 test/corpus/*.txt 回归（37 项，100% 通过）
npx tree-sitter parse <file.lua>  # 查看单文件 CST
```

**`lsp/crates/tree-sitter-mylua/build.rs` 编译的是 `grammar/src/parser.c`**，所以改 `grammar.js` 后要先 `tree-sitter generate` 再 `cargo build`。

### LSP（Rust）

```bash
cd lsp
cargo build               # debug 二进制 target/debug/mylua-lsp
cargo build --release     # release（打包进 .vsix 的产物）
cargo test --tests        # 跑所有集成测试（434 条，全绿）

# 跑单个测试文件
cargo test --test test_hover
# 跑单个测试函数
cargo test --test test_hover -- test_hover_dotted_chain_middle --exact --nocapture
```

**Windows 特有坑**：VS Code 扩展调试运行中会锁住 `target/debug/mylua-lsp.exe`，此时 `cargo test` 会失败。用独立 target 目录绕过：

```powershell
$env:CARGO_TARGET_DIR="target-test"; cargo test --tests
```

### VS Code Extension

```bash
cd vscode-extension
npm install
npm run compile           # tsc -p ./ → out/
npm run watch             # tsc -watch 增量
npm run build:local       # 一键：检测 host target → cargo build --release → vsce package；产出 .vsix 在本目录
npm run release           # build:local + vsce publish（需 VSCE_PAT 或 vsce login）
```

调试：在 VS Code 中按 **F5** 启动 Extension Development Host。

### 一键冒烟：构建 LSP + 扩展并打开 EDH

**macOS/Linux**:
```bash
.cursor/scripts/test-extension.sh [--skip-build|--skip-lsp|--skip-ext]
```

**Windows**:
```powershell
.cursor/scripts/test-extension.ps1 [-SkipBuild|-SkipLsp|-SkipExt]
```

脚本自动：`cargo build` → `npm run compile` → kill 旧 EDH → 以 `tests/lua-root/` + `tests/lua-root2/` 为 workspace 拉起新 EDH。当用户说「测试一下扩展」「重启 EDH」「试试改动」时首选这个脚本（见 [`.cursor/skills/test-extension/SKILL.md`](.cursor/skills/test-extension/SKILL.md)）。

## 架构大图

### 数据流

```
*.lua (disk/buffer)
  ↓ tree-sitter 增量 reparse (tree.edit + parse(new, Some(old)))
CST (tree-sitter)
  ↓ summary_builder.rs（单趟 AST walk）
DocumentSummary { globals, types, functions, tables, module_return, call_sites, is_meta, ... }
  ↓ aggregation.rs（WorkspaceAggregation 合并所有 summary）
GlobalShard + TypeShard + RequireByReturn + TypeDependants
  ↓ resolver.rs（跨文件 stub 链式解析 + 缓存 + 环路保护）
LSP handler（hover / goto / completion / references / diagnostics / ...）
  ↓
publishDiagnostics 经 DiagnosticScheduler（hot/cold VecDeque + 300ms debounce + 单消费者）
```

**关键设计取舍**：
- 解析与高亮：**自研 Tree-sitter 在 LSP 内**负责语法树；**自研 TextMate** 在扩展内做基色；**LSP semantic tokens 只补 TextMate 无法静态判定的语义差**（全局/局部、Emmy 类型名、`defaultLibrary`）——**刻意不做** token 细分（`keyword` / `number` / `string` / 注释等交给 TextMate）
- 全工作区能力：goto / references / workspace symbol 是**硬性目标**，不是「仅打开文件」
- 性能指标：5 万 Lua 文件级别的工作区；冷启动 rayon 并行解析 + 50 文件/批流式 merge + 磁盘 summary cache（`.vscode/.cache-mylua-lsp/`）
- Column-0 块边界（grammar 自研扩展）：行首关键字强制关闭嵌套块，缺 `end` 的错误在下一个顶层语句即时报出；代价是嵌套代码必须缩进。详见 `grammar/lua.bnf` §2.1.1

### LSP crate 布局（lib + bin 拆分）

```
lsp/crates/mylua-lsp/
  src/lib.rs      # library crate：导出所有模块 + Backend 定义（集成测试直接调用）
  src/main.rs     # binary crate：薄入口，仅 main() + stdio LSP server
  src/*.rs        # 24 个模块：scope / goto / hover / references / workspace_symbol /
                  # emmy / aggregation / workspace_scanner / completion / rename /
                  # semantic_tokens / diagnostics / symbols / types / type_system /
                  # table_shape / resolver / summary / summary_builder / config /
                  # diagnostic_scheduler / util / document / logger 等
  tests/          # Rust 集成测试（~30 个文件，434 条测试）；test_helpers.rs 提供
                  # parse_doc / setup_single_file / setup_workspace_from_dir
```

**写新 LSP 能力时优先走集成测试**：不需要启动 stdio LSP server，直接从 `setup_workspace_from_dir("tests/xxx")` 拉起 Backend 状态后调用 handler 函数。模板见 `tests/test_hover.rs` 等。

### 扩展侧二进制查找顺序（[`vscode-extension/src/extension.ts`](vscode-extension/src/extension.ts)）

1. `mylua.server.path` 用户配置（string 或 `{darwin,linux,win32}` 对象）
2. **Development 模式**（F5 EDH）→ `<extensionPath>/../lsp/target/debug/<bin>`，**绕过** `server/` 目录，避免 `npm run prepackage` 的陈旧拷贝掩盖 `cargo build` 新产物
3. Production → `<extensionPath>/server/<bin>`（.vsix 内置）

## 完成代码修改后的自动流程

每次完成一组**功能性代码修改**后、向用户确认完成之前：

1. **编译验证**
   - Rust：`cd lsp && cargo build`（零新增 error；warning 尽量消除）
   - TypeScript：`cd vscode-extension && npm run compile`
2. **调用 code-reviewer 子代理**（[`.cursor/agents/code-reviewer.md`](.cursor/agents/code-reviewer.md) 定义的角色）：提供变更文件列表（`git diff --name-only`）与改动目标
3. BLOCKING 问题全部修复后再向用户告知完成

跳过条件：单行修改、注释/文档变更、配置文件微调。

## 平台 / 位置编码细节

- LSP **Position 按 UTF-16 code unit** 语义处理（LSP 规范要求）。tree-sitter 提供字节列，`util::byte_col_to_utf16_col` / `utf16_col_to_byte_col` 做转换。中文 / emoji 行的 hover / goto / semantic token 依赖这层转换对齐
- 平台二进制名：`win32` 用 `mylua-lsp.exe`，其他 `mylua-lsp`
- 跨 OS 打包不可行：macOS → Windows 需要 `link.exe`（MSVC），Windows → macOS 不可合法分发 Apple SDK。要一次发所有平台用 `.github/workflows/release.yml`（5 target 矩阵）
