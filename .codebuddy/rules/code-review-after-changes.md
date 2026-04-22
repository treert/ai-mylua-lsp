---
# Please note: Do not modify the header of this document. If modified, CodeBuddy (Internal Edition) will apply the default logic settings.
type: always
---
# 代码修改后自动 Review

每次完成一组**功能性代码修改**后（即将告知用户"完成"之前），必须执行以下流程。

## 跳过条件

以下场景**不需要** review：单行修改、注释/文档变更、配置文件微调。

## 流程

### 第 1 步：编译验证

运行构建命令，确保**零新增 error**；新增 warning 须评估并尽量消除。

- Rust：`cd lsp && cargo build`
- TypeScript：`cd vscode-extension && npm run compile`

### 第 2 步：代码审查

按照 `.codebuddy/agents/code-reviewer.md` 中定义的审查维度和分级标准，对本次变更进行审查。审查时需要：

1. 获取**变更文件列表**（通过 `git diff --name-only` 获取）
2. 明确**本次改动的目标**（用户需求摘要）
3. 主动读取相关代码文件，理解上下文后再判断
4. 按审查维度逐项检查，按问题分级输出报告

### 第 3 步：处理 Review 结果

- **APPROVED** → 告知用户完成
- **CHANGES_REQUESTED** → 修复所有 BLOCKING 问题后重新审查，直到通过