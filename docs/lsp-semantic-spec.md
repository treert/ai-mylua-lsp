# LSP 语义规范

定义 Lua/EmmyLua 的语义约定，以及各 LSP 能力如何消费索引数据。

索引数据模型详见 [`index-architecture.md`](index-architecture.md)。

---

## 1. 语义模型与名字解析

### 1.1 全局可见性

- 工作区内各文件对全局环境的贡献进入合并视图（遵守 `local` / 块作用域）。
- **不要求**先 `require` 才能看见其它文件的全局符号。
- 同名冲突保留候选列表，按打分选最佳候选。

### 1.2 `_G` 全局环境别名

`_G` 就是全局环境表本身，因此 `_G.X` 与裸 `X` 是**同一个全局名**（`_G._G == _G`，故 `_G._G.X` 同理）。

- **规范化时机**：在 `GlobalShard` 的键入口统一剥除 `_G.` 前缀（`normalize_global_path`），读、写、诊断、补全、`function_name_index` 共用同一键空间。上层调用方**不需要**再对 `_G` 做特殊判断。
- **不产生重复条目**：`_G.Foo = 1` 只登记一条 `Foo`，不再同时登记 `_G.Foo`；`workspace/symbol` 因此不会出现两个同义条目。
- **kind 跟随规范化**：`_G.X = v` 定义的是全局变量本身，登记为 `Variable`；`_G.T.f = v` 规范化后仍是多段路径，登记为 `TableExtension`。
- **裸 `_G` 保留**：stdlib 声明了 `---@class _G` + `_G = {}`，`_G` 自身作为根节点仍可达；`_G.` 的成员枚举（补全）遍历 trie 根集合，而非 `_G` 节点的 children。
- **遮蔽例外**：同作用域内 `local _G = {}` 会遮蔽全局环境，此时 `_G.X` 是普通 table 字段访问，既不登记为全局，也不享受上述别名。遮蔽判定依赖作用域解析结果，因此诊断中的**源码文本兜底前缀显式排除 `_G`**——否则文本层会绕过作用域判定，误判遮蔽的局部 `_G` 为全局环境并漏报诊断。

> `_G` 与 `_ENV` 的别名关系**不对称**，详见 §1.3。

### 1.3 `_ENV` 环境重定向

Lua 5.2+ 把每个 chunk 编译为「首个 upvalue 为 `_ENV`」的函数，自由名 `x` 按定义就是 `_ENV.x` 的语法糖。

**实现方式**：在每个文件的 File 作用域**预置一条隐式 `_ENV` 声明**（零长度、位于 chunk 起点、类型为全局环境），等价于开头隐式的 `local _ENV = _G`。

这样作用域树成为 `_ENV` 的**单一真源**：所有既有的「这个名字是局部变量吗」的提问——semantic tokens、hover、goto、补全、`undefinedGlobal`——都自动得到正确答案，各层**不需要**自己写 `_ENV` 分支。

**判据是「`_ENV` 指向什么」，而非「有无声明」**（预置后总是有声明）：

| `_ENV` 指向 | 自由名 `x` 的语义 |
|---|---|
| 全局环境（隐式声明，或显式 `local _ENV = _G`）| 普通全局，走 `GlobalShard`（即常规路径）|
| 其他表（`local _ENV = t`、`_ENV` 形参、`_ENV = t`）| 该表的字段，**不进** `GlobalShard`，且不报 `undefinedGlobal` |
| 未知类型（`local _ENV = f()`）| 保持静默：既不污染全局索引，也不猜测字段是否存在 |

- **读侧**复用 `FieldOf` stub，**写侧**复用 `register_nested_field_write`，未引入新的 `TypeFact` 类型。
- **`_ENV = expr` 的位置敏感性**由 `ScopeDecl.visible_after_byte` 表达：赋值前的自由名仍属于旧环境，赋值后属于新环境。因此 `g = 1; _ENV = {}; g = 2` 中两个 `g` 是**不同符号**，goto/references 不会互相关联。
- **`_ENV` 自身永不登记为全局**：它是 upvalue，`_G._ENV` 恒为 nil。
- **与 `_G` 的不对称性**：`_G` 是全局表的字段且指向自身，故 `_G.` 可重复剥离（`_G._G.X ≡ X`）；`_ENV` 是词法名而非表字段，只能作路径**起点**，故 `_ENV.` 仅在头部剥离一次。因此 `_ENV.X ≡ X`、`_ENV._G.X ≡ X`，而 `_G._ENV.X` 与 `_ENV._ENV.X` 运行时是 index nil，**不做归一**，保留为无法解析的伪键。
- **已知限制**：`load(chunk, name, mode, env)` 的第四参数、条件分支内的 `_ENV` 赋值（需流敏感分析）、`debug.setupvalue` 均不追踪。沙箱内的自由名不提供补全（不污染、不误报，但也不提供能力）。

### 1.4 `require` 绑定

- 模式：`local <name> = require(<静态字符串>)`
- 路径解析：模块串 → 目标 URI（`?.lua`、`require.aliases` 别名替换）
- 语义：`<name>` 绑定到目标文件 `return` 的模块值
- 反向索引：`(目标 URI) → [(来源文件, 局部名), …]`
- 非静态 `require`、拼接路径不建绑定

### 1.5 Emmy 类型名

`---@class`、`---@alias` 等进入工作区类型表。解析顺序：本文件 → 工作区。

### 1.6 标识符解析流程

1. **Lua 作用域**（`local`、块、闭包；含隐式 `_ENV`，见 §1.3）
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

- 链式全局路径同时收录顶层名与完整路径（如 `Mgr`、`Mgr.HellModel`）
- `_G.` 前缀在入库前已规范化，`_G.Mgr.HellModel` 与 `Mgr.HellModel` 是同一条目（见 §1.2）
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

默认值不在本文维护；以 `vscode-extension/package.json` 为唯一来源。

| 配置项 | 类型 | 说明 |
|--------|------|------|
| `server.path` | string/object | LSP 可执行文件路径，支持按平台配置 |
| `server.autoRestartOnConfigChange` | boolean | VS Code 配置变更后自动重启 LSP；关闭时弹窗询问 |

| `debug.fileLog` | boolean | 写调试日志到 `.vscode/mylua-lsp.log` |
| `runtime.version` | `"5.3"` \| `"5.4"` | Lua 运行时版本 |
| `runtime.topKeyword` | boolean | 启用列 0 关键字分割（改善错误定位） |
| `require.aliases` | object | require 路径别名，最长前缀匹配 |
| `workspace.include` | string[] | 索引包含的 glob 模式 |
| `workspace.exclude` | string[] | 索引排除的 glob 模式 |
| `workspace.library` | string[] | 额外索引目录（只读，抑制诊断） |
| `workspace.useBundledStdlib` | boolean | 自动注入内置 stdlib stubs |
| `diagnostics.enable` | boolean | 语义诊断开关；语法诊断始终保留 |
| `diagnostics.scope` | `"full"` \| `"openOnly"` | 诊断范围 |

| `diagnostics.undefinedGlobal` | severity | 未定义全局变量 |
| `diagnostics.emmyTypeMismatch` | severity | Emmy 类型不匹配 |
| `diagnostics.emmyUnknownField` | severity | Emmy 未知字段 |
| `diagnostics.luaFieldError` | severity | Lua 高确定性字段错误 |
| `diagnostics.luaFieldWarning` | severity | Lua 保守字段警告 |
| `diagnostics.duplicateTableKey` | severity | 重复 table key |
| `diagnostics.unusedLocal` | severity | 未使用局部变量 |
| `diagnostics.argumentCountMismatch` | severity | 参数数量不匹配 |
| `diagnostics.argumentTypeMismatch` | severity | 参数类型不匹配 |
| `diagnostics.returnMismatch` | severity | 返回值不匹配 |
| `inlayHint.enable` | boolean | 启用内嵌提示 |
| `inlayHint.parameterNames` | boolean | 调用实参前显示形参名 |
| `inlayHint.variableTypes` | boolean | 局部变量后显示推断类型 |

| `gotoDefinition.strategy` | `"auto"` \| `"single"` \| `"list"` | 多候选跳转策略 |
| `references.strategy` | `"best"` \| `"merge"` \| `"select"` | 多候选引用策略 |


> severity 可选值：`"error"` / `"warning"` / `"hint"` / `"off"`
