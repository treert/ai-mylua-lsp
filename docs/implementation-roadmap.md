# 实现路线图与方案讨论

本文档约束 **交付顺序**、**子工程协作方式** 与 **技术选型倾向**。需求见 [`requirements.md`](requirements.md)；架构见 [`architecture.md`](architecture.md)。

**当前阶段：需求分析 + 仓库骨架**（方案趋于：自研 Tree-sitter 文法、Extension 与 LSP 分离、**workspace-wide** 能力为硬性目标）。顶层 `grammar/`、`lsp/`、`vscode-extension/` 已建，待填入实现。

## 1. 原则

- **最佳方案优先**：在可维护性可接受的前提下，**工作区级定义/引用/符号** 与 **Tree-sitter 增量解析** 不因「先入为主 MVP」而降级为「仅打开文件」。
- **文法自主**：Tree-sitter 与 TextMate **各自自研**、独立演进；通过 **共享语言边界/version 策略** 与测试防止漂移；支持未来 **定制语法**。
- **扩展与 LSP 解耦**：接口为 **LSP + 配置 + grammar 版本**；两边可 **并行开发**、独立 CI。
- **早期正确性**：`$/cancelRequest`、增量索引失效图、配置契约从第一阶段就纳入设计，避免后期重写。

## 2. 仓库与协作模型（已定）

- **代码组织**：采用 **单一 Git 仓库（Monorepo）** 管理 Tree-sitter 文法、LSP Server、VS Code 扩展及相关文档/脚本。
- **建议顶层布局**（实现落地时可微调，需在本文与 [`architecture.md`](architecture.md) 同步）：
  - `grammar/` — Tree-sitter 工程（`tree-sitter test`、生成 parser，供 LSP 链接）。
  - `lsp/` — 语言服务器（构建产物如单一可执行文件）。
  - `vscode-extension/` — 扩展（TextMate、配置、启动同仓库内或已安装的 LSP 二进制）。
- **版本与发版**：同一 Monorepo 内为各包维护 **协调版本**（例如 Changesets、`release-please` 或手动的「一统版本号」）；对外仍可同时发布 **独立协议/扩展市场包**，但 **源码与 PR 以单仓为界**。
- **多仓库**：非当前选型；若将来拆分，另起 ADR，不再作为默认假设。

**CI 契约（单仓）**：`grammar/` 变更 → 触发 LSP 解析/golden 测试；`lsp/` 合并前跑 workspace-wide 集成测试；`vscode-extension/` 发包或打 VSIX 前对齐 **本仓库构建的 LSP 产物**（或锁定的 release 标签）。支持按路径过滤（path filter）加速 PR 检查。

## 3. 阶段划分（调整项）

### 阶段 A — 骨架：Tree-sitter（LSP）+ TextMate（扩展）+ LSP 宿主

- **Tree-sitter 文法**（LSP 内）：Lua 5.3+ 核心 + EmmyLua 注释节点；可运行 `tree-sitter test`。
- **LSP**：`initialize`、文档同步、基于 Tree-sitter 的 **基础语法诊断**；单文件 `documentSymbol`；协商 **semantic tokens**（可先空实现再填满）。
- **Extension**：自研 **TextMate** grammar（**基色高亮**）；拉起 LSP；配置 schema；**semanticTokenScopes** 等默认主题映射（随能力迭代）。

### 阶段 B — 语义与工作区 MVP

- EmmyLua 绑定层；**跨文件定义**；Hover。
- **轻量全库索引**（路径 + `require` 边）；为 workspace-wide 打基础。

### 阶段 C — Workspace-wide 完整能力 + 5 万文件硬化

- **`textDocument/references`**、**`workspace/symbol`** 达到需求文档硬性标准；增量索引与 **内存/延迟** 调优；压测 5 万文件。
- **语义诊断** 分级与后台调度固化。

### 阶段 D — 体验与扩展

- **Semantic tokens** 与语义层充分协同（全局变量等着色、modifier 定制）；rename、completion 等可按产品优先级追加。
- **定制语法** 试点：在 grammar 中增加受控扩展点并走通一条 LSP 特性（证明文法—语义契约）。

## 4. 技术栈倾向（需求分析结论草案）

**说明**：Tree-sitter 官方提供 **C 解析器** 与 **多语言绑定**，**不指定**「LSP 必须用哪种语言」；下表是 **本仓库** 在性能、分发、Monorepo 集成上的倾向，可随团队栈调整。

| 层级 | 倾向 | 说明 |
|------|------|------|
| **文法** | **自研 tree-sitter** | 与需求一致；与 LSP 同版本锁定。 |
| **LSP 运行时** | **Rust 或 Go（本仓库优先 Rust）** | 与绑定/FFI、单二进制分发、`spawn` 集成友好；**全工作区索引** 场景下资源可控性易做好；**非** Tree-sitter 官方对应用层的推荐。 |
| **Extension** | **TypeScript** | VS Code 官方路径；thin；通过子进程启动 LSP 二进制。 |
| **若坚持用 TypeScript 写 LSP** | 须单独论证 | Tree-sitter `node-addon` / WASM / 子进程隔离、以及 **5 万文件索引** 的内存画像；除非团队约束极强，否则不作为本仓库默认。 |

定稿后在本文 **删除「草案」措辞** 并记录 **选定语言与项目内理由**（与官方 Tree-sitter 文档区分）。

## 5. 验收检查点（阶段门禁）

- **A**：grammar 测试绿；LSP 可被扩展拉起；单文件解析与大纲可信。
- **B**：跨 `require` 定义 + Hover；依赖图可增量更新。
- **C**：**全工作区** references 与 workspace symbol 在大型夹具上 **正确性与性能** 达标；5 万文件场景通过约定门禁。
- **D**：体验项与定制语法试点按产品列表 closure。

## 6. 文档维护

- 阶段推进、**Monorepo 布局** 或技术栈定稿变更时，更新本文，并同步 [`architecture.md`](architecture.md)。
