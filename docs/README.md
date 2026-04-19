# 文档中心

本目录集中维护与本项目相关的说明文档，便于新成员与 **AI 会话**快速建立上下文。

**状态**：需求已落地、阶段 C 完整实现。仓库为 **Monorepo**（`grammar/`、`lsp/`、`vscode-extension/`），见根目录 [`README.md`](../README.md)。方案要点：**LSP 内自研 Tree-sitter（解析树）**、**扩展内自研 TextMate（基色）+ LSP semantic tokens（语义着色）**、**Extension 与 LSP 分离并行开发**、**全工作区（workspace-wide）定义/引用/符号**。

## 索引

| 文档 | 内容 |
|------|------|
| [`requirements.md`](requirements.md) | 功能/非功能需求、Tree-sitter 主线、workspace-wide 硬性范围、文法自主与定制演进 |
| [`architecture.md`](architecture.md) | Extension / LSP / Grammar 三分解、数据流；跨文件索引 **概要** |
| [`index-architecture.md`](index-architecture.md) | **索引内部架构**：数据模型（DocumentSummary / 聚合层）、两层推断与惰性解析、类型推断与 Table Shape、链式追踪、索引构建与维护（冷启动 / 增量 / 签名指纹 / 持久化） |
| [`lsp-semantic-spec.md`](lsp-semantic-spec.md) | **LSP 语义能力**：Lua/EmmyLua 语义约定（全局已见 / require 绑定 / Emmy 类型名）、LSP 能力消费（goto / hover / references / diagnostics / symbol）、候选决议与配置项 |
| [`implementation-roadmap.md`](implementation-roadmap.md) | 阶段门禁（A/B/C 已完成、D 主体完成）、**已定 Monorepo** 布局与 CI、技术栈倾向（Rust/Go LSP + TS 扩展） |
| [`index-implementation-plan.md`](index-implementation-plan.md) | **索引架构落地实施步骤（历史归档）**：步骤 1–7 全部完成，作为类似量级改造的参考模板保留 |
| [`performance-analysis.md`](performance-analysis.md) | **性能现状评估**：架构亮点 + 5 万文件目标下剩余的 4 个瓶颈（冷启动 cache 同步 IO、tree-sitter 全量 reparse、documents 全驻内存、references 线扫）+ 规模分级表 + 三档优化路线图（Tier 1 低垂果实 / Tier 2 架构调整 / Tier 3 高级）+ 已落地变更简史 |
| [`future-work.md`](future-work.md) | **后续待办**（当前无已知待办）+ 新增条目模板 + 新增能力时的维护清单 |
| [`indexing-future-work.md`](indexing-future-work.md) | **索引系统优化方向（领域专题）**：`WorkspaceAggregation` 已知坑点（冷启动反向边丢失、fingerprint 粒度、annotation 误判、反向图 O(N) 查重、affected 漏收等 7 项）+ 泛型支持缺口（variance 忽略、函数级实参推断、上界约束、arity 校验等 6 项）+ 推荐落地顺序 |

**测试**：LSP 具备独立测试能力（无需 VS Code 联调），集成测试覆盖所有核心功能。详见 [`lsp/README.md`](../lsp/README.md) 和 [`ai-readme.md`](../ai-readme.md) 的「独立测试框架」章节。

仓库级入口与 AI 规则见根目录 [`ai-readme.md`](../ai-readme.md)。
