# Future Work — 后续待办与维护提示

> **当前状态：无已知待办。**
>
> 过往的待办项（诊断扩展、selection_range 精细化、signature_help shape-owner、callHierarchy、documentLink、foldingRange 分支折叠、semantic tokens delta、`---@meta`、`fun()` 多返回 + `self` 泛型绑定 等）已全部落地。各能力的实现细节与测试覆盖记录在 [`../ai-readme.md`](../ai-readme.md) 的「已实现 LSP 能力」章节与 `lsp/crates/mylua-lsp/tests/` 下的集成测试文件中，commit 历史（按 P0/P1/P2 编号归类）可通过 git log 追溯。
>
> 本文档只保留 **真正还没做** 的方向；已实现的不再列在这里。

---

## 1. 待办清单

暂无。

新发现的方向追加到这里时请按以下模板：

```markdown
### <简短标题>

- **动机**：为什么要做
- **影响范围**：涉及的模块 / 数据结构 / 对外能力
- **验收**：什么条件下认为做完
- **风险 / 默认开关**：是否需要 opt-in、对既有行为的影响
```

同时在 [`README.md`](README.md) 的文档索引行同步一句话描述。

---

## 2. 维护提示（新增能力时的清单）

- **新增诊断类别**：在 `DiagnosticsConfig` 加字段 + 默认 severity；默认开启时需在 fixture 上跑一遍确认不会在真实项目上产生大量噪声
- **新增 LSP capability**：在 `lib.rs::initialize` 的 `ServerCapabilities` 声明 + async handler；独立的 `src/<feature>.rs` 模块 + 对应集成测试文件
- **代码修改后**：按 [`../.cursor/rules/code-review-after-changes.mdc`](../.cursor/rules/code-review-after-changes.mdc) 跑构建验证 + code-reviewer
- **文档同步**：对外能力变动同步 [`../ai-readme.md`](../ai-readme.md)「已实现 LSP 能力」章节；架构/数据流变动同步 [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md)
