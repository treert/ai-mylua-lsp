# Custom Require 注解设计

**日期**: 2026-07-31
**状态**: Approved
**关联**: `tests/lua-root/test_custom_require.lua`, `tests/lua-root/module_abc/abc_mgr.lua`

## 1. 背景与目标

### 现状

LSP 对 `require("mod.path")` 有内置特殊支持：
- **构建期** (`type_infer.rs::infer_call_return_type` / `visitors.rs::try_extract_require`)：识别 callee 文本 == `"require"`，提取第一个字符串字面量参数为 `module_path`，生成 `SymbolicStub::RequireRef { module_path }`。
- **解析期** (`resolver.rs::resolve_require`)：`RequireRef` → `agg.resolve_module_to_id(module_path)` → 取目标文件的 `module_return_type`。
- **document_link**：让 `require("mod")` 字符串可点击跳转。

### 问题

项目实际使用中，常自定义"类 require"的封装函数：

```lua
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    local module = require(module_path)
    return module
end

local a = custom_require("mgr_abc.abc_mgr")
a.test_print(a.version)
```

`custom_require` 函数体里的 `require(module_path)` 用的是**变量**而非字符串字面量，构建期无法静态求值，导致 `custom_require` 的推断返回值为 `Unknown`，下游 `a` 无法解析类型。

### 目标

通过自定义 EmmyLua 注解 `@customrequire`，让 LSP 能识别用户定义的类 require 函数：
1. 标记函数的某个参数为 module 路径参数
2. 支持对参数值做 regex 变换得到真实 module 路径
3. 让该函数的返回值解析为目标 module 的返回类型
4. 复用现有 `RequireRef` → `resolve_require` 链路

### 非目标

- 不支持多条变换规则链式
- 不实现语义高亮（P2）
- 不实现 document_link 对 custom require 参数的跳转（P2）
- 不对注解失效情况生成诊断（P1 静默降级）

## 2. 注解语法

### 形式

```
---@customrequire param=<name> [regex-pattern] [template]
```

- `param=<name>`：指定哪个参数是 module 路径参数（必填）
- `[regex-pattern]`：变换规则的 regex，使用 Rust `regex` crate 语法（可选）
- `[template]`：替换模板，`$1`/`$2`/... 是捕获组占位符，其余字符全部字面量（可选）

### 分隔规则

- 各部分以**空格**分隔
- 省略 pattern+template：直接用原参数值作为 require 路径
- 只有 pattern 无 template：template = ""（匹配部分删除为空串）
- template 取第一个空格后的**全部内容**（可含空格，不进一步切分）

### `=` 的处理

`emmy.rs` 的 Tokenizer 没有 `Equal` token 变体，`=` 字符在 tokenizer 中走 catch-all 分支被静默跳过。因此 `param=module_name` 的 token 流为两个相邻的 `Name` token：`Name("param")` `Name("module_name")`，解析时直接连续 `eat_name` 两次即可。

### 样例

```lua
-- 形态1：无变换，直接当 require 路径
---@customrequire param=module_name
function direct_require(module_name)
    return require(module_name)
end

-- 形态2：字面量替换（简单场景）
---@customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    return require(module_path)
end

-- 形态3：捕获重组（复杂场景）
---@customrequire param=module_name ^mgr\.(\w+)$ module_$1
function remap_require(module_name) ... end

-- 形态4：删除前缀（template 为空）
---@customrequire param=module_name ^mgr_\.
function strip_prefix(module_name) ... end
```

### 边界处理

- pattern 为空字符串：视为无变换规则
- 多个 `@customrequire` 注解：只取第一个，其余忽略
- `param=<name>` 的 name 在函数参数列表中找不到：注解失效
- regex 编译失败：注解失效，静默降级
- 所有失效情况均静默降级，不报诊断（P1）；后续可通过语义高亮提示（P2）

## 3. 数据结构改动

### 3.1 新增结构（`type_system.rs`）

```rust
/// 解析后的 @customrequire 注解规格
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomRequireSpec {
    /// 标记为 module 路径的参数名（如 "module_name"）
    pub param_name: LuaSymbol,
    /// 参数在签名中的位置索引（构建期填充，便于 O(1) 取值）
    pub param_index: u32,
    /// 变换规则；None 表示直接用原值
    pub transform: Option<ModulePathTransform>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModulePathTransform {
    /// regex 源字符串（序列化存源字符串，解析期使用时编译并缓存）
    pub pattern: String,
    pub template: String,
}
```

### 3.2 SymbolicStub 扩展（`type_system.rs`）

现有 `FunctionCallReturn` 只存类型不存值。新增字段携带原始字符串参数：

```rust
FunctionCallReturn {
    func_name: LuaSymbol,
    #[serde(default)]
    call_arg_types: Vec<TypeFact>,
    /// 新增：每个位置参数的原始字符串字面量值。
    /// 仅当调用实参是字符串字面量时为 Some；变量/表达式时为 None。
    #[serde(default)]
    raw_string_args: Vec<Option<String>>,
}
```

**设计决策**：复用 `FunctionCallReturn` 而非新建 stub 变体，理由：
- 少一个分支、序列化兼容（`#[serde(default)]` 保证旧数据可反序列化）
- 语义上 `FunctionCallReturn` 本就是"函数调用的返回值"，custom require 也是函数调用的一种

### 3.3 FunctionSummary 扩展（`summary.rs`）

```rust
pub struct FunctionSummary {
    // ... 现有字段 ...
    /// 当函数标注了 @customrequire 时存在
    #[serde(default)]
    pub custom_require: Option<CustomRequireSpec>,
}
```

## 4. 数据流

### 4.1 构建期（summary_builder）

#### 注解解析（`emmy.rs`）

新增 `EmmyAnnotation::CustomRequire` 变体：

```rust
pub enum EmmyAnnotation {
    // ... 现有变体 ...
    CustomRequire {
        param_name: String,
        pattern: Option<String>,
        template: Option<String>,
    },
}
```

`parse_annotation_line` 中 `match tag.as_str()` 新增分支：

```rust
"customrequire" => parse_ann_customrequire(tz),
```

解析函数核心逻辑：

```rust
fn parse_ann_customrequire(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let first = tz.eat_name()?;
    if first != "param" { return None; }
    // "=" 被 tokenizer 静默跳过，直接吃下一个 Name
    let param_name = tz.eat_name()?;
    // rest_as_string 返回原始源文本（保留 ^ \ $ + 等字符）
    let rest = tz.rest_as_string().trim().to_string();
    match rest.find(' ') {
        None => {
            if rest.is_empty() {
                Some(EmmyAnnotation::CustomRequire { param_name, pattern: None, template: None })
            } else {
                Some(EmmyAnnotation::CustomRequire {
                    param_name,
                    pattern: Some(rest),
                    template: Some(String::new()),
                })
            }
        }
        Some(idx) => Some(EmmyAnnotation::CustomRequire {
            param_name,
            pattern: Some(rest[..idx].to_string()),
            template: Some(rest[idx+1..].to_string()),
        }),
    }
}
```

关键点：`rest_as_string()` 返回原始源文本，pattern 里的 regex 元字符能正确保留。只有 `--`（Lua 注释）会截断 `consumable_end`，但 `@customrequire` 行内不会出现 `--`。

#### 注解消费（`summary_builder/emmy_visitors.rs`）

把 `CustomRequire` 注解填入 `FunctionSummary.custom_require`：
- 根据 `param_name` 在函数签名参数列表中查找位置，填充 `param_index`
- 找不到则不设置 `custom_require`（静默降级）

#### 调用返回值推断（`type_infer.rs::infer_call_return_type`）

生成 `FunctionCallReturn` 时，对每个实参节点判断：
- 是字符串字面量 → `Some(extracted_value)`
- 否则 → `None`

### 4.2 解析期（`resolver.rs`）

现有 `resolve_function_call_return` 逻辑：查 `global_shard` → 取 `FunctionRef` → 取第一个返回值。

新增分支：取到 `FunctionSummary` 后，若 `custom_require.is_some()`：

1. 从 `raw_string_args[param_index]` 取原始字符串值
   - 为 `None` → 降级返回 Unknown
2. 应用 `transform`：
   - 无 transform → module_path = 原值
   - 有 transform → 编译 regex（缓存）→ `re.replace_all(raw, template)` → module_path
   - regex 编译失败 → 降级返回 Unknown
3. 转为 `SymbolicStub::RequireRef { module_path }` → 复用现有 `resolve_require`

### 4.3 不变的部分

- `RequireRef` stub 与 `resolve_require` 逻辑完全复用
- module 索引、document_link 的核心逻辑不动
- 只在"生成 RequireRef 的入口"前加一层 custom require 拦截

## 5. fingerprint 扩展

`fingerprint.rs::hash_symbolic_stub` 需覆盖 `FunctionCallReturn` 的新字段 `raw_string_args`，确保序列化指纹包含新字段，避免缓存不一致。

`FunctionSummary` 的新字段 `custom_require` 也需纳入 `FunctionSummary` 的指纹计算。

regex 编译结果不序列化、只存源字符串，fingerprint 不受 regex 运行时编译结果影响。

## 6. 测试

### 测试组织

- Lua 测试样例内嵌在 Rust 测试代码中（和现有 `lsp/crates/mylua-lsp/tests/` 一致）
- `tests/lua-root/` 下的文件由用户手动维护，作为 VS Code 手工验证用，Rust 测试不依赖

### 集成测试

新建 `lsp/crates/mylua-lsp/tests/test_custom_require.rs`：

#### 用例1：无变换规则

```lua
--- @customrequire param=module_name
function direct_require(module_name) return require(module_name) end
local m = direct_require("module_abc.abc_mgr")
```
期望：`m` 解析为 abc_mgr 表，`m.version` hover = "1.0.0"

#### 用例2：字面量替换（核心样例）

```lua
--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    return require(module_path)
end
local a = custom_require("mgr_abc.abc_mgr")
```
期望：`a` 解析为 `RequireRef("module_abc.abc_mgr")` → abc_mgr 表

#### 用例3：捕获重组

```lua
--- @customrequire param=module_name ^mgr\.(\w+)$ module_$1
```
期望：`remap_require("mgr.abc_mgr")` → `RequireRef("module_abc_mgr")`

#### 用例4：删除前缀

```lua
--- @customrequire param=module_name ^mgr_\.
```
期望：template 为空串，匹配部分被删除

#### 用例5：静默降级（无字符串实参）

```lua
local prefix = "mgr_abc.abc_mgr"
local b = custom_require(prefix)
```
期望：`b` 类型为 Unknown，不报错

#### 用例6：regex 编译失败

```lua
--- @customrequire param=module_name [unclosed(
```
期望：注解失效，`x` 类型 Unknown，不崩溃

### 跨文件验收

将 `custom_require` 定义在 `utils/loader.lua`，在 `main.lua` 中 `require("utils.loader")` 后调用——验证 `FunctionSummary.custom_require` 通过 aggregation 正确传递。

### LSP 能力验收矩阵

| 能力 | 用例2期望 |
|------|----------|
| Hover | `a` 悬停显示 abc_mgr 表结构 |
| Goto Definition | `a.test_print` 跳转到 abc_mgr.lua 的函数定义 |
| Completion | `a.` 后补全 `version`/`init`/`update`/`get_name`/`test_print` |
| Diagnostics | `a.nonexistent` 报 undefined field（若启用） |

### 单元测试

`emmy.rs` 的 `#[cfg(test)] mod tests` 中加 `parse_ann_customrequire` 解析测试，验证 token 切分正确。

## 7. 实现范围

### P1（本次实现）

1. `type_system.rs`：新增 `CustomRequireSpec` / `ModulePathTransform`，扩展 `FunctionCallReturn.raw_string_args`
2. `emmy.rs`：新增 `EmmyAnnotation::CustomRequire` + `parse_ann_customrequire`
3. `summary.rs`：`FunctionSummary` 加 `custom_require: Option<CustomRequireSpec>` 字段
4. `summary_builder/emmy_visitors.rs`：把 `CustomRequire` 注解填入 `FunctionSummary`
5. `type_infer.rs`：`infer_call_return_type` 生成 `FunctionCallReturn` 时填充 `raw_string_args`
6. `resolver.rs`：`resolve_function_call_return` 增加 custom require 拦截分支
7. `fingerprint.rs`：扩展 `hash_symbolic_stub` 覆盖新字段
8. 集成测试 `test_custom_require.rs`
9. 单元测试（`emmy.rs` 内）

### P2（后续，本次不做）

- 语义高亮（`@customrequire` 行内的 token 着色）
- document_link 对 custom require 字符串参数的可点击跳转
- 多条变换规则链式
- 注解失效的诊断提示

## 8. 文档更新

按 AGENTS.md 规则，新增 LSP 能力 → 同次提交更新 `docs/lsp-capabilities.md`。
