# 需求说明

功能与非功能约定边界。实现细节见 [`architecture.md`](architecture.md)。

## 1. 产品形态

- **分体交付**：VS Code Extension 与 LSP Server 独立工程、独立版本，可并行开发
- **LSP 通用性**：LSP Server 不依赖 VS Code API，可被任意标准 LSP 客户端使用
- **Monorepo**：grammar / lsp / vscode-extension 同仓管理

## 2. Lua 语言版本

- **仅支持 Lua 5.3+**（含 5.3、5.4 及后续兼容版本）
- 不支持 Lua 5.1 / 5.2、LuaJIT 专有语法
- 标准库按 5.3+ 行为建模

## 3. 功能需求

### 3.1 语法高亮与解析

三层分工：

| 层 | 技术 | 职责 |
|---|------|------|
| 基色 | TextMate grammar（自研） | 关键字、字符串、数字、注释等静态着色 |
| 语法树 | Tree-sitter parser（自研） | 结构化 AST，供 LSP 做语义分析 |
| 语义着色 | LSP semantic tokens | TextMate 基色的薄补丁：全局/局部区分、方法/函数区分、Emmy 类型名等 |

**原则**：semantic tokens 刻意最小化，只发 TextMate 无法静态判断的少量 token。

### 3.2 语义跳转与工作区范围

全工作区语义下可用（受 include/exclude 配置约束）：

- goto definition / declaration / typeDefinition
- references（全工作区引用查找）
- workspace/symbol（全库符号搜索）
- EmmyLua 类型侧导航（`@class` / `@alias` 名跳转）

**工作区语义模型**：工作区内所有 `.lua` 文件视为已加载进同一全局环境（遵守 `local` 块作用域）。某文件顶层产生的全局名对其他文件默认可见，无需 `require`。

**require 绑定**：`local <name> = require("<模块>")` 时，将 `<name>` 绑定到目标文件的 `return` 表达式。字符串 → 文件的解析可配置（根目录、别名等）。

### 3.3 Hover

- 展示签名、文档注释
- 合并 EmmyLua 注解信息（`@param` / `@return` / `@type`）
- 类型推断：尽量提供有用信息，不确定则不报

### 3.4 诊断

- **语法诊断**：必须
- **语义诊断**：支持开关与严重级别配置，与 5 万文件规模下的调度策略协同（后台、分阶段、可取消）

### 3.5 大纲（Document Symbol）

- 函数、表字面量命名字段、`local function` 等可导航结构
- EmmyLua `@class` 等作为分组或附加元数据

### 3.6 自研解析

- Tree-sitter 文法为 LSP 侧词法/句法树的唯一来源
- 文法支持版本化扩展（附加产生式、externals、变体 feature flag）
- Syntax tree → 语义模型 builder 接口稳定可测试

## 4. EmmyLua 注释

与业界 EmmyLua 风格注解兼容：

`@class` / `@field` / `@param` / `@return` / `@type` / `@alias` / `@generic` / `@enum` / `@overload` / `@meta` / `@diagnostic`

不保证与某一固定发布版工具字节级一致，以实用互通为准。

## 5. 非功能需求

| 维度 | 目标 |
|------|------|
| 规模 | 单工作区 ~5 万个 Lua 文件可日常使用 |
| 冷启动 | 全量索引达到 workspace-wide 查询可用，支持渐进增强 |
| 交互延迟 | 当前文件 definition/hover 低延迟；全工作区操作可取消、有进度 |
| 内存 | 有上限与淘汰策略；索引为 workspace-wide 查询优化 |

## 6. 配置

- Lua 版本假定、require 路径与别名、诊断开关与级别、include/exclude glob 等
- 配置 schema 与 LSP `initializationOptions` / `workspace/didChangeConfiguration` 对齐

## 7. 文档维护

- 变更 **架构、数据流、索引策略、文法边界或对外配置项** 时，同步更新 [`architecture.md`](architecture.md)、[`lsp-semantic-spec.md`](lsp-semantic-spec.md)（若跨文件行为变化）与本文。
