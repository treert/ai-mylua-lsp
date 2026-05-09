# MyLua 语法支持规划

本文档用于记录 `mylua-lsp` 后续支持 MyLua 自定义语法的设计判断、语法范围、实现路线和风险点。后续新会话可以先阅读本文，再继续讨论细节和规划实现。

## 背景

当前 `mylua-lsp` 已经可以正式使用。接下来希望支持基于 Lua 的 MyLua 语法扩展，并通过新的文件后缀 `.mylua` 与普通 `.lua` 文件在使用层面区分。

核心判断：

- 使用 **单个 parser** 同时支持 `.lua` 和 `.mylua`。
- parser 使用 MyLua superset 语法，避免维护两套 grammar。
- `.lua` 文件是否允许 MyLua 扩展语法，后续通过诊断开关控制，而不是通过第二套 parser 控制。

建议新增配置：

```json
{
  "mylua.runtime.strictLuaSyntaxForLuaFiles": true
}
```

当该配置开启时：

- `.mylua` 文件正常允许全部 MyLua 语法。
- `.lua` 文件仍然用同一个 parser 解析。
- LSP 对 `.lua` 文件扫描 MyLua 专有 AST 节点，并对自定义语法报 warning/error。

这样可以兼顾：

- parser 和 analyzer 架构简单。
- `.lua` 文件不会因为扩展语法直接解析失败。
- 后续仍可提供“标准 Lua 严格模式”。

## 参考文档与测试样例

MyLua runtime 与语法设计参考：

- `/Users/zhuguosen/MyGit/mylua/lua/doc/mylua.bnf`
- `/Users/zhuguosen/MyGit/mylua/lua/doc/mylua.md`
- `/Users/zhuguosen/MyGit/mylua/lua/README.md`

测试样例：

- `/Users/zhuguosen/MyGit/mylua/lua/testes/dollarext.lua`
- `/Users/zhuguosen/MyGit/mylua/lua/testes/array.lua`
- `/Users/zhuguosen/MyGit/mylua/lua/testes/func-named-args.lua`
- `/Users/zhuguosen/MyGit/mylua/lua/testes/continue.lua`

## MyLua 语法扩展范围

### 1. `.mylua` 文件后缀

新增 `.mylua` 文件后缀，与 `.lua` 同时由 `mylua-lsp` 支持。

预期：

- VS Code 能识别 `.mylua` 文件。
- LSP 对 `.mylua` 文件提供 hover、goto、diagnostics、completion 等能力。
- workspace scan 同时索引 `.lua` 和 `.mylua`。
- require module name 规则同时支持 `.lua` / `.mylua`。

### 2. `continue`

新增 `continue` 语句。

示例：

```lua
for i = 1, 10 do
    if i % 2 ~= 0 then
        continue
    end
    print(i)
end
```

第一阶段只需要 parser 接受它，summary/scope 可以像 `break` 一样忽略。

### 3. `$string`

新增 `$` 字符串语法糖。

示例：

```lua
local s = "world"
print($"hello $s")
print($"1+2=${1+2}")
```

第一阶段建议：

- parser 把 `$"..."` / `$'...'` 接受为 `dollar_string`。
- 类型推断先按 `string` 处理。
- `${...}` 插值内容第一版可以不做完整 AST 嵌套解析，先保证整体不报语法错误。

后续增强：

- 对 `${expr}` 内部表达式做 AST 支持。
- 支持 hover/goto/diagnostics 进入插值表达式。

### 4. `$function`

新增 `$` 函数语法糖。

示例：

```lua
local f1 = ${ return "f1" }
local f2 = $(ff){ return "f2 " .. ff() }
```

语法：

```bnf
dollar_func ::= '$' [ '(' [parlist] ')' ] '{' block '}'
```

建议：

- parser 增加 `dollar_function`。
- analyzer 中尽量把它按普通 `function_definition` 处理。
- 如果 AST 能复用 `function_body` / `parameter_list`，后续 summary、signature help、hover 改动会更小。

### 5. `??` nil-coalescing 操作符

新增 `??` 操作符。

示例：

```lua
local x = value ?? default_value
assert((false ?? 1) == false)
```

语义：

- 只检查左侧是否为 `nil`。
- 与 Lua 的 `or` 不同，`false ?? x` 结果仍是 `false`。

第一阶段：

- parser 支持 `??`。
- type inference 可以先粗略处理为左右类型 union 或 unknown。

后续增强：

- 如果左侧确定非 nil，结果偏向左侧类型。
- 如果左侧可能 nil，结果为左侧去 nil 后与右侧合并。

### 6. `?` 安全访问 / 安全调用

新增安全访问与安全调用语法。

示例：

```lua
xxx?.xx
xxx?.xx?.yyy
xxx?()
xxx?:method()
xxx?['key']
xxx?.xx ?? 123
```

BNF 相关：

```bnf
var ::= Name | prefixexp ['?'] '[' exp ']' | prefixexp ['?'] '.' Name
functioncall ::= prefixexp ['?'] args | prefixexp ['?'] ':' Name ['?'] args
```

第一阶段：

- parser 接受 `?.` / `?[]` / `?()` / `?:method()`。
- analyzer 可以先按普通 field/call 处理，避免崩溃。

后续增强：

- `field_access` diagnostics 对安全访问降噪。
- type inference 将结果标记为可能 `nil`。
- goto/hover 仍尽量解析到原字段/函数。

### 7. `array_constructor`

新增 array 构造语法。

示例：

```lua
local a = []
local b = [1, nil, 3]
```

语法：

```bnf
array_constructor ::= '[' exp { fieldsep exp } [ fieldsep ] ']'
```

第一阶段：

- parser 支持 `array_constructor`。
- analyzer 中先按 `table_constructor` 近似处理。
- `#array` 等 runtime 语义不需要 LSP 第一阶段完整理解。

后续增强：

- 类型系统区分 `array` 与 `map`。
- `table.newarray` / `[]` 推断为 array。
- `table.newmap` / `{}` 推断为 map。

### 8. `t[] = value` push 语法

新增空下标赋值语法。

示例：

```lua
local t = {}
t[] = 1
t[] = 2
```

语义：

- 近似 `t[#t + 1] = value`。
- 只支持写入，不支持读取。

第一阶段：

- parser 允许 variable index 为空。
- analyzer 所有读取 `index` field 的地方要能处理 `None`。

后续增强：

- diagnostics 检查 `t[]` 只能出现在赋值左侧。
- 对 `print(t[])` 这类读取报错。

### 9. 参数尾逗号

函数定义和函数调用都允许尾逗号。

示例：

```lua
function f(a, b,) end
f(1, 2,)
```

第一阶段直接在 grammar 中放开即可。

### 10. 命名参数与 `*args`

新增调用侧命名参数和展开。

示例：

```lua
f(a=1, c=3, b=2)
f(11, c=33, *args)
f(b=11, *args, a=11, g())
f(*[100], 13)
```

设计说明：

- 命名参数只在调用侧支持。
- `*args` 和 `k=v` 平级，后出现者优先。
- `*args` 从 table/map 中取参数。
- 函数定义侧不引入 `*kwargs`。

第一阶段：

- parser 支持 `named_argument` 与 `spread_argument`。
- call argument extraction 能取出实际表达式，避免 diagnostics 崩溃。
- signature help 可以暂时按 best-effort 处理。

后续增强：

- argument count diagnostics 区分 positional/named/spread。
- argument type diagnostics 对 `name=value` 按参数名匹配。
- completion 在调用参数位置提示参数名。

### 11. keyword 在无歧义位置作为 name

支持 keyword 在特定上下文作为普通名字。

示例：

```lua
local tb = {
    local = 1,
}
tb.end = 1
function tb:for()
end
```

建议不要把所有 keyword 全局变成 identifier。

建议只在这些上下文放开：

- field access：`t.end`
- table field key：`{ local = 1 }`
- method/function name：`function t:for()` / `function t.end()`

实现建议：

- 在 grammar 中引入 `_member_name` 或 `_name_like_identifier`。
- 它可以匹配 `identifier` 和指定 keyword token。
- 如果可能，用 `alias(..., $.identifier)`，减少 Rust analyzer 改动。

### 12. 数字字面量支持 `_`

示例：

```lua
local a = 1_000_000
local b = 0.000_000_000_01
local c = 0x00_14_22_01_23_45
```

第一阶段只需要扩展 number regex。

## 当前 `mylua-lsp` 主要改造点

### VS Code extension

文件：

- `vscode-extension/package.json`
- `vscode-extension/src/extension.ts`

需要：

- 注册 `.mylua` 后缀。
- `documentSelector` 同时匹配 `lua` / `mylua`。
- file watcher 同时监听 `**/*.lua` / `**/*.mylua`。
- `mylua.workspace.include` 默认改为 `['**/*.lua', '**/*.mylua']`。
- 语法高亮第一版可以复用现有 Lua TextMate grammar。

### Workspace scanner

文件：

- `lsp/crates/mylua-lsp/src/workspace_scanner.rs`

需要：

- 扫描 `.lua` 和 `.mylua`。
- `file_path_to_module_name` 同时 strip `.lua` / `.mylua`。
- `init.mylua` 和 `init.lua` 一样处理。
- 注释中 “Lua files” 可以逐步改成 “Lua/MyLua files”。

### Tree-sitter grammar

文件：

- `grammar/grammar.js`
- `grammar/src/scanner.c`

需要：

- 增加 keyword：`continue`。
- 增加 operator：`??`。
- 增加 tokens/productions：`$string`、`$function`、`array_constructor`、`safe access/call`、`named_argument`、`spread_argument`。
- 放开参数尾逗号。
- 放开部分 keyword-as-name 上下文。
- 扩展 number regex 支持 `_`。

生成流程：

```bash
cd grammar
npx tree-sitter generate
cd ../lsp
cargo test
```

### Rust analyzer

相关区域：

- `summary_builder`
- `type_infer`
- `util::extract_call_arg_nodes`
- diagnostics：`field_access`、`call_args`、`duplicate_key`、`syntax`
- hover/goto/references/signature_help/semantic_tokens

第一阶段原则：

- 新 AST 节点不应导致 panic 或大量误报。
- `array_constructor` 可先按 table 处理。
- `dollar_string` 可先按 string 处理。
- `dollar_function` 可先按 function 处理。
- `continue_statement` 可先无语义处理。
- `named_argument` / `spread_argument` 先保证 call args 提取合理。

## 推荐里程碑

### M1：文件后缀与语法接入

目标：

- `.mylua` 文件能被 VS Code 和 LSP 识别。
- MyLua 测试样例能被 parser 接受。
- 不追求完整语义，只追求无 syntax error。

包含：

- `.mylua`
- `continue`
- `$string`
- `$function`
- `??`
- `?.` / `?[]` / `?()` / `?:method()`
- `[]`
- `t[]`
- trailing comma
- named args / `*args`
- numeric `_`
- keyword-as-name

### M2：LSP 基础能力稳定

目标：

- 打开 MyLua 文件后，hover/goto/diagnostics/completion 不崩溃。
- 常见语法不产生大量误报。

包含：

- `array_constructor` 类型近似为 table。
- `dollar_string` 类型为 string。
- `dollar_function` 类型为 function。
- call args 支持 named/spread。
- optional chain 的 field diagnostics 初步降噪。
- `t[]` 相关逻辑避免空 index 崩溃。

### M3：MyLua 语义增强

目标：

- LSP 能更准确理解 MyLua runtime 语义。

包含：

- `??` 类型推断。
- optional chain 的 nil-aware 类型推断。
- named args 按参数名做 diagnostics/signature help。
- `array` / `map` 类型区分。
- `t[]` 只允许赋值左侧的语义诊断。
- `.lua` 严格模式诊断开关。

## 难度评估

### 简单

- `.mylua` 后缀接入。
- watcher / workspace include。
- `continue`。
- 数字 `_`。
- 参数尾逗号。
- `??` parser 支持。

### 中等

- `[]` array constructor。
- `$string`。
- `$function`。
- keyword-as-name。
- optional chain parser 支持。

### 较复杂

- named args / `*args` 的完整 diagnostics 和 signature help。
- optional chain 的精确类型推断和诊断降噪。
- `??` nil-aware 类型推断。
- `array` / `map` 在类型系统内完整区分。
- `.lua` 严格模式下对 MyLua AST 节点做精确诊断。

## 设计原则

1. **单 parser，MyLua superset grammar**
   - 降低维护成本。
   - 避免 `.lua` 与 `.mylua` 双 parser 带来的 analyzer 分支复杂度。

2. **先 parse，后语义**
   - 先保证 MyLua 文件不报 syntax error。
   - 再逐步补齐 hover/goto/diagnostics/type inference。

3. **尽量复用现有 AST 节点语义**
   - `dollar_function` 尽量复用 function 相关结构。
   - `dollar_string` 尽量复用 string 相关结构。
   - `array_constructor` 第一阶段可近似为 table。

4. **`.lua` 严格模式用 diagnostics 实现**
   - parser 仍接受 MyLua 语法。
   - 对 `.lua` 文件中的 MyLua 专有语法报 warning/error。
   - `.mylua` 文件不报此类诊断。

5. **避免大规模重构**
   - 优先做局部 grammar 与 analyzer 适配。
   - 避免为了第一阶段支持而重写 summary/type/diagnostics 架构。
