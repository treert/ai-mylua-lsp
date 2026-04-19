# vscode-extension

VS Code 扩展：自研 **TextMate** 基色高亮、语言配置、**启动** 同仓库构建的 `mylua-lsp` 二进制。

## 功能

- **TextMate 语法高亮**：Lua 5.3+ 关键字、字符串、数字、注释（含 EmmyLua `---@` 注解标注）
- **语言配置**：括号匹配、自动闭合、折叠、缩进规则
- **LSP 客户端**：通过 stdio 启动 `mylua-lsp`，自动获得诊断 / 大纲 / 跳转 / 悬浮 / 引用 / 全库搜索等能力

## 开发

```bash
cd vscode-extension
npm install
npm run compile

# 需先构建 LSP server:
cd ../lsp && cargo build
```

在 VS Code 中按 F5 启动 Extension Development Host 调试。

## LSP 二进制查找顺序

1. **`mylua.server.path`** —— 用户显式覆盖，最高优先级。两种写法：
   - 字符串：对所有平台生效。
   - 对象：按 `process.platform` 键取值，键可选 `darwin` / `linux` / `win32`。例：
     ```jsonc
     { "darwin": "/abs/mac/mylua-lsp", "win32": "C:/tools/mylua-lsp.exe" }
     ```
2. **Development 模式**（F5 Extension Development Host）—— 直接用 `<extensionPath>/../lsp/target/debug/mylua-lsp(.exe)`，**不走** `server/` 目录。这是为了避免 `npm run prepackage` 的陈旧二进制掩盖刚 `cargo build` 出来的最新产物。
3. **Production 模式** —— `<extensionPath>/server/mylua-lsp(.exe)`（`.vsix` 内置）。如缺失最后兜底再回到 dev 路径。

平台二进制名固定：`win32` 用 `mylua-lsp.exe`，其他用 `mylua-lsp`。

## 打包发布

VS Code Marketplace 支持 [平台特定扩展](https://code.visualstudio.com/api/working-with-extensions/publishing-extension#platformspecific-extensions)：同一 extension ID 可以同时挂多个 `.vsix`，每个带 `target` 标签，Marketplace 自动按用户当前平台分发对应那份。本项目正是走这个模式。

### 两个手动脚本（常用）

**① 本地打包一份自己用**

```bash
cd vscode-extension
npm install          # 首次
npm run build:local
```

自动完成：
1. 检测当前 `process.platform` + `process.arch` → VS Code target（如 Mac ARM → `darwin-arm64`，Intel Mac → `darwin-x64`，Win x64 → `win32-x64`，Linux x64 → `linux-x64`）
2. `rustup target add <triple>` — idempotent，已装就跳过
3. `cargo build --release --target <triple>` — 编 LSP
4. 清理 `server/` → 拷贝新二进制 → `tsc` 编译 TS → `vsce package --target <target>`
5. 输出 `vscode-extension/mylua-<target>-<version>.vsix`，最后打印一条 `code --install-extension <...>` 命令可直接复制安装

**② 发布到 VS Code Marketplace**

```bash
cd vscode-extension
export VSCE_PAT=<your-pat>    # 或者之前 `npx @vscode/vsce login <publisher>` 过也行
npm run release
```

流程 = `build:local` 全部步骤 + 最后 `vsce publish --packagePath <vsix>`。只推送**当前平台**对应的 `.vsix`；想覆盖其他 OS 请在那台机器上跑同一个命令，或用 GitHub Actions 矩阵（下一节）。

> **`VSCE_PAT` 怎么来**：https://dev.azure.com → User settings → Personal access tokens → New → Marketplace: Manage scope → 生成一个并保存到安全地方。

### 支持的目标矩阵

`scripts/lib/host-target.mjs` 登记了 6 个主流 host → target 映射（Mac/Win/Linux × x64/arm64）。如果你在 host-target 映射表里没命中的平台（如 FreeBSD、32 位 ARM），脚本会显式报错退出并列出支持的组合。

更全的 9 个 VS Code target 在 `scripts/prepackage.mjs::TARGET_MAP` 中，手动设 `MYLUA_TARGET=<name> npm run package` 可走那些冷门 target，但需要自己先 `cargo build --release --target <triple>`。

### 跨 OS 打包的现实

| 起点 / 终点 | 可行性 | 备注 |
|---|---|---|
| macOS ↔ macOS（arm64 ↔ x64） | ✅ 简单 | Apple SDK 同机支持两架构 |
| Windows ↔ Windows（x64 ↔ arm64） | ✅ 简单 | MSVC 同机支持 |
| Linux ↔ Linux（x64 ↔ arm64） | ⚠️ 需 `cross` | Docker 驱动 |
| **macOS → Windows** | ❌ 实质不可行 | `x86_64-pc-windows-msvc` 需要 `link.exe`，MSVC 只在 Windows 存在 |
| **macOS → Linux** | ❌ 麻烦 | 需 Docker + `cross` |
| **Windows → macOS** | ❌ 几乎不可能 | Apple SDK 不可合法分发 |

**所以 `build:local` / `release` 本质只服务当前机器**。要一次发布所有平台用 CI。

### 自动化发版（GitHub Actions）

`.github/workflows/release.yml` 提供全平台矩阵 workflow：

- **触发**：`git push origin v<x.y.z>` 或手工 `workflow_dispatch`
- **构建**：5 个 target（`darwin-arm64` / `darwin-x64` / `win32-x64` / `linux-x64` / `linux-arm64`）在对应原生 runner 上并行 `cargo build --release --target <triple>` + `npm run package`
- **产物**：每个 target 一个 `vsix-<target>` artifact
- **Release**：tag 推送时自动创建 GitHub Release 并附上全部 `.vsix`
- **Marketplace**：workflow 底部有注释掉的 `vsce publish` 片段，添加 `VSCE_PAT` repo secret 后取消注释即可启用

发版动作示意：

```bash
(cd vscode-extension && npm version patch)   # 或 minor / major
git push origin main
git push origin v0.1.1
# Actions 跑完后 Releases 页面即有 5 个 .vsix
```

### 脚本总览

| 脚本 | 作用 | 典型调用 |
|---|---|---|
| `scripts/build-local.mjs` | 本机一键打 .vsix，自动检测 target | `npm run build:local` |
| `scripts/publish.mjs` | 本机一键打 + 发 Marketplace | `npm run release` |
| `scripts/package.mjs` | 单次打包 orchestrator（compile + prepackage + vsce package），受 `MYLUA_TARGET` 控制 target 标签；CI 与上面两个脚本都通过它进入底层 | `MYLUA_TARGET=<t> npm run package` |
| `scripts/prepackage.mjs` | 根据 `MYLUA_TARGET` 或 host 默认，把 release 二进制从 `lsp/target/<triple>/release/` 拷到 `vscode-extension/server/` | 内部调用 |
| `scripts/lib/host-target.mjs` | 共享 helper：host → target 映射 + cargo / rustup / npm run package 包装 | 内部调用 |

`server/` 每次打包前会被清空，避免跨平台二进制遗留。`.vsix` 内二进制名为 `server/mylua-lsp`（UNIX）或 `server/mylua-lsp.exe`（Windows），运行时 `extension.ts` 按 `process.platform` 决定用哪个。

## Lua 标准库

`assets/lua5.4/` 是打包内置的 Lua 5.4 stdlib EmmyLua 注解 stubs。启动时扩展会把 `<extensionPath>/assets/lua<runtime.version>/` 自动注入到 `mylua.workspace.library` 列表最前端（由 `mylua.workspace.useBundledStdlib` 开关控制，默认 `true`），让 `print` / `string.format` / `table.concat` / `io.open` 等标准库符号获得 hover、goto、signature help 与补全能力，且不会在 Problems 面板产生诊断。

用户可在 `mylua.workspace.library` 中追加自己的注解包（LÖVE、OpenResty 或企业内部 SDK 的 EmmyLua stubs），路径可是绝对路径、`~/…`，或相对首个 workspace root。

## 约定

- **不做**语言理解；解析与语义均在 `../lsp`。
- TextMate grammar 仅负责基色高亮，语义着色由 LSP semantic tokens 叠加。
