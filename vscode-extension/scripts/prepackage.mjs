#!/usr/bin/env node
/**
 * prepackage.mjs — vsce prepackage hook.
 *
 * Copies the release `mylua-lsp` binary into
 * `vscode-extension/server/` so `vsce package` includes it in the
 * generated `.vsix`. Supports two modes:
 *
 * 1. **Host mode** (no `MYLUA_TARGET` env var): copies from
 *    `lsp/target/release/mylua-lsp(.exe)`, i.e. whatever the local
 *    `cargo build --release` produced. This is the convenient path
 *    for single-developer installs and quick smoke tests — the
 *    resulting `.vsix` runs on the host's OS/arch only.
 *
 * 2. **Target mode** (`MYLUA_TARGET=<vscode-target>`): copies from
 *    `lsp/target/<rust-triple>/release/mylua-lsp(.exe)`, where the
 *    triple is derived from the VS Code platform target via
 *    `TARGET_MAP` below. Used by the CI release matrix to produce
 *    platform-specific `.vsix` files (see
 *    `.github/workflows/release.yml`).
 *
 * The `server/` directory is wiped before each copy so stale
 * artifacts from a previous packaging run on a different platform
 * don't leak into the new `.vsix` (e.g. `mylua-lsp.exe` left
 * behind after a Windows package on a macOS developer machine).
 *
 * Exits non-zero with an actionable message if the expected
 * binary is missing — build it with `cargo build --release
 * [--target <triple>]` in `lsp/` before running this script.
 */

import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  rmSync,
} from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Mapping from VS Code platform target → Rust cargo triple.
 *
 * This is the full set of targets the packaging pipeline is
 * *prepared* to build — `MYLUA_TARGET=<key> npm run package`
 * works locally for any of them as long as the matching Rust
 * toolchain is installed (`rustup target add <triple>`).
 *
 * The CI release matrix in `.github/workflows/release.yml`
 * intentionally covers a subset (curated by market share and
 * runner availability). When you want CI coverage for another
 * target, add a matrix row there — no code change here is
 * required.
 *
 * `exe: true` switches the binary name to `mylua-lsp.exe` (the
 * only Windows-specific bit); everything else follows the same
 * `lsp/target/<triple>/release/<bin>` convention.
 */
const TARGET_MAP = Object.freeze({
  'darwin-x64':   { triple: 'x86_64-apple-darwin',        exe: false },
  'darwin-arm64': { triple: 'aarch64-apple-darwin',       exe: false },
  'linux-x64':    { triple: 'x86_64-unknown-linux-gnu',   exe: false },
  'linux-arm64':  { triple: 'aarch64-unknown-linux-gnu',  exe: false },
  'linux-armhf':  { triple: 'armv7-unknown-linux-gnueabihf', exe: false },
  'alpine-x64':   { triple: 'x86_64-unknown-linux-musl',  exe: false },
  'alpine-arm64': { triple: 'aarch64-unknown-linux-musl', exe: false },
  'win32-x64':    { triple: 'x86_64-pc-windows-msvc',     exe: true  },
  'win32-arm64':  { triple: 'aarch64-pc-windows-msvc',    exe: true  },
});

const __dirname = dirname(fileURLToPath(import.meta.url));
const extensionRoot = resolve(__dirname, '..');
const repoRoot = resolve(extensionRoot, '..');
const lspRoot = join(repoRoot, 'lsp');
const destDir = join(extensionRoot, 'server');

function die(msg) {
  console.error(`[prepackage] ${msg}`);
  process.exit(1);
}

const target = (process.env.MYLUA_TARGET || '').trim();
let sourceBinary;
let destBinary;
let targetDescription;

if (target.length > 0) {
  const mapping = TARGET_MAP[target];
  if (!mapping) {
    die(
      `unknown MYLUA_TARGET=${target}\n` +
        `    supported values: ${Object.keys(TARGET_MAP).join(', ')}`,
    );
  }
  const binaryName = mapping.exe ? 'mylua-lsp.exe' : 'mylua-lsp';
  sourceBinary = join(lspRoot, 'target', mapping.triple, 'release', binaryName);
  destBinary = join(destDir, binaryName);
  targetDescription = `target=${target} (${mapping.triple})`;
} else {
  // Host mode: use whatever `cargo build --release` on this machine produced.
  const isWindows = process.platform === 'win32';
  const binaryName = isWindows ? 'mylua-lsp.exe' : 'mylua-lsp';
  sourceBinary = join(lspRoot, 'target', 'release', binaryName);
  destBinary = join(destDir, binaryName);
  targetDescription = `host (${process.platform}-${process.arch})`;
}

if (!existsSync(sourceBinary)) {
  // Quote the path so the hint is copy-paste safe on systems whose
  // project lives under a path containing spaces (common on Windows
  // `C:\Users\John Doe\...` and macOS `~/My Projects/`). Double
  // quotes work as-is in bash, zsh, fish, PowerShell, and cmd.exe;
  // paths containing a literal double quote are rare enough to not
  // warrant platform-specific branching here.
  const quoted = `"${lspRoot}"`;
  const cargoArgs = target.length > 0
    ? `cargo build --release --target ${TARGET_MAP[target].triple}`
    : 'cargo build --release';
  die(
    `release binary missing for ${targetDescription}\n` +
      `    expected: ${sourceBinary}\n` +
      `    build it first: (cd ${quoted} && ${cargoArgs})`,
  );
}

rmSync(destDir, { recursive: true, force: true });
mkdirSync(destDir, { recursive: true });
copyFileSync(sourceBinary, destBinary);

// On UNIX-like hosts (only relevant when we're copying a UNIX
// binary — cross-target copies on Windows hosts skip this).
if (!destBinary.endsWith('.exe')) {
  try {
    chmodSync(destBinary, 0o755);
  } catch (err) {
    console.warn(`[prepackage] could not chmod ${destBinary}: ${err.message}`);
  }
}

console.log(`[prepackage] ${targetDescription}: ${sourceBinary} -> ${destBinary}`);
