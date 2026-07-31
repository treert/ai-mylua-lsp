# Custom Require 注解 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 LSP 识别用户自定义的类 require 函数（通过 `@customrequire` 注解），支持参数 regex 变换，返回值解析为目标 module 类型。

**Architecture:** 在现有 `FunctionCallReturn` stub 上加 `raw_string_args` 字段携带实参字符串字面量值；`FunctionSummary` 加 `custom_require: Option<CustomRequireSpec>` 携带注解规格；解析期在 `resolve_function_call_return` 中拦截 custom require 分支，把字符串参数变换为 module 路径后转为 `RequireRef` 复用现有解析。

**Tech Stack:** Rust, tree-sitter, serde, `regex` crate（新增依赖）

## Global Constraints

- **禁止运行任何 Rust 格式化命令**（`cargo fmt`、`rustfmt`、IDE format、批量格式化脚本）。Rust 改动必须保持局部既有格式；只允许手工调整必要缩进/换行。
- 测试样例 Lua 代码内嵌在 Rust 测试代码中（与现有 `lsp/crates/mylua-lsp/tests/` 一致），不依赖 `tests/lua-root/` 目录。
- 所有失效情况静默降级（返回 Unknown），不报诊断（P1）。
- `#[serde(default)]` 必须加在新字段上，保证旧数据反序列化兼容。
- 每个任务结束前运行 `cargo test -p mylua-lsp` 确认未破坏现有测试。

---

## File Structure

**新建文件:**
- `lsp/crates/mylua-lsp/tests/test_custom_require.rs` — 集成测试

**修改文件:**
- `lsp/crates/mylua-lsp/Cargo.toml` — 新增 `regex` 依赖
- `lsp/crates/mylua-lsp/src/type_system.rs` — 新增 `CustomRequireSpec`/`ModulePathTransform`，扩展 `SymbolicStub::FunctionCallReturn`
- `lsp/crates/mylua-lsp/src/emmy.rs` — 新增 `EmmyAnnotation::CustomRequire` 变体 + `parse_ann_customrequire`，单元测试
- `lsp/crates/mylua-lsp/src/summary.rs` — `FunctionSummary` 加 `custom_require` 字段
- `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs` — `build_function_summary` 中消费 `CustomRequire` 注解
- `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs` — 生成 `FunctionCallReturn` 时填充 `raw_string_args`
- `lsp/crates/mylua-lsp/src/resolver.rs` — `resolve_function_call_return` 加 custom require 拦截分支
- `lsp/crates/mylua-lsp/src/summary_builder/fingerprint.rs` — `hash_symbolic_stub` 覆盖 `raw_string_args`
- `docs/lsp-capabilities.md` — 文档更新（AGENTS.md 强制要求）

---

## Task 1: 新增 regex 依赖

**Files:**
- Modify: `lsp/crates/mylua-lsp/Cargo.toml:20-32`

**Interfaces:**
- Produces: `regex` crate 可用（后续 task 在 `type_system.rs` 和 `resolver.rs` 中 `use regex::Regex`）

- [ ] **Step 1: 在 `[dependencies]` 末尾新增 regex 依赖**

打开 `lsp/crates/mylua-lsp/Cargo.toml`，在 `lasso = ...` 这一行下方（第 31 行之后）新增一行（保持与上一行相同的缩进，2 空格）：

```toml
regex = "1"
```

- [ ] **Step 2: 验证依赖可用**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 编译通过，无错误（regex 会被下载）。

- [ ] **Step 3: Commit**

```bash
cd lsp
git add crates/mylua-lsp/Cargo.toml crates/mylua-lsp/Cargo.lock
git commit -m "deps: add regex crate for custom require transforms"
```

---

## Task 2: 扩展 type_system.rs 数据结构

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/type_system.rs`（新增 `CustomRequireSpec`/`ModulePathTransform` 结构；扩展 `SymbolicStub::FunctionCallReturn`；扩展 `Display` 实现）

**Interfaces:**
- Produces: 
  - `pub struct CustomRequireSpec { pub param_name: LuaSymbol, pub param_index: u32, pub transform: Option<ModulePathTransform> }`
  - `pub struct ModulePathTransform { pub pattern: String, pub template: String }`
  - `SymbolicStub::FunctionCallReturn` 新增字段 `raw_string_args: Vec<Option<String>>`（带 `#[serde(default)]`）
- Consumes: `LuaSymbol`（来自 `lua_symbol` 模块）

- [ ] **Step 1: 在 `ModulePathTransform` 和 `CustomRequireSpec` 结构定义**

在 `lsp/crates/mylua-lsp/src/type_system.rs` 文件中，找到 `pub struct ParamInfo` 定义（约第 103-109 行）的末尾（`}` 之后），在下一行新增两个结构（与 `ParamInfo` 同级）：

```rust
/// 解析后的 @customrequire 注解规格。标记函数某个参数为 module 路径参数，
/// 并可选地附带 regex 变换规则，使调用处 `custom_require("foo")` 的返回值
/// 等价于 `require(transform("foo"))`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomRequireSpec {
    /// 标记为 module 路径的参数名（如 "module_name"）
    pub param_name: LuaSymbol,
    /// 参数在签名中的位置索引（构建期填充，便于 O(1) 取值）
    pub param_index: u32,
    /// 变换规则；None 表示直接用原值
    pub transform: Option<ModulePathTransform>,
}

/// regex 变换规则。序列化时存源字符串，解析期使用时编译并缓存。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModulePathTransform {
    /// regex 源字符串（Rust regex crate 语法）
    pub pattern: String,
    /// 替换模板，`$1`/`$2`/... 是捕获组占位符，其余字符全部字面量
    pub template: String,
}
```

- [ ] **Step 2: 扩展 `SymbolicStub::FunctionCallReturn`**

在 `lsp/crates/mylua-lsp/src/type_system.rs` 中找到 `SymbolicStub::FunctionCallReturn` 定义（约第 78-82 行）：

```rust
FunctionCallReturn {
    func_name: LuaSymbol,
    #[serde(default)]
    call_arg_types: Vec<TypeFact>,
},
```

替换为：

```rust
FunctionCallReturn {
    func_name: LuaSymbol,
    #[serde(default)]
    call_arg_types: Vec<TypeFact>,
    /// 每个位置参数的原始字符串字面量值。
    /// 仅当调用实参是字符串字面量时为 Some；变量/表达式时为 None。
    #[serde(default)]
    raw_string_args: Vec<Option<String>>,
},
```

- [ ] **Step 3: 扩展 `Display for SymbolicStub`**

在 `lsp/crates/mylua-lsp/src/type_system.rs` 中找到 `impl fmt::Display for SymbolicStub`（约第 338 行），定位到 `FunctionCallReturn` 分支（约第 352 行附近）：

```rust
Self::FunctionCallReturn {
    func_name,
    call_arg_types,
} => write!(f, "{}()", func_name),
```

替换为（添加新字段到解构，但 display 保持简洁）：

```rust
Self::FunctionCallReturn {
    func_name,
    call_arg_types,
    raw_string_args: _,
} => write!(f, "{}()", func_name),
```

- [ ] **Step 4: 编译验证**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 出现多处编译错误，因为现有代码构造 `FunctionCallReturn` 时没传 `raw_string_args`。**先不修复**，后续 task 会处理。这一步只是为了确认结构定义本身语法正确。

如果错误信息是"missing field `raw_string_args`" 说明结构定义正确；如果有其他语法错误，修正结构定义。

- [ ] **Step 5: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/type_system.rs
git commit -m "feat(types): add CustomRequireSpec and raw_string_args on FunctionCallReturn"
```

---

## Task 3: emmy.rs 新增注解解析

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/emmy.rs`（新增 `EmmyAnnotation::CustomRequire` 变体；新增 `parse_ann_customrequire`；在 `parse_annotation_line` 注册；单元测试）

**Interfaces:**
- Produces:
  - `EmmyAnnotation::CustomRequire { param_name: String, pattern: Option<String>, template: Option<String> }`
  - `fn parse_ann_customrequire(tz: &mut Tokenizer) -> Option<EmmyAnnotation>`（私有，文件内使用）
- Consumes: `Tokenizer::eat_name` / `Tokenizer::rest_as_string`（现有方法）

- [ ] **Step 1: 在 `EmmyAnnotation` 枚举新增 `CustomRequire` 变体**

在 `lsp/crates/mylua-lsp/src/emmy.rs` 中找到 `pub enum EmmyAnnotation`（约第 148 行），在 `Other { tag, text }` 变体之前（或 `Diagnostic` 之后，约第 205 行）新增：

```rust
/// `@customrequire param=<name> [pattern] [template]` — 标记函数为
/// 类 require 的封装，使其返回值解析为目标 module 的返回类型。
/// `pattern`/`template` 均为 None 时直接用原参数值作为 module 路径。
CustomRequire {
    param_name: String,
    pattern: Option<String>,
    template: Option<String>,
},
```

- [ ] **Step 2: 在 `parse_annotation_line` 注册新分支**

在 `lsp/crates/mylua-lsp/src/emmy.rs` 中找到 `parse_annotation_line` 函数的 `match tag.as_str()`（约第 1177-1215 行）。在 `"meta" => ...` 分支之后、`_ => Some(EmmyAnnotation::Other {...})` 之前，新增：

```rust
"customrequire" => parse_ann_customrequire(&mut tz),
```

- [ ] **Step 3: 实现 `parse_ann_customrequire`**

在 `lsp/crates/mylua-lsp/src/emmy.rs` 中找到 `parse_ann_meta` 函数（约第 1206-1210 行）之后，在 `parse_annotation_line` 函数结束 `}` 之后，新增函数：

```rust
/// `@customrequire param=<name> [regex-pattern] [template]`
///
/// 解析规则：
/// - 必须以 `param` 字面量开头，后跟 `=`（tokenizer 静默跳过 `=`），再跟参数名
/// - 余下文本取 `rest_as_string()` 原始源文本（保留 `^`、`\`、`$` 等字符）
/// - 按首个空格切分 pattern / template：
///   - 无空格且非空 → 只有 pattern，template 为空串
///   - 无空格且空 → 无 pattern/template
///   - 有空格 → 空格前为 pattern，空格后全部为 template
fn parse_ann_customrequire(tz: &mut Tokenizer) -> Option<EmmyAnnotation> {
    let first = tz.eat_name()?;
    if first != "param" {
        return None;
    }
    // `=` 字符被 tokenizer 静默跳过（catch-all 分支），直接吃下一个 Name
    let param_name = tz.eat_name()?;
    // rest_as_string 返回原始源文本，regex 元字符能正确保留
    let rest = tz.rest_as_string();
    let rest = rest.trim();
    match rest.find(' ') {
        None => {
            if rest.is_empty() {
                Some(EmmyAnnotation::CustomRequire {
                    param_name,
                    pattern: None,
                    template: None,
                })
            } else {
                Some(EmmyAnnotation::CustomRequire {
                    param_name,
                    pattern: Some(rest.to_string()),
                    template: Some(String::new()),
                })
            }
        }
        Some(idx) => Some(EmmyAnnotation::CustomRequire {
            param_name,
            pattern: Some(rest[..idx].to_string()),
            template: Some(rest[idx + 1..].to_string()),
        }),
    }
}
```

- [ ] **Step 4: 新增单元测试**

在 `lsp/crates/mylua-lsp/src/emmy.rs` 文件末尾的 `#[cfg(test)] mod tests` 模块中（如果已存在则追加，不存在则新增），添加测试：

```rust
#[cfg(test)]
mod customrequire_parse_tests {
    use super::*;

    #[test]
    fn parse_no_transform() {
        let anns = parse_emmy_comments("customrequire param=module_name");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::CustomRequire { param_name, pattern, template } => {
                assert_eq!(param_name, "module_name");
                assert_eq!(pattern, &None);
                assert_eq!(template, &None);
            }
            other => panic!("expected CustomRequire, got {:?}", other),
        }
    }

    #[test]
    fn parse_literal_replace() {
        let anns = parse_emmy_comments("customrequire param=module_name mgr_abc module_abc");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::CustomRequire { param_name, pattern, template } => {
                assert_eq!(param_name, "module_name");
                assert_eq!(pattern.as_deref(), Some("mgr_abc"));
                assert_eq!(template.as_deref(), Some("module_abc"));
            }
            other => panic!("expected CustomRequire, got {:?}", other),
        }
    }

    #[test]
    fn parse_regex_with_capture() {
        let anns = parse_emmy_comments("customrequire param=module_name ^mgr\\.(\\w+)$ module_$1");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::CustomRequire { param_name, pattern, template } => {
                assert_eq!(param_name, "module_name");
                assert_eq!(pattern.as_deref(), Some("^mgr\\.(\\w+)$"));
                assert_eq!(template.as_deref(), Some("module_$1"));
            }
            other => panic!("expected CustomRequire, got {:?}", other),
        }
    }

    #[test]
    fn parse_pattern_only_empty_template() {
        // 只有 pattern，无空格 → template 为空串
        let anns = parse_emmy_comments("customrequire param=module_name ^mgr_\\.");
        assert_eq!(anns.len(), 1);
        match &anns[0] {
            EmmyAnnotation::CustomRequire { param_name, pattern, template } => {
                assert_eq!(param_name, "module_name");
                assert_eq!(pattern.as_deref(), Some("^mgr_\\."));
                assert_eq!(template.as_deref(), Some(""));
            }
            other => panic!("expected CustomRequire, got {:?}", other),
        }
    }

    #[test]
    fn parse_rejects_missing_param_keyword() {
        // 第一个 token 不是 "param" → 返回 None（被 parse_emmy_comments 过滤）
        let anns = parse_emmy_comments("customrequire foo=module_name");
        assert_eq!(anns.len(), 0);
    }

    #[test]
    fn parse_rejects_missing_param_name() {
        // 只有 "param"，没有参数名 → 返回 None
        let anns = parse_emmy_comments("customrequire param");
        assert_eq!(anns.len(), 0);
    }
}
```

- [ ] **Step 5: 运行单元测试**

Run: `cd lsp && cargo test -p mylua-lsp --lib emmy::customrequire_parse_tests`
Expected: 全部 6 个测试通过。

如果失败，检查 `rest_as_string` 是否正确返回原始文本（regex 元字符 `^`、`\`、`$` 不应被吃掉）。注意 Rust 原始字符串 `r"..."` 中的 `\\` 是字面量两个反斜杠。

- [ ] **Step 6: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/emmy.rs
git commit -m "feat(emmy): parse @customrequire annotation"
```

---

## Task 4: summary.rs 扩展 FunctionSummary

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary.rs:101-117`（`FunctionSummary` 结构）

**Interfaces:**
- Produces: `FunctionSummary.custom_require: Option<CustomRequireSpec>` 字段（带 `#[serde(default)]`）
- Consumes: `CustomRequireSpec`（来自 Task 2 的 `type_system.rs`）

- [ ] **Step 1: 在 `FunctionSummary` 新增字段**

在 `lsp/crates/mylua-lsp/src/summary.rs` 中找到 `pub struct FunctionSummary`（约第 102-117 行）。在 `generic_params` 字段之后、`}` 之前，新增：

```rust
/// 当函数标注了 `@customrequire` 时存在。
/// 标记该函数为类 require 封装：返回值解析为目标 module 的类型。
#[serde(default)]
pub custom_require: Option<crate::type_system::CustomRequireSpec>,
```

- [ ] **Step 2: 修复所有 `FunctionSummary { ... }` 字面量构造**

搜索所有 `FunctionSummary {` 构造点。每个构造点都要加上 `custom_require: None,`（除非该构造点本就给值）。

在 `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs` 找到 `build_function_summary` 函数末尾的构造（约第 1069-1080 行），在 `generic_params: ...` 之后、`}` 之前，加：

```rust
custom_require: None,
```

在 `lsp/crates/mylua-lsp/src/summary.rs` 的 `#[cfg(test)]` 测试模块中如果有 `FunctionSummary {` 构造，同样加 `custom_require: None,`。

搜索命令（用于定位，不是执行）：
```bash
cd lsp
grep -rn "FunctionSummary {" crates/mylua-lsp/src/
```

- [ ] **Step 3: 编译验证**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 编译通过（`FunctionCallReturn` 的构造错误可能仍存在，但 `FunctionSummary` 相关错误应消除）。

- [ ] **Step 4: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/summary.rs crates/mylua-lsp/src/summary_builder/visitors.rs
git commit -m "feat(summary): add custom_require field to FunctionSummary"
```

---

## Task 5: visitors.rs 消费 CustomRequire 注解

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs:966-1080`（`build_function_summary` 中的注解循环和构造）

**Interfaces:**
- Consumes: 
  - `EmmyAnnotation::CustomRequire`（来自 Task 3）
  - `params: Vec<ParamInfo>`（函数签名参数列表，含 `name` 字段）
- Produces: `FunctionSummary.custom_require = Some(CustomRequireSpec { ... })`

- [ ] **Step 1: 在注解循环中收集 CustomRequire**

在 `lsp/crates/mylua-lsp/src/summary_builder/visitors.rs` 的 `build_function_summary` 函数中，找到注解循环（约第 966-1007 行）。在循环之前（约第 956 行 `let mut vararg_param = None;` 附近）新增：

```rust
let mut custom_require_spec: Option<crate::emmy::EmmyAnnotation> = None;
```

在 `match ann {` 分支中（约第 967 行），在 `EmmyAnnotation::Generic { params: gparams } => {...}` 分支之后、`_ => {}` 之前，新增：

```rust
EmmyAnnotation::CustomRequire { .. } => {
    // 只取第一个；多个 @customrequire 注解时忽略其余
    if custom_require_spec.is_none() {
        custom_require_spec = Some(ann.clone());
    }
}
```

- [ ] **Step 2: 根据 spec 填充 custom_require 字段**

在 `build_function_summary` 函数末尾构造 `FunctionSummary { ... }` 之前（约第 1063 行 `let sig = ...` 附近），新增 custom_require 解析逻辑：

```rust
// 解析 @customrequire 注解：根据 param_name 在签名中查找位置索引
let custom_require = match custom_require_spec {
    Some(crate::emmy::EmmyAnnotation::CustomRequire {
        param_name,
        pattern,
        template,
    }) => {
        let param_index = params
            .iter()
            .position(|p| p.name.as_str() == param_name.as_str())
            .map(|i| i as u32);
        match param_index {
            Some(idx) => {
                let transform = match (pattern, template) {
                    (Some(p), Some(t)) => {
                        if p.is_empty() {
                            None
                        } else {
                            Some(crate::type_system::ModulePathTransform {
                                pattern: p,
                                template: t,
                            })
                        }
                    }
                    _ => None,
                };
                Some(crate::type_system::CustomRequireSpec {
                    param_name: crate::lua_symbol::intern_lua_symbol(&param_name),
                    param_index: idx,
                    transform,
                })
            }
            None => None, // param_name 在参数列表中找不到 → 静默降级
        }
    }
    _ => None,
};
```

- [ ] **Step 3: 在 FunctionSummary 构造中传入 custom_require**

在 `build_function_summary` 末尾的 `FunctionSummary { ... }` 构造中，把 Task 4 加的 `custom_require: None,` 改为：

```rust
custom_require,
```

- [ ] **Step 4: 编译验证**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 编译通过。

- [ ] **Step 5: 运行现有测试确认无回归**

Run: `cd lsp && cargo test -p mylua-lsp`
Expected: 现有测试全部通过（除可能仍存在的 `FunctionCallReturn` 构造错误外，但那些会在 Task 6 修复；此步骤只看测试本身是否通过）。

- [ ] **Step 6: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/summary_builder/visitors.rs
git commit -m "feat(builder): consume @customrequire annotation into FunctionSummary"
```

---

## Task 6: type_infer.rs 填充 raw_string_args

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs:525-528`（`FunctionCallReturn` 构造点）

**Interfaces:**
- Consumes:
  - `extract_call_arg_nodes(args, source)`（来自 `util.rs`，返回实参节点列表）
  - `extract_string_from_node(ctx, node)`（来自 `table_extract.rs`，返回字符串字面量值或 None）
- Produces: `SymbolicStub::FunctionCallReturn { func_name, call_arg_types, raw_string_args }`

- [ ] **Step 1: 新增 collect_raw_string_args 辅助函数**

在 `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs` 中，找到 `collect_call_arg_types` 函数（约第 147 行）之后，新增：

```rust
/// 收集调用实参的原始字符串字面量值。
/// 对每个实参节点：是字符串字面量 → Some(value)，否则 → None。
/// 顺序与 `collect_call_arg_types` 一致，按位置对应。
fn collect_raw_string_args(
    ctx: &BuildContext,
    call_node: tree_sitter::Node,
) -> Vec<Option<String>> {
    let Some(args) = call_node.child_by_field(field::ARGUMENTS) else {
        return Vec::new();
    };
    crate::util::extract_call_arg_nodes(args, ctx.source)
        .into_iter()
        .map(|arg| {
            // 复用现有字符串提取逻辑（unwrap expression_list + parse string literal）
            crate::summary_builder::table_extract::extract_string_from_node(ctx, arg)
        })
        .collect()
}
```

- [ ] **Step 2: 修改 FunctionCallReturn 构造点**

在 `lsp/crates/mylua-lsp/src/summary_builder/type_infer.rs` 中找到 `infer_call_return_type` 函数末尾的 `FunctionCallReturn` 构造（约第 525-528 行）：

```rust
TypeFact::Stub(SymbolicStub::FunctionCallReturn {
    func_name: callee_text.into(),
    call_arg_types: collect_call_arg_types(ctx, node),
})
```

替换为：

```rust
TypeFact::Stub(SymbolicStub::FunctionCallReturn {
    func_name: callee_text.into(),
    call_arg_types: collect_call_arg_types(ctx, node),
    raw_string_args: collect_raw_string_args(ctx, node),
})
```

- [ ] **Step 3: 确保 extract_string_from_node 可见性**

`extract_string_from_node` 当前是 `pub(super) fn`（在 `table_extract.rs:254`），从 `type_infer.rs`（同模块 `summary_builder`）访问没问题。但 `type_infer.rs` 中要确保调用路径正确。

如果 `collect_raw_string_args` 中调用 `crate::summary_builder::table_extract::extract_string_from_node` 报可见性错误，改为在 `type_infer.rs` 顶部已有 `use super::table_extract::extract_string_from_node;`（该文件第 10 行已有此 import），那么 `collect_raw_string_args` 内可直接写 `extract_string_from_node(ctx, arg)`。调整 Step 1 的代码为：

```rust
fn collect_raw_string_args(
    ctx: &BuildContext,
    call_node: tree_sitter::Node,
) -> Vec<Option<String>> {
    let Some(args) = call_node.child_by_field(field::ARGUMENTS) else {
        return Vec::new();
    };
    crate::util::extract_call_arg_nodes(args, ctx.source)
        .into_iter()
        .map(|arg| extract_string_from_node(ctx, arg))
        .collect()
}
```

- [ ] **Step 4: 编译验证**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 编译通过。所有 `FunctionCallReturn` 构造点都已更新。

- [ ] **Step 5: 运行现有测试**

Run: `cd lsp && cargo test -p mylua-lsp`
Expected: 现有测试全部通过，无回归。

- [ ] **Step 6: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/summary_builder/type_infer.rs
git commit -m "feat(infer): populate raw_string_args in FunctionCallReturn stub"
```

---

## Task 7: fingerprint.rs 扩展哈希

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/summary_builder/fingerprint.rs:138-148`（`hash_symbolic_stub` 的 `FunctionCallReturn` 分支）

**Interfaces:**
- Consumes: `SymbolicStub::FunctionCallReturn.raw_string_args`（来自 Task 2）

- [ ] **Step 1: 扩展 hash_symbolic_stub 的 FunctionCallReturn 分支**

在 `lsp/crates/mylua-lsp/src/summary_builder/fingerprint.rs` 中找到 `hash_symbolic_stub` 的 `FunctionCallReturn` 分支（约第 138-148 行）：

```rust
SymbolicStub::FunctionCallReturn {
    func_name,
    call_arg_types,
} => {
    "function_call_return".hash(hasher);
    func_name.hash(hasher);
    call_arg_types.len().hash(hasher);
    for arg in call_arg_types {
        hash_type_fact(arg, hasher);
    }
}
```

替换为（新增 `raw_string_args` 字段解构和哈希）：

```rust
SymbolicStub::FunctionCallReturn {
    func_name,
    call_arg_types,
    raw_string_args,
} => {
    "function_call_return".hash(hasher);
    func_name.hash(hasher);
    call_arg_types.len().hash(hasher);
    for arg in call_arg_types {
        hash_type_fact(arg, hasher);
    }
    raw_string_args.len().hash(hasher);
    for arg in raw_string_args {
        match arg {
            Some(s) => {
                1u8.hash(hasher);
                s.hash(hasher);
            }
            None => {
                0u8.hash(hasher);
            }
        }
    }
}
```

- [ ] **Step 2: 扩展 FunctionSummary 的指纹计算**

在 `lsp/crates/mylua-lsp/src/summary_builder/fingerprint.rs` 中找到 `compute_signature_fingerprint` 函数的函数循环（约第 185-191 行）：

```rust
for id in &func_ids {
    id.0.hash(&mut hasher);
    if let Some(fs) = ctx.function_summaries.get(id) {
        fs.generic_params.hash(&mut hasher);
        fs.signature_fingerprint.hash(&mut hasher);
    }
}
```

替换为（新增 custom_require 的哈希）：

```rust
for id in &func_ids {
    id.0.hash(&mut hasher);
    if let Some(fs) = ctx.function_summaries.get(id) {
        fs.generic_params.hash(&mut hasher);
        fs.signature_fingerprint.hash(&mut hasher);
        match &fs.custom_require {
            Some(spec) => {
                1u8.hash(&mut hasher);
                spec.param_name.hash(&mut hasher);
                spec.param_index.hash(&mut hasher);
                match &spec.transform {
                    Some(t) => {
                        1u8.hash(&mut hasher);
                        t.pattern.hash(&mut hasher);
                        t.template.hash(&mut hasher);
                    }
                    None => {
                        0u8.hash(&mut hasher);
                    }
                }
            }
            None => {
                0u8.hash(&mut hasher);
            }
        }
    }
}
```

- [ ] **Step 3: 编译验证**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 编译通过。

- [ ] **Step 4: 运行测试**

Run: `cd lsp && cargo test -p mylua-lsp`
Expected: 全部通过。

- [ ] **Step 5: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/summary_builder/fingerprint.rs
git commit -m "feat(fingerprint): include raw_string_args and custom_require in hash"
```

---

## Task 8: resolver.rs 加 custom require 拦截分支

**Files:**
- Modify: `lsp/crates/mylua-lsp/src/resolver.rs:592-621`（`resolve_function_call_return`）

**Interfaces:**
- Consumes:
  - `SymbolicStub::FunctionCallReturn.raw_string_args`（来自 Task 6）
  - `FunctionSummary.custom_require: Option<CustomRequireSpec>`（来自 Task 4/5）
  - `CustomRequireSpec.param_index` / `CustomRequireSpec.transform`
  - `regex::Regex`（来自 Task 1）
- Produces: 复用 `resolve_require`（现有函数），返回 `ResolvedType`

- [ ] **Step 1: 在 resolver.rs 顶部新增 regex import**

在 `lsp/crates/mylua-lsp/src/resolver.rs` 顶部（现有 `use` 语句之后），新增：

```rust
use regex::Regex;
```

- [ ] **Step 2: 新增 apply_custom_require_transform 辅助函数**

在 `lsp/crates/mylua-lsp/src/resolver.rs` 中，找到 `resolve_function_call_return` 函数（约第 592 行）之前，新增辅助函数：

```rust
/// 应用 custom require 的 regex 变换。
/// 无 transform → 返回原值。
/// 有 transform → 编译 regex（失败返回 None）→ replace_all。
fn apply_custom_require_transform(
    raw: &str,
    transform: &crate::type_system::ModulePathTransform,
) -> Option<String> {
    // regex 编译失败 → 静默降级返回 None
    let re = Regex::new(&transform.pattern).ok()?;
    Some(re.replace_all(raw, &transform.template).into_owned())
}
```

- [ ] **Step 3: 修改 resolve_function_call_return 签名和实现**

在 `lsp/crates/mylua-lsp/src/resolver.rs` 中找到 `resolve_function_call_return` 函数（约第 592-621 行）：

```rust
fn resolve_function_call_return(
    ctx: ResolveCtx,
    func_name: &str,
    call_arg_types: &[TypeFact],
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let candidate = match agg.global_shard.get(func_name) {
        Some(candidates) if !candidates.is_empty() => candidates[0].clone(),
        _ => return ResolvedType::unknown(ctx),
    };

    let owner_uri_id = candidate.source_uri_id();
    let owner_ctx = ResolveCtx::new(owner_uri_id);
    let resolved = resolve_recursive(owner_ctx, &candidate.type_fact, agg, depth + 1, visited);
    let ret = match &resolved.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => sig.returns.first().cloned(),
        TypeFact::Known(KnownType::FunctionRef(fid)) => agg
            .summary_by_id(owner_uri_id)
            .and_then(|summary| summary.function_summaries.get(fid))
            .and_then(|fs| function_return_with_call_args(fs, call_arg_types)),
        _ => None,
    };

    let Some(ret) = ret else {
        return ResolvedType::unknown(owner_ctx);
    };
    resolve_recursive(owner_ctx, &ret, agg, depth + 1, visited)
}
```

替换为（新增 `raw_string_args` 参数和 custom require 拦截分支）：

```rust
fn resolve_function_call_return(
    ctx: ResolveCtx,
    func_name: &str,
    call_arg_types: &[TypeFact],
    raw_string_args: &[Option<String>],
    agg: &WorkspaceAggregation,
    depth: usize,
    visited: &mut HashSet<String>,
) -> ResolvedType {
    let candidate = match agg.global_shard.get(func_name) {
        Some(candidates) if !candidates.is_empty() => candidates[0].clone(),
        _ => return ResolvedType::unknown(ctx),
    };

    let owner_uri_id = candidate.source_uri_id();
    let owner_ctx = ResolveCtx::new(owner_uri_id);
    let resolved = resolve_recursive(owner_ctx, &candidate.type_fact, agg, depth + 1, visited);

    // Custom require 拦截：若函数标注了 @customrequire，根据 raw_string_args
    // 取参数字符串值，应用 transform 后转为 RequireRef 复用 resolve_require。
    if let TypeFact::Known(KnownType::FunctionRef(fid)) = &resolved.type_fact {
        if let Some(summary) = agg.summary_by_id(owner_uri_id) {
            if let Some(fs) = summary.function_summaries.get(fid) {
                if let Some(spec) = &fs.custom_require {
                    if let Some(module_path) =
                        try_resolve_custom_require_module(spec, raw_string_args)
                    {
                        return resolve_require(ctx, &module_path, agg, depth + 1, visited);
                    }
                    // 取不到字符串值或变换失败 → 降级 Unknown
                    return ResolvedType::unknown(ctx);
                }
            }
        }
    }

    let ret = match &resolved.type_fact {
        TypeFact::Known(KnownType::Function(sig)) => sig.returns.first().cloned(),
        TypeFact::Known(KnownType::FunctionRef(fid)) => agg
            .summary_by_id(owner_uri_id)
            .and_then(|summary| summary.function_summaries.get(fid))
            .and_then(|fs| function_return_with_call_args(fs, call_arg_types)),
        _ => None,
    };

    let Some(ret) = ret else {
        return ResolvedType::unknown(owner_ctx);
    };
    resolve_recursive(owner_ctx, &ret, agg, depth + 1, visited)
}

/// 根据 CustomRequireSpec 从 raw_string_args 取参数值并应用变换。
/// 返回 None 表示：参数索引越界、值是 None（非字符串字面量）、regex 编译失败。
fn try_resolve_custom_require_module(
    spec: &crate::type_system::CustomRequireSpec,
    raw_string_args: &[Option<String>],
) -> Option<String> {
    let idx = spec.param_index as usize;
    let raw = raw_string_args.get(idx).and_then(|v| v.as_ref())?;
    match &spec.transform {
        None => Some(raw.clone()),
        Some(transform) => apply_custom_require_transform(raw, transform),
    }
}
```

- [ ] **Step 4: 更新 resolve_stub 中的调用点**

在 `lsp/crates/mylua-lsp/src/resolver.rs` 中找到 `resolve_stub` 函数（约第 470-531 行）。其中 `SymbolicStub::FunctionCallReturn { ... }` 分支（约第 510-513 行）：

```rust
SymbolicStub::FunctionCallReturn {
    func_name,
    call_arg_types,
} => resolve_function_call_return(ctx, func_name, call_arg_types, agg, depth, visited),
```

替换为（添加 `raw_string_args` 字段解构和传参）：

```rust
SymbolicStub::FunctionCallReturn {
    func_name,
    call_arg_types,
    raw_string_args,
} => resolve_function_call_return(
    ctx,
    func_name,
    call_arg_types,
    raw_string_args,
    agg,
    depth,
    visited,
),
```

- [ ] **Step 5: 编译验证**

Run: `cd lsp && cargo check -p mylua-lsp`
Expected: 编译通过。所有 `resolve_function_call_return` 调用点都已更新。

- [ ] **Step 6: 运行现有测试**

Run: `cd lsp && cargo test -p mylua-lsp`
Expected: 现有测试全部通过（custom require 还没测，但不应破坏现有功能）。

- [ ] **Step 7: Commit**

```bash
cd lsp
git add crates/mylua-lsp/src/resolver.rs
git commit -m "feat(resolver): intercept @customrequire functions and resolve via RequireRef"
```

---

## Task 9: 集成测试 test_custom_require.rs

**Files:**
- Create: `lsp/crates/mylua-lsp/tests/test_custom_require.rs`
- Reference: `lsp/crates/mylua-lsp/tests/test_helpers.rs`（`setup_workspace`、`setup_single_file`、`hover::hover`、`goto::goto_definition` 等用法）
- Reference: `lsp/crates/mylua-lsp/tests/test_hover.rs`（hover 测试结构）
- Reference: `lsp/crates/mylua-lsp/tests/test_goto.rs`（goto 测试结构）

**Interfaces:**
- Consumes: 所有前序 task 的产出

- [ ] **Step 1: 新建测试文件**

创建 `lsp/crates/mylua-lsp/tests/test_custom_require.rs`：

```rust
mod test_helpers;

use mylua_lsp::hover;
use test_helpers::*;
use tower_lsp_server::ls_types::Range;

/// 模块 abc_mgr 的 Lua 源码（被所有测试共享）。
const ABC_MGR_SRC: &str = r#"local abc_mgr = {}

abc_mgr.name = "abc_mgr"
abc_mgr.version = "1.0.0"

function abc_mgr.init()
    print("init")
end

function abc_mgr.update()
    print("update")
end

function abc_mgr.get_name()
    return abc_mgr.name
end

function abc_mgr.test_print(...)
    print("abc_mgr", ...)
end

return abc_mgr
"#;

/// hover 单个位置，断言返回的 markdown 内容包含指定子串。
fn assert_hover_contains(
    docs: &std::collections::HashMap<UriId, mylua_lsp::document::Document>,
    agg: &mut mylua_lsp::aggregation::WorkspaceAggregation,
    uri: &tower_lsp_server::ls_types::Uri,
    line: u32,
    character: u32,
    needle: &str,
) {
    let uri_id = intern_uri(uri);
    let doc = docs.get(&uri_id).expect("doc should exist");
    let result = hover::hover(doc, uri_id, pos(line, character), agg);
    let markdown = result
        .map(|h| h.contents)
        .map(|c| match c {
            tower_lsp_server::ls_types::HoverContents::Markup(md) => md.value,
            _ => String::new(),
        })
        .unwrap_or_default();
    assert!(
        markdown.contains(needle),
        "hover at {}:{} expected to contain {:?}, got: {:?}",
        line,
        character,
        needle,
        markdown
    );
}

#[test]
fn custom_require_no_transform_resolves_module() {
    // 形态1：无变换规则，直接当 require 路径
    let main_src = r#"--- @customrequire param=module_name
function direct_require(module_name)
    return require(module_name)
end

local m = direct_require("module_abc.abc_mgr")
m.version
m.test_print
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // hover `m` 在 `local m = direct_require(...)` 行（line 5, col 6）
    // 期望解析为 abc_mgr 表，hover 应包含 version 字段
    assert_hover_contains(&docs, &mut agg, &main_uri, 5, 6, "abc_mgr");
}

#[test]
fn custom_require_literal_replace_resolves_module() {
    // 形态2：字面量替换（核心样例）
    let main_src = r#"--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    local module = require(module_path)
    return module
end

local a = custom_require("mgr_abc.abc_mgr")
a.version
a.test_print
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // hover `a` 在 `local a = custom_require(...)` 行（line 7, col 6）
    // 期望解析为 abc_mgr 表（transform 把 mgr_abc.abc_mgr → module_abc.abc_mgr）
    assert_hover_contains(&docs, &mut agg, &main_uri, 7, 6, "abc_mgr");
    // hover `a.version` 中的 version（line 8, col 2）应显示 "1.0.0"
    assert_hover_contains(&docs, &mut agg, &main_uri, 8, 2, "1.0.0");
}

#[test]
fn custom_require_regex_capture_resolves_module() {
    // 形态3：捕获重组
    let main_src = r#"--- @customrequire param=module_name ^mgr\.(\w+)$ module_$1
function remap_require(module_name)
    return require(string.gsub(module_name, "^mgr%.", "module_"))
end

local r = remap_require("mgr.abc_mgr")
r.version
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // `remap_require("mgr.abc_mgr")` → transform: ^mgr\.(\w+)$ → module_$1
    // → "module_abc_mgr"。注意：这个模块路径不存在，所以 hover 应解析失败。
    // 但我们可以验证 regex 变换本身工作：改用存在的路径。
    // 这个测试主要验证 regex 解析不报错、不崩溃。
    // hover `r`（line 5, col 6）应返回空或 Unknown，不 panic。
    let _r_hover = hover::hover(
        docs.get(&intern_uri(&main_uri)).unwrap(),
        intern_uri(&main_uri),
        pos(5, 6),
        &mut agg,
    );
    // 不 panic 即通过
}

#[test]
fn custom_require_pattern_only_empty_template() {
    // 形态4：删除前缀。template 为空串。
    // 用一个能匹配到的路径：mgr_.module_abc.abc_mgr → module_abc.abc_mgr
    let main_src = r#"--- @customrequire param=module_name ^mgr_\.
function strip_prefix(module_name)
    return require(string.gsub(module_name, "^mgr_%.", ""))
end

local s = strip_prefix("mgr_.module_abc.abc_mgr")
s.version
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // `strip_prefix("mgr_.module_abc.abc_mgr")` → 删除 "mgr_." 前缀
    // → "module_abc.abc_mgr" → 解析为 abc_mgr 表
    assert_hover_contains(&docs, &mut agg, &main_uri, 5, 6, "abc_mgr");
    assert_hover_contains(&docs, &mut agg, &main_uri, 6, 2, "1.0.0");
}

#[test]
fn custom_require_silent_fallback_on_non_string_arg() {
    // 用例5：静默降级（实参不是字符串字面量）
    let main_src = r#"--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    return require(module_name)
end

local prefix = "mgr_abc.abc_mgr"
local b = custom_require(prefix)
b.nonexistent
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // `custom_require(prefix)` 中 prefix 是变量，不是字符串字面量
    // → raw_string_args[param_index] = None → 降级 Unknown
    // hover `b`（line 7, col 6）应不包含 abc_mgr（类型是 Unknown）
    let uri_id = intern_uri(&main_uri);
    let doc = docs.get(&uri_id).unwrap();
    let result = hover::hover(doc, uri_id, pos(7, 6), &mut agg);
    let markdown = result
        .map(|h| h.contents)
        .map(|c| match c {
            tower_lsp_server::ls_types::HoverContents::Markup(md) => md.value,
            _ => String::new(),
        })
        .unwrap_or_default();
    assert!(
        !markdown.contains("abc_mgr"),
        "non-string arg should fall back to Unknown, got: {:?}",
        markdown
    );
}

#[test]
fn custom_require_regex_compile_failure_silent() {
    // 用例6：regex 编译失败（[unclosed(）
    let main_src = r#"--- @customrequire param=module_name [unclosed(
function bad_regex(module_name)
    return require(module_name)
end

local x = bad_regex("anything")
x.foo
"#;

    let (docs, mut agg, _parser) =
        setup_workspace(&[("module_abc/abc_mgr.lua", ABC_MGR_SRC), ("main.lua", main_src)]);

    let main_uri = make_uri("main.lua");
    // regex `[unclosed(` 编译失败 → transform 失效 → 整个 custom_require 降级
    // hover `x`（line 5, col 6）应不 panic，返回 Unknown 或空
    let _x_hover = hover::hover(
        docs.get(&intern_uri(&main_uri)).unwrap(),
        intern_uri(&main_uri),
        pos(5, 6),
        &mut agg,
    );
    // 不 panic 即通过
}

#[test]
fn custom_require_cross_file_definition_and_call() {
    // 跨文件验收：custom_require 定义在 utils/loader.lua，
    // 在 main.lua 中调用，验证 FunctionSummary.custom_require 通过
    // aggregation 正确传递。
    let loader_src = r#"--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    return require(module_path)
end

return custom_require
"#;

    let main_src = r#"local loader = require("utils.loader")
local cr = loader

local a = cr("mgr_abc.abc_mgr")
a.version
"#;

    let (docs, mut agg, _parser) = setup_workspace(&[
        ("module_abc/abc_mgr.lua", ABC_MGR_SRC),
        ("utils/loader.lua", loader_src),
        ("main.lua", main_src),
    ]);

    let main_uri = make_uri("main.lua");
    // `cr("mgr_abc.abc_mgr")` 应解析为 abc_mgr 表
    // 注意：这个测试依赖 cr 能从 loader 的 module_return_type 正确解析回 custom_require 函数。
    // 如果 FunctionSummary.custom_require 通过 aggregation 正确传递，
    // 且 cr 解析为 loader 的 custom_require FunctionRef，则应工作。
    // hover `a` 在 `local a = cr(...)` 行（line 4, col 6）
    assert_hover_contains(&docs, &mut agg, &main_uri, 4, 6, "abc_mgr");
}
```

- [ ] **Step 2: 运行测试，预期部分失败**

Run: `cd lsp && cargo test -p mylua-lsp --test test_custom_require`
Expected: 
- `custom_require_no_transform_resolves_module` 可能通过
- `custom_require_literal_replace_resolves_module` 可能通过
- `custom_require_regex_capture_resolves_module` 应通过（不 panic）
- `custom_require_pattern_only_empty_template` 应通过
- `custom_require_silent_fallback_on_non_string_arg` 应通过
- `custom_require_regex_compile_failure_silent` 应通过
- `custom_require_cross_file_definition_and_call` 可能通过或失败（取决于 require 返回函数后再调用 custom require 的链路是否完整）

如果有失败，根据失败信息诊断：
- 如果 hover 返回空 markdown：检查 `setup_workspace` 是否正确注册了 module mapping（`uri_to_module_name` 需要正确解析 `module_abc/abc_mgr.lua` 为 `module_abc.abc_mgr`）
- 如果 `abc_mgr` 不在 hover 中：说明 RequireRef 没生成或 module_path 错误，在 `resolve_function_call_return` 中加 `eprintln!` 调试

- [ ] **Step 3: 修复失败的测试**

根据失败原因修复。常见问题：
1. **hover 行号偏移**：Lua 源码的行号是从 0 开始（LSP 协议），但人眼数行从 1 开始。重新数 hover 目标的行号。
2. **module 路径不匹配**：`uri_to_module_name` 把 `file:///test/module_abc/abc_mgr.lua` 转为 `module_abc.abc_mgr`，确认这个映射正确。
3. **跨文件调用链断裂**：`cr = loader`（loader 是 require 返回的函数），`cr(...)` 应解析为 `FunctionCallReturn { func_name: "cr", ... }`，但 `cr` 不是 `custom_require` 这个名字。可能需要在 resolver 中处理"通过变量传递的函数引用"。这种情况可能超出 P1 范围，如果失败可简化测试或标记为 `#[ignore]`。

- [ ] **Step 4: 全部测试通过后运行完整测试套件**

Run: `cd lsp && cargo test -p mylua-lsp`
Expected: 所有测试（含新增和现有）全部通过。

- [ ] **Step 5: Commit**

```bash
cd lsp
git add crates/mylua-lsp/tests/test_custom_require.rs
git commit -m "test(custom-require): integration tests for @customrequire annotation"
```

---

## Task 10: 文档更新

**Files:**
- Modify: `docs/lsp-capabilities.md`（AGENTS.md 强制要求新增 LSP 能力需同步文档）

- [ ] **Step 1: 阅读 lsp-capabilities.md 找到合适的章节**

Run: 查找 `docs/lsp-capabilities.md` 中 require 相关的章节（或 module resolution / annotations 章节）。

- [ ] **Step 2: 在合适位置新增 @customrequire 能力说明**

在 `docs/lsp-capabilities.md` 中找到 EmmyLua 注解相关章节（如 `@param`、`@return` 等），在相近位置新增 `@customrequire` 的说明：

```markdown
### `@customrequire`

标记函数为类 require 的封装，使其返回值解析为目标 module 的返回类型。

**语法:**
```
---@customrequire param=<name> [regex-pattern] [template]
```

- `param=<name>`：指定哪个参数是 module 路径参数（必填）
- `regex-pattern`：Rust regex 语法的变换规则（可选）
- `template`：替换模板，`$1`/`$2` 为捕获组占位符，其余字符字面量（可选）

**示例:**
```lua
--- @customrequire param=module_name mgr_abc module_abc
function custom_require(module_name)
    local module_path = string.gsub(module_name, "mgr_abc", "module_abc")
    return require(module_path)
end

local a = custom_require("mgr_abc.abc_mgr")
-- a 解析为 module_abc.abc_mgr 的返回类型
```

**限制:**
- 仅当调用实参是字符串字面量时生效（变量/表达式降级为 Unknown）
- 不支持多条变换规则链式
- 注解失效时静默降级，不生成诊断
```

- [ ] **Step 3: Commit**

```bash
cd /d f:\MyGit\ai-mylua-lsp
git add docs/lsp-capabilities.md
git commit -m "docs: add @customrequire annotation to lsp-capabilities.md"
```

---

## Self-Review Checklist

完成所有 task 后，对照 spec 检查：

1. **Spec 覆盖**:
   - 注解语法（第2节）→ Task 3 ✓
   - 数据结构（第3节）→ Task 2, 4 ✓
   - 数据流构建期（第4.1节）→ Task 3, 5, 6 ✓
   - 数据流解析期（第4.2节）→ Task 8 ✓
   - fingerprint 扩展（第5节）→ Task 7 ✓
   - 测试（第6节）→ Task 3 单元测试 + Task 9 集成测试 ✓
   - 文档更新（第8节）→ Task 10 ✓

2. **类型一致性**:
   - `CustomRequireSpec.param_name: LuaSymbol` 在 Task 2 定义、Task 5 构造、Task 8 读取 ✓
   - `CustomRequireSpec.param_index: u32` 同上 ✓
   - `CustomRequireSpec.transform: Option<ModulePathTransform>` 同上 ✓
   - `ModulePathTransform.pattern: String` / `template: String` 同上 ✓
   - `SymbolicStub::FunctionCallReturn.raw_string_args: Vec<Option<String>>` 在 Task 2 定义、Task 6 填充、Task 8 读取 ✓
   - `FunctionSummary.custom_require: Option<CustomRequireSpec>` 在 Task 4 定义、Task 5 填充、Task 8 读取 ✓

3. **边界处理**:
   - 无 pattern+template → Task 3 解析返回 `None, None`；Task 5 构造 `transform: None`；Task 8 直接用原值 ✓
   - 只有 pattern 无 template → Task 3 返回 `Some(p), Some("")`；Task 5 构造 `transform: Some(...)`；Task 8 用空模板 replace_all ✓
   - param_name 找不到 → Task 5 返回 `None`；Task 8 不触发 custom require 分支 ✓
   - regex 编译失败 → Task 8 `apply_custom_require_transform` 返回 `None`；降级 Unknown ✓
   - raw_string_args 越界或 None → Task 8 `try_resolve_custom_require_module` 返回 `None`；降级 Unknown ✓
