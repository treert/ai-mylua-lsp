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
- **键空间与类型解析必须两侧都归一（重要）**：`normalize_global_path` 只管**键空间**，因此写侧 `_G._G.X = 1` 早就落在裸键 `X` 上；但读一个字段还要经过**类型解析**，那里同样需要承认 `_G._G == _G`。该规则由 `resolver::resolve_field_access` 单点实现（字段名为 `_G` 且基类型是全局环境时，原样返回环境 fact）。此前只有键空间做了归一，读侧与写侧因此分歧：`_G._G.X` 报 `Unknown field '_G' on type '_G'`，跳转与 hover 也全部落空。放在解析层而不是诊断层，goto / hover / 字段诊断一次对齐。
- **"什么是全局环境 fact" 只有一个判据**：`type_system::global_env_fact`（构造）与 `type_system::is_global_env_fact`（判定）是唯一实现，构建侧（`summary_builder::type_infer`）、查询侧（`type_inference`）与 `resolver` 共用。两种拼写都算全局环境：`EmmyType("_G")`（隐式 `_ENV` 声明 / stdlib `---@class _G`）与 `GlobalRef("_G")`（显式 `local _ENV = _G`）。三层曾各自复制一份，新增拼写只改一处就会在另两层静默失效。
- **不产生重复条目**：`_G.Foo = 1` 只登记一条 `Foo`，不再同时登记 `_G.Foo`；`workspace/symbol` 因此不会出现两个同义条目。
- **kind 跟随规范化**：`_G.X = v` 定义的是全局变量本身，登记为 `Variable`；`_G.T.f = v` 规范化后仍是多段路径，登记为 `TableExtension`。
- **裸 `_G` 是内置概念**：`_G` 的值即全局环境表，由 LSP 直接内置识别（`GLOBAL_TABLE_NAME`），**不依赖 stdlib 声明 `_G = {}`**——因此即使用户未配置任何 library，`_G.X` 仍可正常解析。stdlib 只保留 `---@class _G` 提供 hover 文档与跳转目标。`_G.` 的成员枚举（补全）遍历 trie 根集合，而非 `_G` 节点的 children。
- **遮蔽例外**：同作用域内 `local _G = {}` 会遮蔽全局环境，此时 `_G.X` 是普通 table 字段访问，既不登记为全局，也不享受上述别名。遮蔽判定依赖作用域解析结果，因此诊断中的**源码文本兜底前缀显式排除 `_G`**——否则文本层会绕过作用域判定，误判遮蔽的局部 `_G` 为全局环境并漏报诊断。
- **`_ENV` 重定向优先于内置识别（重要）**：`_G` 是全局表的一个**普通字段**，因此重定向后的环境同样不提供它——`_ENV = {}` 之后名字 `_G` 就是 nil。所以自由名规则中**先判环境重定向，后判内置 `_G`**（`infer_bare_name_fact` 与构建侧同名分支两处）。顺序反了会让 `_G.X` 越过沙箱直达真全局，跳到运行时不可达的符号上；写侧则会把 `_G.x = 1` 经 `_G.` 前缀规范化后登记成裸键 `x`，污染整个工作区索引。内置识别的目的只是**摆脱对 stub 里 `_G = {}` 的依赖**，与"是否尊重 `_ENV` 重定向"无关；常规环境下 `env_field_base_fact` 返回 `None`，内置分支照旧命中，stub 独立性不受影响。

> `_G` 与 `_ENV` 的别名关系**不对称**，详见 §1.3。

### 1.3 `_ENV` 环境重定向

Lua 5.2+ 把每个 chunk 编译为「首个 upvalue 为 `_ENV`」的函数，自由名 `x` 按定义就是 `_ENV.x` 的语法糖。

**实现方式**：在每个文件的 File 作用域**预置一条隐式 `_ENV` 声明**（零长度、位于 chunk 起点、类型为全局环境），等价于开头隐式的 `local _ENV = _G`。

这样作用域树成为 `_ENV` 的**单一真源**：所有既有的「这个名字是局部变量吗」的提问——semantic tokens、hover、goto、补全、`undefinedGlobal`——都自动得到正确答案，各层**不需要**自己写 `_ENV` 分支。

**`_ENV` 的类型被归一为恰好两种**（`summary_builder::visitors::env_binding_fact`，三个绑定点共用：`_ENV = expr`、`local _ENV = expr`、`_ENV` 形参）：

| 归一结果 | 何时 | 说明 |
|---|---|---|
| 全局环境 fact | 隐式声明、`_ENV = _G`、`local _ENV = _G`、或指向 `_G` 的局部 | 常规全局路径 |
| 某个 `TableShape` | 其余一切 | RHS 已有 shape 则原样用；否则**合成一个并 `mark_open`** |

合成 shape 的必要性：自由名写入 `x = 1` 就是 `_ENV.x = 1`，需要一张表来承载。此前 `_ENV` 类型推不出时（`setmetatable(...)` 的泛型返回、工厂函数、形参）没有落点，于是 `x = 1` **既不进全局索引（正确）、也不进任何 shape**——名字凭空消失，goto / hover / references 全部失效。归一后写入总有归属，且仍不污染全局索引。

**两种支持的沙箱写法**（`__index` **不做追踪**，其余情形一律按第 2 种处理）：

| 写法 | shape | 读 | 写 | 诊断 |
|---|---|---|---|---|
| 1. 无元表，`_ENV = {}`；需要的全局提前 `local` 保存 | `is_closed` | **只查 shape** | 落 shape | 缺失字段报 `envUnknownField` |
| 2. `setmetatable({}, {__index=_G})`：可读全局、不写全局 | 非 `is_closed` | 查 shape → **回退全局** | 落 shape | 两处都查不到报 `undefinedGlobal` |

第 2 种是**约定而非推断**：不追踪 `__index` 指向何处，凡带元表（或类型推不出）的环境**一律假定**为 `{__index=_G}`。`__index` 指到别处的代码也会得到全局的答案——这是刻意的，用于迫使按上述两种方式书写。

约定带来的关键收益：**诊断可以恢复**。既然假定 `__index=_G`，那么「shape 里没有、全局索引里也没有」就等于运行时是 nil，报 `undefinedGlobal` 不再是猜测。此前这类环境下诊断全面静默，会掩盖真实的 nil 访问。

**唯一判据是 `env_field_at` 是否返回 `Some`**（`name_resolution`），四个消费方共用：导航（`resolve_bare_name`）、`references::verify_global`、`undefinedGlobal`、补全（经 `env_completion_scope`，同一套谓词）。返回 `None` 即「按普通全局处理」。四者不一致时，光标处把沙箱名解析为全局、而验证侧把沙箱内出现点排除掉，引用结果会只剩声明而丢掉所有沙箱内用法（§1.6 要防的正是这类不对称）。

**`_ENV = _G` 的恢复语义**：`local _ENV = _G`、`_ENV = _G`、以及先 `local G = _G` 再 `_ENV = G` 都能正确恢复到全局环境（写侧重新进入 `GlobalShard`）。但**先重定向再 `_ENV = _G` 无法恢复**——那时名字 `_G` 已经是 nil，该语句实际是 `_ENV = nil`，我们也正确报出 `'_G' is not a field of the current _ENV`。要恢复必须提前捕获 `_G`。

**shape 何时非穷尽**：`setmetatable(t, …)` / `rawset(t, …)` 在构建侧对目标 shape 调 `TableShape::mark_open()`（`summary_builder::mark_shapes_opened_by_metatable_calls`），覆盖分离式写法 `local t = {}; setmetatable(t, mt); _ENV = t`——它的 `_ENV` fact **就是** `{}` 字面量的确定 shape，只看「是不是 table」会漏掉。「字段集合非穷尽」是关于**这张表本身**的事实，记录在 shape 上即经既有的 `is_closed` 单点流向全部消费方：

| 消费方 | 如何收到 |
|---|---|
| 导航 | `env_describes_its_fields` 检查 `is_closed` |
| `envUnknownField` | `check_one_read` 原有的 `!is_closed` 早退 |
| `luaFieldWarning` | 严重度自 `luaFieldError` 降为 `luaFieldWarning`（两者默认同为 `warning`，故默认配置下不可见）|

> `mark_open` 只标记**实际传给** `setmetatable` 的那张表，同文件其它表不受影响。该扫描此前是 `diagnostics/env_field.rs` 的私有副本，只让它自己的检查静默；收归构建侧后私有扫描已退役，`env_field.rs` 的 `poisoned` 只剩 `_ENV[k] = v` 动态键这一项。

- **读侧**复用 `FieldOf` stub，**写侧**复用 `register_nested_field_write`，未引入新的 `TypeFact` 类型。
- **裸名与点号两条写入路径都要设闸**：`x = 1`、`Foo.bar = 1`、`function foo()`、`function Foo.f()` 在沙箱下**一律不登记**任何全局。点号路径曾漏设闸门，导致 `Foo.bar = 1` 仍导出全局 `Foo.bar`；`_G.x = 1` 更隐蔽——经 `_G.` 规范化后落在裸键 `x` 上。判据统一为「基名在该位置属于重定向环境的字段」（`env_field_base_fact`），`_ENV` 自身返回 `None`，故 `_ENV.foo = 1` 仍正确归一为全局 `foo`。
- **"不登记全局"≠"什么都不登记"**：闸门之后还要分辨该写入在沙箱里是否**合法**：

| 沙箱内写法 | 运行时含义 | 处理 |
|---|---|---|
| `x = 1` / `function foo() end` | 往新环境写字段，**合法** | `register_nested_field_write(ENV_NAME, …)` 写入环境 shape |
| `Foo.bar = 1` / `function Foo.f() end` | `Foo` 在新环境是 nil，index nil **报错** | 什么都不登记 |

  两种 `function` 形式曾一并被闸门拦掉，使 `function foo() end` 的名字**既不在全局索引、也不在环境 shape**——等于凭空消失，goto / hover / references 全部失效，比它替代掉的索引泄漏更糟。赋值形式 `foo = function() end` 一直是对的，两者必须对称。
- **查询侧的对应支路**：三个导航能力统一经 §1.6 的 `name_resolution::resolve_bare_name` 得到 `EnvField`，各自呈现。不要在单个能力里内联 `_ENV` 判定。
- **`_ENV = expr` 的位置敏感性**由 `ScopeDecl.visible_after_byte` 表达：赋值前的自由名仍属于旧环境，赋值后属于新环境。因此 `g = 1; _ENV = {}; g = 2` 中两个 `g` 是**不同符号**，goto/references 不会互相关联。
- **`_ENV` 自身永不登记为全局**：它是 upvalue，`_G._ENV` 恒为 nil。因此它也**不在** `lua_builtins::COMMON` 的内置全局名单里——隐式声明已让"这是局部变量吗"的提问对 `undefinedGlobal` 与 semantic tokens 都给出正确答案，名单里再列一次只会陈述与模型相反的事实。
- **与 `_G` 的不对称性**：`_G` 是全局表的字段且指向自身，故 `_G.` 可重复剥离（`_G._G.X ≡ X`，读侧的同一规则见 §1.2）；`_ENV` 是词法名而非表字段，只能作路径**起点**，故 `_ENV.` 仅在头部剥离一次。因此 `_ENV.X ≡ X`、`_ENV._G.X ≡ X`，而 `_G._ENV.X` 与 `_ENV._ENV.X` 运行时是 index nil，**不做归一**，保留为无法解析的伪键。
- **补全按环境分层**（`completion::collect_free_name_completions`）：判据取自同一个 `name_resolution::env_completion_scope`，因此补全给出的名字与导航/诊断的判断一致——不会出现"补全提示的名字被 `envUnknownField` 立刻打红"。

| 环境 | 补全候选 |
|---|---|
| 全局环境 | 全局命名空间（常规） |
| 穷尽 shape（写法 1）| **只有**该 shape 的字段（全局在那里是 nil，给了就是错的）|
| 非穷尽 shape（写法 2 及其余）| shape 字段 **叠加** 全局命名空间（按 `{__index=_G}` 约定两者都可达）|

  可见的 `local` 不受影响——它们是词法名而非环境字段，沙箱开头 `local print = print` 的意义正在此。
- **已知限制**：`load(chunk, name, mode, env)` 的第四参数、条件分支内的 `_ENV` 赋值（需流敏感分析）、`debug.setupvalue` 均不追踪；`__index` 的实际指向**按设计不追踪**（见上文约定）。`document_highlight` 按设计只做文本 + 作用域匹配、不查索引，因此与其它全局名一样不区分环境边界。沙箱表**字面量内**写的字段仍不可达（见 `future-work.md` §3.1）。
- **对 stub / 声明文件的影响（重要）**：`_ENV = {}` 是一条**真实生效**的环境重绑语句，不是"声明 `_ENV` 存在"。因此声明文件（stdlib、`workspace.library` 下的第三方 stub）中**不得**出现该语句——否则同文件中位于其**之后**的所有「赋值形式」全局（`X = {}`）都会被视为那张废弃表的字段而无法导出到全局索引；函数形式声明（`function f() end`）不受影响，故故障表现极为隐蔽。自带 stdlib 已移除该行；`_ENV` 无需任何声明，由隐式声明提供。
- **自由名 fact 的唯一入口**：文法会把裸名包成两种节点形状（`identifier` 与单子节点的 `variable`），两者必须应用同一套规则。查询侧收敛于 `type_inference::infer_bare_name_fact`，构建侧收敛于 `summary_builder::type_infer` 的对应分支；新增规则只改一处会在另一形状上静默失效。

#### 1.3.1 环境字段未定义诊断（`envUnknownField`）

环境被重定向后，读取新环境中不存在的名字在运行时是 `nil`，几乎总是 bug。该诊断由 `diagnostics/env_field.rs` 实现，与 `undefinedGlobal` **按环境形态分工**：

| 环境形态 | 归谁管 |
|---|---|
| 穷尽 shape（写法 1，无元表）| `envUnknownField` —— 字段集合已知，缺失即确定为 nil |
| 非穷尽 shape（写法 2 及其余）| `undefinedGlobal` —— 按 `{__index=_G}` 约定回退查全局索引，两处皆无才报 |

交接点是 `name_resolution::env_field_at`：返回 `Some` 则 `undefinedGlobal` 静默、本诊断接管；返回 `None` 则按普通全局走 `undefinedGlobal`。

**为什么这条诊断是位置敏感的，而 `luaFieldWarning` 不是**：普通 table 的 shape 是全文件汇总、无顺序概念，`local M = {}; print(M.a); M.a = 1` 不报——此行为**不变**。chunk 的环境则不同：文件顶层语句恰好执行一次、且按源码顺序，因此「读取早于首次写入」是可判定的事实。

**双侧围栏**（缺一即误报）：

| 侧 | 约束 | 原因 |
|---|---|---|
| 读 | 必须直接位于 chunk 顶层作用域 | 函数体内的读，其执行时机与定义位置无关 |
| 写 | **每一处**写都必须直接位于 chunk 顶层作用域 | 出现函数体内 / 顶层分支内的写，即放弃位置判定，视该字段为已定义 |

**两种消息**（抑制码同为 `env-field`）：

| 情形 | 消息 |
|---|---|
| 字段完全不存在 | `'x' is not a field of the current _ENV` |
| 存在但赋值在读取之后 | `'x' is read before it is assigned in the current _ENV` |

**内置名不豁免**：`_ENV = {}` 后 `print` / `string` / `_G` 按语义确实都是 nil——这正是标准沙箱要写 `local print = print` 的原因。环境形状**完全已知**时报出它们没有任何猜测成分，豁免只会掩盖真 bug。

真正需要静默的是让内置名重新可达的写法 `setmetatable({}, { __index = _G })`。这不靠名单豁免，而是把"装了 metatable"表达为它本来的含义：**该表静态记录的字段集合不再是环境的穷尽描述**，整个环境转入静默——与动态键写入同一类事实。

**shape 失去穷尽性的三个来源**（任一命中即静默整个环境）：

| 来源 | 说明 | 记录位置 |
|---|---|---|
| `setmetatable(t, …)` | `__index` / `__newindex` 链不被追踪 | 构建侧 `mark_shapes_opened_by_metatable_calls` → `TableShape::is_closed` |
| `rawset(t, …)` | 写入不登记在 shape 上 | 同上 |
| 动态键 `_ENV[k] = v` | 字段名无法静态确定 | 本模块局部的 `poisoned` 集合 |

前两者记录在 **shape 自身**上，因为「字段集合非穷尽」是关于那张表的事实，而非本诊断独有的关切——同一事实还要供导航的全局回退（§1.3）与 `luaFieldWarning` 的严重度使用。本模块因此只需原有的 `!is_closed` 早退即可收到。动态键写入是关于**这一处 `_ENV` 绑定**的陈述而非关于表，故仍留在本模块。

> **不能**用「`setmetatable(...)` 的返回类型推不出 table」来代替对调用本身的追踪，两个独立原因：① `local t = {}; setmetatable(t, …); _ENV = t` 根本不经过返回值，`_ENV` 的 fact **就是**字面量的 `Known(Table)`；② 即便 `_ENV = setmetatable({}, …)`，当前"返回类型推不出 table"也是**偶然**的——stdlib 签名是 `---@generic T … @return T`，现在解析为未替换的 `EmmyType("T")` 仅因为泛型实参尚未从调用点回填；一旦实现回填就会拿到 `{}` 的 shape。届时静默由 `is_closed` 独立保证（该 shape 已被标 open），`test_global_env.rs::setmetatable_env_reports_nothing` 与 `late_setmetatable_env_navigates_a_free_name_to_the_global` 共同锁定此点。

**保持静默的其余场景**：`_ENV` 指向未知类型 / 非 table；`load` 第四参数、`debug.setupvalue`、`debug.setmetatable`；仅在函数体内写入的字段。

**实现要点**：写点位置由本模块自建的写点表给出，**不用** `FieldInfo.def_range`——后者经 `set_field` 覆盖后是**最后一次**写入位置，而位置敏感判定需要的是首次写入，且需要区分「顶层直线流」与「函数体/分支内」。写点收集必须覆盖赋值形式与声明形式（`function foo() end`）两种写法，缺一即对该形态误报。

### 1.4 `require` 绑定

- 模式：`local <name> = require(<静态字符串>)`
- 路径解析：模块串 → 目标 URI（`?.lua`、`require.aliases` 别名替换）
- 语义：`<name>` 绑定到目标文件 `return` 的模块值
- 反向索引：`(目标 URI) → [(来源文件, 局部名), …]`
- 非静态 `require`、拼接路径不建绑定

### 1.5 Emmy 类型名

`---@class`、`---@alias` 等进入工作区类型表。解析顺序：本文件 → 工作区。

### 1.6 标识符解析流程

**裸名（无 `.` / `:` 限定）的解析顺序**，由 `name_resolution::resolve_bare_name` **单点实现**：

| 步 | 解析目标 | 结果 |
|---|---|---|
| 1 | **Lua 作用域**（`local`、块、闭包；含隐式 `_ENV`，见 §1.3） | `Local` |
| 2 | **重定向 `_ENV` 的字段**（见 §1.3）——此时名字**不是**全局，不再往下走 | `EnvField` |
| 3 | Emmy 类型名 → 本文件类型表 → 工作区类型表 | `TypeName` |
| 4 | 全局合并表（唯一可能多候选，故有 `GotoStrategy` / `ReferencesStrategy`） | `Global` |

`goto`、`hover`、`references` 消费同一个 `BareName` 结果，各自只负责呈现。**这是硬性约束**：本文档曾长期声称三者共用入口，而实际上各自内联了一份顺序，导致 `_ENV` 重定向规则只在 `goto` 生效——`hover` 返回空，`references` 退化为全文文本匹配、无法区分重绑前后的同名符号（与 §1.3 的承诺矛盾）。新增裸名规则**只应改 `name_resolution`**。

**不属于本层**（各能力自行处理的位置特化分支）：

| 场景 | 原因 |
|---|---|
| 点号 / 方法字段（`a.b.c`、`obj:m()`） | 已共享 `resolver::resolve_field_chain`；各能力差异在呈现而非解析 |
| `require` 绑定 LHS、`goto` 标签 | 仅 `goto` 有，纳入等于把单一能力的关注点塞进公共类型 |

**`EnvField` 无定义位置时必须静默**，不得回落到全局分支：此时名字确定**不是**全局，回落会跳到运行时不可达的同名符号上。

**references 的两侧对称性**：`Identity::EnvField` 在每个候选位置**重新解析**（`env_field_location`），因此 `_ENV = expr` 的位置敏感性自动生效，重绑前后的 `g` 落在不同声明上而不合并；对称地 `verify_global` 也必须排除「该位置环境已重定向」，否则点击重绑**前**的 `g` 仍会把沙箱内的 `g` 算作同一符号。两者缺一即漏。

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
| 重定向 `_ENV` 上不存在 / 尚未赋值的字段 | `warning`（`envUnknownField`，见 §1.3.1） |

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
| `diagnostics.envUnknownField` | severity | 重定向 `_ENV` 上的未定义字段（见 §1.3.1） |
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
