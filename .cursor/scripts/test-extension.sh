#!/usr/bin/env bash
#
# Launch (or restart) the MyLua Extension Development Host
# opening tests/lua-root as the workspace.
#
# Usage:
#   .cursor/scripts/test-extension.sh [--skip-build] [--skip-lsp] [--skip-ext]
#
set -euo pipefail

# Ensure cargo/rustup and common tools are on PATH when launched outside
# an interactive shell (e.g. from Cursor agent or a bare login shell).
[[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXT_DIR="$REPO_ROOT/vscode-extension"
LUA_ROOT="$REPO_ROOT/tests/lua-root"
EDH_MARKER="extensionDevelopmentPath=$EXT_DIR"

SKIP_BUILD=false
SKIP_LSP=false
SKIP_EXT=false

for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=true ;;
    --skip-lsp)   SKIP_LSP=true ;;
    --skip-ext)   SKIP_EXT=true ;;
    -h|--help)
      echo "Usage: $0 [--skip-build] [--skip-lsp] [--skip-ext]"
      echo "  --skip-build  Skip both LSP and extension build"
      echo "  --skip-lsp    Skip LSP cargo build only"
      echo "  --skip-ext    Skip extension npm compile only"
      exit 0
      ;;
    *) echo "Unknown option: $arg"; exit 1 ;;
  esac
done

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
echo "    Workspace: $LUA_ROOT"
"$EDITOR_CLI" --extensionDevelopmentPath="$EXT_DIR" "$LUA_ROOT" &

echo ""
echo "==> Done! Extension Development Host launched with lua-root."
echo "    Run again to restart."
