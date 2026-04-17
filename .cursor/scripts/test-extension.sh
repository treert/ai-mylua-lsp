#!/usr/bin/env bash
#
# Launch (or restart) the MyLua Extension Development Host
# opening tests/lua-root as the workspace.
#
# Usage:
#   .cursor/scripts/test-extension.sh [--skip-build] [--skip-lsp] [--skip-ext] [-w] [-w 0] [-w 1]
#
set -euo pipefail

# Ensure cargo/rustup and common tools are on PATH when launched outside
# an interactive shell (e.g. from Cursor agent or a bare login shell).
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXT_DIR="$REPO_ROOT/vscode-extension"
LUA_ROOT="$REPO_ROOT/tests/lua-root"
WORKSPACE_FILE="$REPO_ROOT/tests/mylua-tests.code-workspace"
EDH_MARKER="extensionDevelopmentPath=$EXT_DIR"

SKIP_BUILD=false
SKIP_LSP=false
SKIP_EXT=false
OPEN_WORKSPACE=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --skip-build) SKIP_BUILD=true; shift ;;
    --skip-lsp)   SKIP_LSP=true; shift ;;
    --skip-ext)   SKIP_EXT=true; shift ;;
    -w)
      OPEN_WORKSPACE=true
      if [[ $# -gt 1 && "$2" =~ ^[01]$ ]]; then
        if [[ "$2" == "0" ]]; then
          OPEN_WORKSPACE=false
        fi
        shift 2
      else
        shift
      fi
      ;;
    -h|--help)
      echo "Usage: $0 [--skip-build] [--skip-lsp] [--skip-ext] [-w] [-w 0] [-w 1]"
      echo "  --skip-build  Skip both LSP and extension build"
      echo "  --skip-lsp    Skip LSP cargo build only"
      echo "  --skip-ext    Skip extension npm compile only"
      echo "  -w            Open tests/mylua-tests.code-workspace"
      echo "  -w 0          Open tests/lua-root"
      echo "  -w 1          Open tests/mylua-tests.code-workspace"
      exit 0
      ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

if [[ "$OPEN_WORKSPACE" == true ]]; then
  LAUNCH_TARGET="$WORKSPACE_FILE"
  LAUNCH_TARGET_LABEL="workspace"
else
  LAUNCH_TARGET="$LUA_ROOT"
  LAUNCH_TARGET_LABEL="lua-root"
fi

if [[ ! -e "$LAUNCH_TARGET" ]]; then
  echo "ERROR: Launch target not found: $LAUNCH_TARGET"
  exit 1
fi

# Auto-detect editor CLI: prefer code, fall back to cursor
if command -v code &>/dev/null; then
  EDITOR_CLI="code"
elif command -v cursor &>/dev/null; then
  EDITOR_CLI="cursor"
else
  echo "ERROR: Neither 'cursor' nor 'code' CLI found in PATH."
  exit 1
fi

# ── Step 1: Build LSP server ──────────────────────────────────────────
if [[ "$SKIP_BUILD" == false && "$SKIP_LSP" == false ]]; then
  echo "==> [1/4] Building LSP server (cargo build)..."
  (cd "$REPO_ROOT/lsp" && cargo build)
else
  echo "==> [1/4] Skipping LSP build"
fi

# ── Step 2: Compile extension ─────────────────────────────────────────
if [[ "$SKIP_BUILD" == false && "$SKIP_EXT" == false ]]; then
  echo "==> [2/4] Compiling VS Code extension (npm run compile)..."
  (cd "$EXT_DIR" && npm run compile)
else
  echo "==> [2/4] Skipping extension compile"
fi

# ── Step 3: Kill existing Extension Development Host ──────────────────
echo "==> [3/4] Checking for existing Extension Development Host..."
PIDS=$(pgrep -f "$EDH_MARKER" 2>/dev/null || true)
if [[ -n "$PIDS" ]]; then
  echo "    Found running EDH (PIDs: $(echo $PIDS | tr '\n' ' ')). Terminating..."
  echo "$PIDS" | xargs kill 2>/dev/null || true
  sleep 2
  # Force kill stragglers
  REMAINING=$(pgrep -f "$EDH_MARKER" 2>/dev/null || true)
  if [[ -n "$REMAINING" ]]; then
    echo "    Force killing remaining processes..."
    echo "$REMAINING" | xargs kill -9 2>/dev/null || true
    sleep 1
  fi
  echo "    Previous instance terminated."
else
  echo "    No existing instance found."
fi

# ── Step 4: Launch Extension Development Host ─────────────────────────
echo "==> [4/4] Launching Extension Development Host ($EDITOR_CLI)..."
echo "    Extension: $EXT_DIR"
echo "    Target ($LAUNCH_TARGET_LABEL): $LAUNCH_TARGET"
"$EDITOR_CLI" --extensionDevelopmentPath="$EXT_DIR" "$LAUNCH_TARGET" &

echo ""
echo "==> Done! Extension Development Host launched with $LAUNCH_TARGET_LABEL."
echo "    Run again to restart."
