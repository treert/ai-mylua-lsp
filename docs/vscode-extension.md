# VS Code Extension

`mylua-lsp` 的 VS Code 扩展，提供 LSP 客户端 + TextMate 语法着色 + 索引状态展示。

## 文件结构

| 文件/目录 | 说明 |
|-----------|------|
| `package.json` | 扩展清单：语言注册、TextMate grammar、配置项（`mylua.*`） |
| `src/extension.ts` | 扩展入口：LSP 客户端启动、配置收集、StatusBar |
| `syntaxes/lua.tmLanguage.json` | TextMate grammar：Lua 基础语法 + EmmyLua 注解着色 |
| `assets/lua5.4/` | 内置 Lua 5.4 stdlib EmmyLua stubs（11 个文件） |
| `scripts/` | 构建打包脚本 |

## 运行时行为

### Server Path 解析优先级

1. **用户配置** `mylua.server.path`：支持 string（全平台）或 object `{darwin,linux,win32}` 按平台取
2. **Development 模式**（F5）→ `<extensionPath>/../lsp/target/debug/<bin>`
3. **Production** → `<extensionPath>/server/<bin>`

### 配置收集

- `useBundledStdlib=true` 时将内置 stdlib 路径预置到 `workspace.library`
- 用户自定义 library 路径追加其后
- stdlib 按 `runtime.version` 查找，找不到时沿 fallback 链回落到 5.4

### StatusBar

- 索引中：`💛X/Y`（已索引/总数）
- 索引完成：`💚mylua` + toast 通知（session 内仅一次）
- 点击 → 打开扩展配置页

## 构建打包

### 脚本说明

| 脚本 | 用途 |
|------|------|
| `scripts/prepackage.mjs` | 拷贝 LSP 二进制到 `server/` 目录 |
| `scripts/package.mjs` | 编排：tsc → prepackage → vsce package |
| `scripts/build-local.mjs` | **一键本地打包**：检测平台 → cargo build → 打 .vsix |
| `scripts/publish.mjs` | 本地构建 + vsce publish |
| `scripts/lib/host-target.mjs` | 共享 helper：平台检测、Rust triple 映射 |

### 常用命令

```bash
# 开发
cd vscode-extension && npm install && npm run compile

# 调试
# F5 启动 Extension Development Host

# 本地打包（一键）
cd vscode-extension && npm run build:local

# 发布
cd vscode-extension && npm run release
```

### 跨平台支持

`prepackage.mjs` 支持 `MYLUA_TARGET` 环境变量指定目标平台，覆盖 9 个 VS Code target（`darwin-arm64`、`win32-x64`、`linux-x64` 等）到 Rust triple 的映射。

## CI/CD

`.github/workflows/release.yml`：4 target 矩阵并行构建，`v*` tag 推送时自动附加到 GitHub Release。

> publisher=`onemore`，extension name=`mylua-lsp`，配置键前缀 `mylua.*`
