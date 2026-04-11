# 文档中心

本目录集中维护与本项目相关的说明文档，便于新成员与 **AI 会话**快速建立上下文。

**状态**：当前为 **需求分析阶段**；仓库顶层已建 **Monorepo 目录**（`grammar/`、`lsp/`、`vscode-extension/`），见根目录 [`README.md`](../README.md)。方案要点包括 **LSP 内自研 Tree-sitter（解析树）**、**扩展内自研 TextMate（基色）+ LSP semantic tokens（语义着色）**、**Extension 与 LSP 分离并行开发**、**全工作区（workspace-wide）定义/引用/符号**。

## 索引

| 文档 | 内容 |
|------|------|
| [`requirements.md`](requirements.md) | 功能/非功能需求、Tree-sitter 主线、workspace-wide 硬性范围、文法自主与定制演进 |
| [`architecture.md`](architecture.md) | Extension / LSP / Grammar 三分解、数据流；跨文件索引 **概要** |
| [`index-architecture.md`](index-architecture.md) | **索引内部架构**：数据模型（DocumentSummary / 聚合层）、两层推断与惰性解析、类型推断与 Table Shape、链式追踪、索引构建与维护（冷启动 / 增量 / 签名指纹 / 持久化）（**讨论稿**） |
| [`lsp-semantic-spec.md`](lsp-semantic-spec.md) | **LSP 语义能力需求**：Lua/EmmyLua 语义约定（全局已见 / require 绑定 / Emmy 类型名）、LSP 能力消费（goto / hover / references / diagnostics / symbol）、候选决议与配置项（**讨论稿**） |
| [`implementation-roadmap.md`](implementation-roadmap.md) | 阶段门禁、**已定 Monorepo** 布局与 CI、技术栈倾向（Rust/Go LSP + TS 扩展） |

仓库级入口与 AI 规则见根目录 [`ai-readme.md`](../ai-readme.md)。
