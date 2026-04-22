# VS Code Extension 详细说明

**本文档记录 `mylua-lsp` VS Code 扩展的文件结构、构建打包流程和运行时行为。**
概览见 [`ai-readme.md`](../ai-readme.md)。

## 文件结构

| 文件 | 说明 |
|------|------|
| [`package.json`](../vscode-extension/package.json) | 扩展清单：语言注册、TextMate grammar、配置项；`scripts.package` 串 `compile → prepackage → vsce package` |
| [`syntaxes/lua.tmLanguage.json`](../vscode-extension/syntaxes/lua.tmLanguage.json) | TextMate grammar：Lua 基础语法（关键字、字符串、数字、注释）+ 完整 EmmyLua 注解着色（16 种 `@tag` 结构化匹配、`fun()`/`{}`/`()` 嵌套类型表达式、内置类型 vs 用户类型区分、点号类型名） |
| [`assets/lua5.4/`](../vscode-extension/assets/lua5.4/) | 打包随扩展的 Lua 5.4 stdlib EmmyLua 注解 stubs（11 个文件）；`<extensionPath>/assets/lua5.4/` 在 dev 与 `.vsix` 布局**同构**，扩展 `resolveBundledLibrary` 单路径查找 |
| [`.vscodeignore`](../vscode-extension/.vscodeignore) | 打包排除：`src/`、`scripts/`、`tsconfig*.json`、`node_modules/`、`.gitignore` 等；默认包含 `out/`、`server/`、`assets/`、`syntaxes/` 等运行时产物 |

## 扩展入口（`extension.ts`）

### Server Path 解析优先级

1. **`mylua.server.path` 用户配置**：支持 string 形式对所有平台生效 / object 形式 `{darwin,linux,win32}` 按 `process.platform` 取
2. **Development 模式**（F5 Extension Development Host）→ 直接用 `<extensionPath>/../lsp/target/debug/<bin>`，**绕过** `server/` 目录避免被 `npm run prepackage` 的陈旧拷贝掩盖
3. **Production** → `<extensionPath>/server/<bin>`，`server/` 缺失时兜底也回到 dev target

二进制名按平台计算（`win32 → mylua-lsp.exe`，其他 `mylua-lsp`）。

### 配置收集（`collectLspConfig`）

读所有 `mylua.*` 配置并合成 library 列表：
- `useBundledStdlib=true` 时把 `<extensionPath>/assets/lua<runtime.version>/` 预置到 `workspace.library` 最前
- 用户自定义路径 append 其后（保序交给服务器做 scan root 合并）
- 无 bundled 目录时（例如 `runtime.version=5.1`）`resolveBundledLibrary` 沿 `BUNDLED_LIBRARY_FALLBACKS = ['5.4']` 链回落到最新可用版本，找不到时静默返回 undefined 不产生错误

### StatusBar 与索引通知

创建 `MyLua` StatusBarItem 并订阅自定义通知 `mylua/indexStatus`：
- **索引进行中**显示 `💛X/Y`（`X` 已索引、`Y` 总文件数；`total=0` 时回退为 `💛mylua`）
- **索引完成**显示 `💚mylua`
- 激活时先以 `💛mylua` 展示再等首个通知到达
- `state=ready` 且携带 `elapsedMs` 时用 `withProgress({location: Notification})` 渲染 4 秒自动消失的 toast "MyLua 索引完成，耗时 X.X 秒（N 个文件）"（session 内幂等：`readyNotified` 模块变量防止重复弹出）
- **点击 StatusBar** → 直接打开 Settings 且 `@ext:onemore.mylua-lsp` 过滤到本扩展所有 `mylua.*` 配置项

> 注意：publisher=`onemore`、extension name=`mylua-lsp`；**配置键前缀仍是 `mylua.*`**，只有扩展标识名改成了 `mylua-lsp`。

## 构建与打包脚本

### `scripts/prepackage.mjs`

- **host 模式**（无 `MYLUA_TARGET`）：拷 `lsp/target/release/mylua-lsp(.exe)` → `<extension>/server/`
- **target 模式**（设置 `MYLUA_TARGET`）：按内置 `TARGET_MAP` 把 VS Code target 字符串（`darwin-arm64` / `win32-x64` / `linux-x64` / `linux-arm64` 等 9 个）映射到 Rust triple，拷 `lsp/target/<triple>/release/<bin>`
- 每次拷贝前 `rmSync(server/)` 清空避免跨平台二进制遗留；Unix 下 `chmod 0o755`；源文件缺失时 exit 1 附带构建提示

### `scripts/package.mjs`

`npm run package` orchestrator：依次跑 `tsc -p ./` → `node scripts/prepackage.mjs` → `npx @vscode/vsce package [--target $MYLUA_TARGET]`。`shellQuote` 单命令行拼接避免 Node `DEP0190` 警告；`shell: true` 保证 Windows 下 `npx.cmd` 解析。

### `scripts/lib/host-target.mjs`

共享 helper：`detectHostTarget()` 按 `(process.platform, process.arch)` 在 6-key `HOST_TARGET_MAP` 里查出 VS Code target + Rust triple；`ensureRustTarget` / `buildLspRelease` / `packageVsix` 是带 `cwd` 与 env 注入的 `spawnSync` 包装；不支持的 host 显式退出并列出可接受组合。

### `scripts/build-local.mjs`

`npm run build:local` 入口：检测 host → `rustup target add` → `cargo build --release --target <triple>` → `MYLUA_TARGET=<target> npm run package` → 打印 `code --install-extension` 提示。**一键给自己或内部团队打 .vsix** 的首选脚本。

### `scripts/publish.mjs`

`npm run release` 入口：全量复用 build-local 的流程，额外跑 `npx @vscode/vsce publish --packagePath <vsix>`。`VSCE_PAT` 未设置时打 warning 但不退出（允许 `vsce login` 缓存凭证）；只推送当前 host 对应的 .vsix，跨 OS 推送请上 GitHub Actions 矩阵。

## CI/CD

### `.github/workflows/release.yml`

矩阵打包 workflow：4 个 target（`darwin-arm64` / `win32-x64` / `linux-x64` / `linux-arm64`）分别在对应 runner（`macos-latest` / `windows-latest` / `ubuntu-latest` / `ubuntu-24.04-arm`）上并行 `cargo build --release --target <triple>` → `npm run package`；`Swatinem/rust-cache` 缓存 cargo；`actions/upload-artifact` 产出 `vsix-<target>`；`v*` tag 推送时 `softprops/action-gh-release` 自动附加到 GitHub Release；底部注释掉的 `vsce publish` 步骤在配置 `VSCE_PAT` secret 后启用 Marketplace 发布。

## 常用命令

- 构建：`cd vscode-extension && npm install && npm run compile`
- 调试：F5 启动 Extension Development Host
- 打包：`cd vscode-extension && npm run build:local`
- 发布：`cd vscode-extension && npm run release`
