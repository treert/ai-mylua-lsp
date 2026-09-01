# Future Work — 后续待办

> **本文件只保留尚未实现的方向。** 已完成的条目直接删除；如果完成项涉及架构或数据结构变更，需在同一次提交中同步更新 [`index-architecture.md`](index-architecture.md)、[`architecture.md`](architecture.md) 等相关文档。
>
> 关联文档：[index-architecture.md](index-architecture.md)、[performance-analysis.md](performance-analysis.md)、[lsp-semantic-spec.md](lsp-semantic-spec.md)

---

## 1. 索引与聚合层

### 1.1 [P1] `signature_fingerprint` 粒度过粗

- **问题**：文件级单一 hash（`DocumentSummary.signature_fingerprint`），任何一个对外 API 变动都让整个下游链路失效。对"挂了几十个 global 的 `Mgr.lua`"影响尤为明显。
- **方案**：改为 **per-name fingerprint**（`HashMap<String, u64>`），按名字逐个 diff，只标脏变化的名字。文件级 hash 保留作 quick check。
- **验收**：改一个 class 的单个 field，其他 class 的下游文件不被标脏。

### 1.2 [P3] `TypeCandidate` 只存剪影 → 消费方二次线扫

- **问题**：不含 fields / parents，消费方需回查 `summaries[uri].type_definitions` 做 `find()` 线扫（该字段仍是 `Vec<TypeDefinition>`）。
- **方案**：`type_definitions` 改为 `HashMap<String, TypeDefinition>`，O(1) 查询。注意同文件多同名 class 的去重。
- **验收**：hover 热路径的"候选 → 详情"查找耗时下降。

### 1.3 [P3] `FieldInfo.assignment_count` 名不副实

- **问题**：`table_shape.rs` 的 `FieldInfo.assignment_count` 注释写着"同一字段多次赋值时累加（union）"，但全仓 6 处构造点全部硬编码 `1`，从未累加，也无任何消费方。同一结构体的 `def_range` 有类似问题：注释说"首次定义位置"，而 `set_field` 是覆盖写入，实际存的是**最后一次**（`diagnostics/env_field.rs` 因此自建写点表，不用该字段，见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §1.3.1）。
- **方案**：二选一——要么实现累加并接上 union 推断，要么删除该字段并修正 `def_range` 的注释使其与实现相符。倾向后者：当前没有消费方，留着只会误导下一个读代码的人。
- **验收**：字段注释与实现一致；若保留则有测试覆盖累加行为。

---

## 2. 泛型支持缺口

### 2.1 [P3] 泛型实参未从调用点回填

- **问题**：`resolver.rs` 能对已知实参做替换，但调用点的实参类型没有传进来——`---@generic T @param x T @return T` 的用户函数以 table 字面量调用时 `call_arg_types` 里是 `Unknown`，返回因此解析为**未替换**的 `EmmyType("T")`。
- **已单独绕过的一格**：`setmetatable` 不再依赖本条——它的恒等语义（返回第一实参）已在 `summary_builder::type_infer::infer_call_return_type` 里直接建模，见 [`lsp-semantic-spec.md`](lsp-semantic-spec.md) §1.3。落地本条时下列测试必须保持绿，它们锁定「沙箱静默由 `is_closed` 独立保证、与返回类型能否解析无关」：
  - `setmetatable_env_reports_nothing`
  - `setmetatable_env_navigates_a_free_name_to_the_global`
  - `setmetatable_env_stays_silent_for_names_it_can_reach`
  - `inline_setmetatable_env_stays_non_exhaustive`
- **方案**：将调用点实参类型传入 `substitute_generic_params`，用 `unify_one` 的现有绑定机制回填。
- **验收**：自定义泛型函数以字面量调用时返回类型可解析；上述测试不变红。

### 2.2 [P3] 泛型上界约束（`@generic T : Foo`）未校验

- **问题**：Emmy 注解解析层已能读出 constraint（`GenericParam.constraint`），但 `FunctionSummary.generic_params` / `TypeDefinition` 只保存泛型名，违反约束的用法无法诊断。
- **方案**：将 bound 传播到 `FunctionSummary` / `TypeDefinition`，并在泛型实例化与调用诊断中校验。
- **验收**：约束违反 / 满足两类用例。

### 2.3 [P3] 泛型实参数量不校验

- **问题**：`Foo<T, U>` 用 `Foo<string>`（少一个）静默兜底不报错。
- **方案**：对比 `generic_params.len()` 与实参数量，不等报 `genericArityMismatch`。

### 2.4 [P3] 递归泛型栈溢出风险

- **问题**：`resolver.rs::substitute_in_fact` 无深度保护（递归调用不带 depth 参数），病态递归输入可能栈溢出。
- **方案**：加深度计数器，超阈值（如 32，与 `MAX_RESOLVE_DEPTH` 一致）停止递归返回原 fact。

---

## 3. 元表与表形状

### 3.1 [P3] 带元表的表仍会被报 `Unknown field`

- **问题**：`local obj = setmetatable({}, { __index = Base })` 之后读 `obj.field_on_base` 会得到 `Unknown field 'field_on_base' on table`（`luaFieldWarning`，默认 `warning`）。装了元表恰恰意味着「静态字段集合不是穷尽描述」，此时断言字段不存在是不可靠的——`__index` 完全可能提供它。
- **现状不是新引入的**：`diagnostics/field_access.rs` 一直只按 `TableShape::is_closed` 把严重度从 `luaFieldError` 降到 `luaFieldWarning`，而不是静默。分离式写法 `local t = {}; setmetatable(t, mt); t.x` 早就如此；内联写法此前因为返回类型推不出 table 而**偶然**逃过检查，`setmetatable` 恒等返回落地后两种写法归于一致（`test_diagnostics.rs::inline_and_separate_setmetatable_report_the_same_thing` 锁定这份一致性），于是这条既有噪声变得更容易撞上——内联写法在 OO 惯用法里更常见。
- **对照**：同一个 `is_closed == false` 事实，`envUnknownField` 与导航层的处理是**静默 / 回退**（§1.3 的 `{__index=_G}` 约定），只有 `field_access` 选择照报。两者对不齐。
- **方案**：需要能区分「非穷尽的来源」——元表 vs 动态键写入 vs `rawset`。`is_closed` 是单个 bool，表达不了，得在 `TableShape` 上记来源（如 `openness: Openness` 枚举）。有了来源后：元表 → 静默（与 `_ENV` 一致）；动态键 → 保留现有 `luaFieldWarning`（`test_diagnostics.rs::dynamic_bracket_key_opens_shape` 锁定）。
- **验收**：`setmetatable({}, {__index=Base})` 上的字段读不再报；动态键 `{ [k] = 1 }` 的行为不变。
- **风险**：改的是默认开启的诊断类别，需按 [`../AGENTS.md`](../AGENTS.md) §7 在真实 fixture 上确认噪声方向。

---

## 4. EmmyLua 注解

### 4.1 [P3] `emmy_type_name_at_byte` 无 AST 上下文

- **问题**：`emmy.rs::emmy_type_name_at_byte` 用纯字节扫描判定光标是否在 `---@...` 行的结构区。多行字符串/长注释里出现 emmy 样式的文本（例如 `[[\n  local x = ---@type Foo\n]]`）会被误识别为真正的类型引用，导致 hover/goto/references 出现错误命中。
- **方案**：三处调用入口（`hover.rs::hover` / `goto.rs::goto_definition` / `references.rs::identify_at_cursor`）改用 AST 先把光标定位到节点，仅当祖先链含 `emmy_comment` / `comment` 时再调 `emmy_type_name_at_byte`。
- **验收**：用户构造的"长字符串内含 emmy-like 行"用例不再触发类型 ref 误命中；既有 trailing/leading emmy 行的 goto/hover 行为保持不变。
- **风险**：触发条件极冷门，目前为已知限制（该函数 doc 注释指回本文档）。

---

## 5. 推荐落地顺序

1. **3.1** 元表来源区分 — 直接影响默认开启诊断的信噪比，改动集中在 `TableShape` + 一处消费方
2. **1.1** per-name fingerprint — 改动较大，可显著缩小大型工作区的级联重算范围
3. **1.3** `assignment_count` / `def_range` 注释与实现对齐 — 改动极小，且消除一处持续误导
4. **2.1** 泛型实参回填 — 能力提升明显
5. **1.2** `type_definitions` O(1) 详情索引 — 规模到 1 万+ 文件前不紧迫
6. 其余 P3 项按需补做

---

## 6. 维护约定

- 已完成的条目直接从本文件删除；如涉及架构变更，同一次提交更新相关文档（`index-architecture.md`、`architecture.md` 等）。
- **写条目前先核实现状**：本文件多次出现"描述与代码脱节"的情况（旧方案已被更好的方案取代、耦合警告因别处改动而失效、行数/计数过期）。动手前用搜索确认一遍，不要照抄旧描述。
- 新增条目模板：

```markdown
### [Px] <标题>

- **问题**：为什么要做（现状是什么、错在哪）
- **方案**：怎么做
- **验收**：什么条件下认为做完
- **风险**：（可选）对既有行为的影响、是否需要 opt-in、与其它条目的耦合
```

---

## 7. 新增能力时的维护清单

- **新增诊断类别**：在 `DiagnosticsConfig` 加字段 + 默认 severity + `vscode-extension/package.json` 配置声明 + `diagnostics/suppression.rs` 的抑制码；默认开启时需在 fixture 上跑一遍确认不会在真实项目上产生大量噪声
- **新增 LSP capability**：在 `lib.rs::initialize` 的 `ServerCapabilities` 声明 + async handler；独立的 `src/<feature>.rs` 模块 + 对应集成测试文件
- **涉及裸标识符解析**：只改 `name_resolution.rs`（§1.6 的单点实现），不要在单个能力里内联解析顺序。该模块同时是导航、references 验证侧、`undefinedGlobal` 与补全的共同判据——四者任一自行判定都会引入不对称
- **代码修改后**：按 [`../.cursor/rules/coding-discipline.mdc`](../.cursor/rules/coding-discipline.mdc) 的纪律执行（尤其「Rust 禁止格式化」与「最小改动」），跑全量测试
- **文档同步**：对外能力变动同步 [`lsp-capabilities.md`](lsp-capabilities.md)；架构/数据流变动同步 [`architecture.md`](architecture.md) / [`index-architecture.md`](index-architecture.md)；语义规则变动同步 [`lsp-semantic-spec.md`](lsp-semantic-spec.md)；用户可见变动追加 `vscode-extension/CHANGELOG.md` 的 `[Unreleased]`
