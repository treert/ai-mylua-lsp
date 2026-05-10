# MyLua 语法支持实现计划

本文档把 `mylua-spec.md` 中的语法设计拆成可逐步完成的工程任务。由于完整实现跨越 VS Code 扩展、workspace scanner、Tree-sitter grammar、external scanner、Rust analyzer、diagnostics 和测试体系，建议按阶段推进，每次会话只认领一个小阶段。

## 总体策略

- 使用单 parser：`.lua` 与 `.mylua` 共用 MyLua superset grammar。
- 先保证 parser 接受 MyLua 语法，再逐步补齐 LSP 语义。
- `.lua` 严格模式后置，通过 diagnostics 扫描 MyLua 专属 AST 节点实现。
- 优先局部适配，避免为了第一阶段支持而重写 summary/type/diagnostics 架构。
- 每完成一个语法点，必须同步补测试，避免 grammar 变更静默破坏 LSP 行为。

## 分阶段计划

### P0：测试与执行闭环

目标：建立后续每次改动都能验证的基础。

任务：

- [ ] 新增 `grammar/test/corpus/mylua.txt`，记录 MyLua AST 快照。
- [ ] 新增 `lsp/crates/mylua-lsp/tests/test_mylua_parse.rs`。
- [ ] 将以下手工样例纳入 parser smoke test：
  - `tests/lua-root/mylua/dollarext.mylua`
  - `tests/lua-root/mylua/array.mylua`
  - `tests/lua-root/mylua/func-named-args.mylua`
  - `tests/lua-root/mylua/continue.mylua`
- [ ] 第一版测试只断言 `root.has_error() == false`，后续再细化 AST 形状。

验证：

```bash
cd grammar
npx tree-sitter test
npx tree-sitter generate
cd ../lsp
cargo test
```

### P1：`.mylua` 文件后缀接入

目标：`.mylua` 文件能被 VS Code 和 LSP 识别、打开、监听、索引。

涉及文件：

- `vscode-extension/package.json`
- `vscode-extension/src/extension.ts`
- `lsp/crates/mylua-lsp/src/workspace_scanner.rs`
- `lsp/crates/mylua-lsp/src/handlers.rs`

任务：

- [ ] VS Code language contribution 注册 `.mylua`。
- [ ] `documentSelector` 同时匹配 `lua` / `mylua`。
- [ ] file watcher 同时监听 `**/*.lua` / `**/*.mylua`。
- [ ] `mylua.workspace.include` 默认改为 `['**/*.lua', '**/*.mylua']`。
- [ ] workspace scan 收集 `.lua` 和 `.mylua`。
- [ ] `file_path_to_module_name` 同时 strip `.lua` / `.mylua`。
- [ ] `init.mylua` 与 `init.lua` 一样映射为目录模块名。
- [ ] watcher 的 create/change/delete 路径判断同时覆盖 `.mylua`。

验收：

- [ ] 打开 `.mylua` 文件能启动 LSP 能力。
- [ ] workspace index 包含 `.mylua` 文件。
- [ ] `require` map 能解析 `.mylua` 模块。

### P2：低风险 grammar 扩展

目标：先接入局部语法，尽快打通 parser 生成流程。

涉及文件：

- `grammar/grammar.js`
- `grammar/src/scanner.c`
- `grammar/src/parser.c`，由 `npx tree-sitter generate` 生成

任务：

- [ ] `continue_statement`。
- [ ] number regex 支持 `_`。
- [ ] 函数定义参数尾逗号。
- [ ] 函数调用参数尾逗号。
- [ ] `??` nil-coalescing operator。
- [ ] `array_constructor`：`[]`、`[1, nil, 3]`。
- [ ] 空下标写入语法：`t[] = value`。

Rust 第一阶段适配：

- [ ] `continue_statement` 可像 `break_statement` 一样无语义处理。
- [ ] `array_constructor` 暂时按 `table_constructor` 近似。
- [ ] 所有读取 variable `index` field 的逻辑允许空 index。
- [ ] `??` 类型推断可暂时保守处理，避免 panic。

验收：

- [ ] 对应 grammar corpus 无 ERROR。
- [ ] 对应 Rust parse smoke test 无 ERROR。
- [ ] `cargo test` 通过。

### P3：中风险 grammar 扩展

目标：支持调用、访问和 name 上下文扩展，先保持 LSP 不崩。

涉及文件：

- `grammar/grammar.js`
- `grammar/src/scanner.c`
- `lsp/crates/mylua-lsp/src/util.rs`
- `lsp/crates/mylua-lsp/src/signature_help.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/call_args.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/field_access.rs`

任务：

- [ ] safe field access：`xxx?.field`。
- [ ] safe index access：`xxx?['key']`。
- [ ] safe call：`xxx?()`。
- [ ] safe method call：`xxx?:method()`。
- [ ] keyword-as-name：field、table key、function/method name、label、goto label。
- [ ] call-side `named_argument`：`f(a=1)`。
- [ ] call-side `spread_argument`：`f(*args)`。

Rust 第一阶段适配：

- [ ] safe access/call 先按普通 access/call 解析，field diagnostics 初步降噪。
- [ ] keyword-as-name 尽量在 grammar 中 alias 为 `identifier`，减少 Rust 侧改动。
- [ ] `extract_call_arg_nodes` 能从 `named_argument` 提取 value 表达式。
- [ ] `spread_argument` 保守处理，避免错误的参数数量/类型诊断。
- [ ] signature help 对 named/spread 先 best-effort。

验收：

- [ ] MyLua named/spread 参数样例无 syntax error。
- [ ] hover/goto/diagnostics/signature help 不崩溃。
- [ ] 不产生明显大量误报。

### P4：`$function`

目标：支持 `$` 函数语法糖，并尽量复用现有 function 语义。

涉及文件：

- `grammar/grammar.js`
- `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs`
- `lsp/crates/mylua-lsp/src/type_inference.rs`
- `lsp/crates/mylua-lsp/src/util.rs`

任务：

- [ ] 新增 `dollar_function`。
- [ ] 支持 `${ block }`。
- [ ] 支持 `$(parlist){ block }`。
- [ ] `dollar_function` 加入 `_primary_expression`。
- [ ] `dollar_function` 加入 `arguments`，支持无括号调用参数。
- [ ] 尽量复用 `parameter_list` / `block` / function 相关 AST 结构。

Rust 第一阶段适配：

- [ ] `dollar_function` 类型按 function 处理。
- [ ] summary/scope 第一阶段可忽略函数体细节，但不能 panic。
- [ ] call args 提取支持 `f ${ ... }` / `f $(x){ ... }`。

验收：

- [ ] `$function` 样例无 syntax error。
- [ ] `cargo test` 通过。

### P5：`$string`

目标：支持独立 `dollar_string` AST、插值、转义和无括号调用参数。

涉及文件：

- `grammar/grammar.js`
- `grammar/src/scanner.c`
- `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs`
- `lsp/crates/mylua-lsp/src/type_inference.rs`
- `lsp/crates/mylua-lsp/src/util.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/mod.rs`

建议 AST：

- `dollar_string`
  - `dollar_string_content`
  - `dollar_escape`
  - `dollar_name_interpolation`
  - `dollar_interpolation`

任务：

- [ ] scanner 增加 dollar string mode。
- [ ] 支持 `$"..."` / `$'...'`。
- [ ] 支持 `$$`。
- [ ] 支持 `$name`。
- [ ] 支持 `${expr}`，内部复用普通 expression 解析。
- [ ] `${expr}` 结束后恢复 dollar string 内容扫描。
- [ ] `dollar_string` 加入 `_primary_expression`。
- [ ] `dollar_string` 加入 `arguments`，支持 `print $"hello"`。

Rust 第一阶段适配：

- [ ] `dollar_string` 类型按 string 处理。
- [ ] `extract_string_literal` 不把 `dollar_string` 当普通静态字符串。
- [ ] require/document link/module completion 不基于 `dollar_string` 做静态路径解析。
- [ ] call args 提取支持 `$string` 无括号单参数。

后续 diagnostics：

- [ ] `${expr}` 不允许跨物理行。
- [ ] `${expr}` 内不支持嵌套 `dollar_string`。
- [ ] `$` 后只允许 `$`、Name 或 `{`。

验收：

- [ ] `dollarext.mylua` 无 syntax error。
- [ ] `$"if local end"` 内容不会被错误识别为 Lua keyword。
- [ ] `${format("world")}` 内部普通字符串不会结束外层 dollar string。
- [ ] 无括号调用 `print $"hello"` 能解析为 function call。

### P6：LSP 基础能力稳定

目标：MyLua 文件可日常打开使用，不崩溃、不产生大面积误报。

重点文件：

- `lsp/crates/mylua-lsp/src/util.rs`
- `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs`
- `lsp/crates/mylua-lsp/src/summary_builder/table_extract.rs`
- `lsp/crates/mylua-lsp/src/type_inference.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/call_args.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/field_access.rs`
- `lsp/crates/mylua-lsp/src/diagnostics/duplicate_key.rs`
- `lsp/crates/mylua-lsp/src/signature_help.rs`
- `lsp/crates/mylua-lsp/src/completion.rs`
- `lsp/crates/mylua-lsp/src/semantic_tokens.rs`
- `lsp/crates/mylua-lsp/src/symbols.rs`

任务：

- [ ] hover 对新节点不崩溃。
- [ ] goto/references 对 keyword-as-name 尽量正常。
- [ ] completion 在 `.mylua` 文件可用。
- [ ] signature help 对 named/spread 参数 best-effort。
- [ ] semantic tokens 不因新 AST 节点遗漏导致异常。
- [ ] duplicate key / field access / call args diagnostics 对新语法降噪。
- [ ] document link 只使用普通静态字符串。

验收：

- [ ] 四个 MyLua 手工样例打开后 diagnostics 可接受。
- [ ] 常见 hover/goto/completion/signature help 操作不崩溃。
- [ ] `cargo test` 通过。

### P7：MyLua 专属 diagnostics 与严格 Lua 模式

目标：在单 parser 策略下区分 `.lua` 与 `.mylua` 的语法允许范围。

涉及文件：

- `vscode-extension/package.json`
- `vscode-extension/src/extension.ts`
- `lsp/crates/mylua-lsp/src/config.rs` 或现有配置模块
- `lsp/crates/mylua-lsp/src/diagnostics/*`
- `lsp/crates/mylua-lsp/src/handlers.rs`

任务：

- [ ] 新增配置 `mylua.runtime.strictLuaSyntaxForLuaFiles`。
- [ ] 配置从 VS Code 传到 LSP。
- [ ] LSP 判断当前 URI 是否为 `.lua` / `.mylua`。
- [ ] `.lua` 严格模式下扫描 MyLua 专属 AST 节点并报 warning/error。
- [ ] `.mylua` 文件不报严格 Lua 语法诊断。
- [ ] `$string` 专属诊断。
- [ ] `t[]` 只能出现在赋值左侧的诊断。

验收：

- [ ] `.mylua` 中使用 MyLua 语法无严格模式告警。
- [ ] `.lua` 中使用 MyLua 语法按配置报 warning/error。
- [ ] 关闭配置后 `.lua` 也允许 MyLua superset 语法。

### P8：MyLua 语义增强

目标：从“不崩溃、少误报”提升到“理解 MyLua runtime 语义”。

任务：

- [ ] `??` nil-coalescing 类型推断。
- [ ] optional chain 的 nil-aware 类型推断。
- [ ] named args 按参数名做参数数量/类型 diagnostics。
- [ ] signature help 按 named args 定位参数。
- [ ] completion 在调用参数位置提示参数名。
- [ ] 区分 array/map 类型。
- [ ] `table.newarray` / `[]` 推断为 array。
- [ ] `table.newmap` / `{}` 推断为 map。
- [ ] 可选：无插值 `dollar_string` 常量折叠，用于 require/document link。

## 推荐会话切分

每次新会话建议只选择一个小目标，并在开始前先阅读：

1. `docs/mylua-spec.md`
2. `docs/mylua-implementation-plan.md`
3. 当前阶段涉及的源码文件

推荐顺序：

1. 会话 A：完成 P0 测试骨架。
2. 会话 B：完成 P1 `.mylua` 后缀接入。
3. 会话 C：完成 P2 中的 `continue`、number `_`、尾逗号。
4. 会话 D：完成 P2 中的 `??`、`array_constructor`、`t[]`。
5. 会话 E：完成 P3 safe access/call。
6. 会话 F：完成 P3 keyword-as-name。
7. 会话 G：完成 P3 named/spread args。
8. 会话 H：完成 P4 `$function`。
9. 会话 I/J：完成 P5 `$string` scanner 与 AST。
10. 会话 K：完成 P6 LSP 稳定性适配。
11. 会话 L：完成 P7 严格 Lua 模式 diagnostics。
12. 会话 M+：逐步推进 P8 语义增强。

## 每次实现前检查清单

- [ ] 明确本次只做哪个阶段/子任务。
- [ ] 先补或调整对应测试。
- [ ] grammar 改动后执行 `npx tree-sitter generate`。
- [ ] Rust 改动后执行 `cargo test`。
- [ ] 如果改了 VS Code extension，确认 package 配置、watcher 和 initializationOptions 一致。
- [ ] 不把 `dollar_string` 当普通静态字符串参与 require/document link，除非进入 P8 常量折叠阶段。
- [ ] 不为了第一阶段支持做大规模重构。

## 风险备忘

- `$string` scanner mode 是最高风险点，应单独会话处理。
- keyword-as-name 如果不 alias 为 `identifier`，hover/goto/references/semantic tokens 容易漏。
- `t[]` 会让 variable `index` 为空，Rust 侧所有读取 index 的地方都要防空。
- `named_argument` / `spread_argument` 会影响 call args、signature help、inlay hint 和 diagnostics，第一阶段应保守。
- safe access/call 如果不降噪，field access diagnostics 可能产生大量误报。
- `.lua` 严格模式必须后置，避免在 parser 阶段引入双 grammar 或大量分支。
