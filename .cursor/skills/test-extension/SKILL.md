---
name: test-extension
description: Build and launch the MyLua VS Code extension in Extension Development Host with tests/lua-root as workspace. Use when user mentions testing the extension, launching extension, trying extension changes, debugging LSP behavior, or restarting the test environment.
---

# Test Extension

Build LSP + extension and launch an Extension Development Host window opening `tests/lua-root/`.
If an EDH window is already running, it is killed first (restart).

## Quick Start

**macOS / Linux (Bash):**

```bash
.cursor/scripts/test-extension.sh
```

**Windows (PowerShell):**

```powershell
.cursor/scripts/test-extension.ps1
```

Both scripts perform **all 4 steps** automatically:
1. `cd lsp && cargo build`
2. `cd vscode-extension && npm run compile`
3. Kill any existing Extension Development Host for this extension
4. Launch `code --extensionDevelopmentPath=... tests/lua-root/`

## Options

**Bash:**

| Flag | Effect |
|------|--------|
| `--skip-build` | Skip both LSP and extension compilation |
| `--skip-lsp` | Skip only `cargo build` (useful when only TS changed) |
| `--skip-ext` | Skip only `npm run compile` (useful when only Rust changed) |

**PowerShell:**

| Flag | Effect |
|------|--------|
| `-SkipBuild` | Skip both LSP and extension compilation |
| `-SkipLsp` | Skip only `cargo build` (useful when only TS changed) |
| `-SkipExt` | Skip only `npm run compile` (useful when only Rust changed) |

## When to Use Each Flag

- **Full rebuild (default)**: After pulling new changes or first run
- `--skip-lsp` / `-SkipLsp`: Only changed TextMate grammar or `extension.ts`
- `--skip-ext` / `-SkipExt`: Only changed Rust LSP code
- `--skip-build` / `-SkipBuild`: Just want to restart the test window without rebuilding

## Workflow for the Agent

When the user asks to test/launch/restart the extension:

1. Detect OS: use `.sh` on macOS/Linux, `.ps1` on Windows
2. Run the script with appropriate flags
3. If the build fails, fix the error and retry
4. Confirm to the user that the EDH window is launched

### Platform Detection

```
macOS/Linux → bash .cursor/scripts/test-extension.sh
Windows     → powershell -File .cursor/scripts/test-extension.ps1
```

### Restart Scenario

The scripts are idempotent — running again kills any previous EDH and launches fresh.

### Build Failure Recovery

If `cargo build` fails — fix Rust code, then re-run the full script.

If `npm run compile` fails — fix TS code, then re-run with `--skip-lsp` / `-SkipLsp`.

## Notes

- Both scripts auto-detect `code` vs `cursor` CLI (prefers `code`)
- `tests/lua-root/` contains Lua files for testing: require, EmmyLua class, member functions, json4lua
- LSP binary is found by the extension at `lsp/target/debug/mylua-lsp` in dev mode
- On Windows, running `cargo test` while EDH is active may lock the exe; use `$env:CARGO_TARGET_DIR="target-test"; cargo test --tests` to avoid
