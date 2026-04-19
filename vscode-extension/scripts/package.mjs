#!/usr/bin/env node
/**
 * package.mjs — orchestrate the full .vsix build.
 *
 * Steps:
 *   1. `tsc -p ./` — compile TypeScript to `out/`
 *   2. `node scripts/prepackage.mjs` — copy the LSP release binary
 *      into `server/` (host binary by default, or cross-compiled
 *      target when `MYLUA_TARGET` is set).
 *   3. `vsce package [--target <MYLUA_TARGET>]` — build the `.vsix`.
 *
 * When `MYLUA_TARGET` is set, this script passes `--target` to
 * `vsce` so the resulting `.vsix` is marked platform-specific and
 * the Marketplace auto-delivers it only to matching clients. When
 * unset, `vsce` produces a universal `.vsix` that will run on any
 * platform whose native binary happens to match the one we baked
 * in — useful for side-loading during single-user development.
 *
 * Invoked from `npm run package`. Runs on Windows / macOS / Linux
 * via Node's cross-platform `spawnSync` (no `bash`-specific syntax).
 */

import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const extensionRoot = resolve(__dirname, '..');

const target = (process.env.MYLUA_TARGET || '').trim();

/// Shell-quote an argument. Necessary because we pass a single
/// command string to `spawnSync` with `shell: true` — that is the
/// cross-platform way to launch `npx.cmd` on Windows without
/// ENOENT, and it avoids Node's DEP0190 "passing args + shell: true
/// is unsafe" deprecation. MYLUA_TARGET values are keyed against
/// `TARGET_MAP` in prepackage.mjs, so we never see shell
/// metacharacters in practice; quoting below is belt-and-suspenders.
function shellQuote(arg) {
  if (process.platform === 'win32') {
    // cmd.exe: wrap in double quotes and escape any embedded
    // double quote as "". Plenty for simple arg strings.
    return `"${arg.replace(/"/g, '""')}"`;
  }
  // POSIX sh: single-quote and escape embedded single quotes.
  return `'${arg.replace(/'/g, `'\\''`)}'`;
}

function run(cmd, args, opts = {}) {
  const line = [cmd, ...args.map(shellQuote)].join(' ');
  const result = spawnSync(line, {
    cwd: extensionRoot,
    stdio: 'inherit',
    shell: true,
    ...opts,
  });
  if (result.status !== 0) {
    console.error(`[package] ${line} exited with code ${result.status}`);
    process.exit(result.status ?? 1);
  }
}

console.log('[package] 1/3  compile TypeScript');
run('npm', ['run', 'compile']);

console.log('[package] 2/3  copy LSP server binary');
run('node', ['scripts/prepackage.mjs']);

console.log('[package] 3/3  vsce package');
const vsceArgs = ['vsce', 'package'];
if (target.length > 0) {
  vsceArgs.push('--target', target);
  console.log(`[package]    target=${target}`);
}
run('npx', vsceArgs);

console.log('[package] done.');
