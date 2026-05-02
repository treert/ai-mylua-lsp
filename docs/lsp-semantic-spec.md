# LSP 语义规范

定义 Lua/EmmyLua 的语义约定，以及各 LSP 能力如何消费索引数据。

索引数据模型详见 [`index-architecture.md`](index-architecture.md)。

---

## 1. 语义模型与名字解析

### 1.1 全局可见性

- 工作区内各文件对全局环境的贡献进入合并视图（遵守 `local` / 块作用域）。
- **不要求**先 `require` 才能看见其它文件的全局符号。
- 同名冲突保留候选列表，按打分选最佳候选。

### 1.2 `require` 绑定

- 模式：`local <name> = require(<静态字符串>)`
- 路径解析：模块串 → 目标 URI（`?.lua`、`require.aliases` 别名替换）
- 语义：`<name>` 绑定到目标文件 `return` 的模块值
- 反向索引：`(目标 URI) → [(来源文件, 局部名), …]`
- 非静态 `require`、拼接路径不建绑定

### 1.3 Emmy 类型名

`---@class`、`---@alias` 等进入工作区类型表。解析顺序：本文件 → 工作区。

### 1.4 标识符解析流程

1. **Lua 作用域**（`local`、块、闭包）
2. 若为 `require` 绑定 → 目标文件 `return` + 模块摘要
3. 若为全局自由名 → 全局合并表
4. Emmy 类型名 → 本文件类型表 → 工作区类型表

此流程是 `goto`、`hover`、`references` 的共同入口。

---

## 2. LSP 能力消费索引

### 2.1 goto definition / hover

| 场景 | 查询路径 | 复杂度 |
|------|---------|--------|
| 局部变量 | 当前文件摘要 | O(1) |
| `require` 绑定 | 绑定表 | O(1) |
| 全局名 / 类型名 | 分片查找 | O(1) |
| 链式字段 `obj.pos.x` | 逐段类型解析 | O(链长) |

多候选时按打分选最佳候选直接跳转，分数接近则展示候选列表。打分优先级：Emmy 定义 > 显式注解 > shape 推断。

策略由 `mylua.gotoDefinition.strategy` 控制（`auto` / `single` / `list`）。

### 2.2 references

- 查找与光标同一语义目标的所有引用，而非同名文本匹配。
- 内部区分 `read` / `write` / `readwrite` 引用类型。
- 响应时按 `includeDeclaration` 参数裁剪。

**身份模型**：

| 语义类别 | 主查询身份 |
|---------|-----------|
| 局部变量 | `LocalSymbolId`（闭包捕获沿用） |
| 全局变量 | `GlobalNodeId` |
| Emmy 字段 | `TypeId + FieldName` |
| table shape 字段 | `TableShapeId + FieldKey` |
| 全局 table 字段 | `GlobalNodeId + FieldKey` |

策略由 `mylua.references.strategy` 控制（`best` / `merge` / `select`）。

### 2.3 workspace/symbol

**收录范围**：

| 收录 | 不收录 |
|------|-------|
| 全局变量、全局函数 | 局部变量 |
| `---@class`、`---@alias` | 普通 table 内部字段 |
| 类成员函数 | 动态写法的方法 |

- 链式全局路径同时收录顶层名与完整路径（如 `_G.Mgr.HellModel`）
- 排序：匹配质量优先，符号类别次之

### 2.4 诊断

采用 **Emmy 路径严格、Lua 路径保守** 的策略。命中 Emmy 类型则按 Emmy 路径处理，否则按 Lua table shape 路径处理。

**Emmy 路径**：

| 情况 | 默认 severity |
|------|-------------|
| 字段赋值类型不兼容 | `warning`（`emmyTypeMismatch`） |
| 字段不存在 | `warning`（`emmyUnknownField`） |

**Lua 路径**：

| 情况 | 默认 severity |
|------|-------------|
| 显式 `nil` / 非对象值成员访问 | `warning`（`luaFieldError`） |
| closed shape 上不存在的字段 | `warning`（`luaFieldError`） |
| 开放结构上的未知字段 | `warning`（`luaFieldWarning`） |
| 字段赋值类型与 shape 冲突 | `warning`（`luaFieldWarning`） |

---

## 3. 配置项

完整配置项列表（均以 `mylua.` 为前缀）：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `server.path` | string/object | `""` | LSP 可执行文件路径，支持按平台配置 |
| `debug.fileLog` | boolean | `false` | 写调试日志到 `.vscode/mylua-lsp.log` |
| `runtime.version` | `"5.3"` \| `"5.4"` | `"5.4"` | Lua 运行时版本 |
| `runtime.topKeyword` | boolean | `false` | 启用列 0 关键字分割（改善错误定位） |
| `require.aliases` | object | `{}` | require 路径别名，最长前缀匹配 |
| `workspace.include` | string[] | `["**/*.lua"]` | 索引包含的 glob 模式 |
| `workspace.exclude` | string[] | `["**/.*", "**/node_modules"]` | 索引排除的 glob 模式 |
| `workspace.indexMode` | `"merged"` \| `"isolated"` | `"merged"` | 多根工作区策略 |
| `workspace.library` | string[] | `[]` | 额外索引目录（只读，抑制诊断） |
| `workspace.useBundledStdlib` | boolean | `true` | 自动注入内置 stdlib stubs |
| `index.cacheMode` | `"summary"` \| `"memory"` | `"memory"` | 索引持久化模式 |
| `diagnostics.enable` | boolean | `true` | 总开关 |
| `diagnostics.scope` | `"full"` \| `"openOnly"` | `"full"` | 诊断范围 |
| `diagnostics.undefinedGlobal` | severity | `"warning"` | 未定义全局变量 |
| `diagnostics.emmyTypeMismatch` | severity | `"warning"` | Emmy 类型不匹配 |
| `diagnostics.emmyUnknownField` | severity | `"warning"` | Emmy 未知字段 |
| `diagnostics.luaFieldError` | severity | `"warning"` | Lua 高确定性字段错误 |
| `diagnostics.luaFieldWarning` | severity | `"warning"` | Lua 保守字段警告 |
| `diagnostics.duplicateTableKey` | severity | `"warning"` | 重复 table key |
| `diagnostics.unusedLocal` | severity | `"hint"` | 未使用局部变量 |
| `diagnostics.argumentCountMismatch` | severity | `"warning"` | 参数数量不匹配 |
| `diagnostics.argumentTypeMismatch` | severity | `"warning"` | 参数类型不匹配 |
| `diagnostics.returnMismatch` | severity | `"warning"` | 返回值不匹配 |
| `gotoDefinition.strategy` | `"auto"` \| `"single"` \| `"list"` | `"auto"` | 多候选跳转策略 |
| `references.strategy` | `"best"` \| `"merge"` \| `"select"` | `"best"` | 多候选引用策略 |

> severity 可选值：`"error"` / `"warning"` / `"hint"` / `"off"`
