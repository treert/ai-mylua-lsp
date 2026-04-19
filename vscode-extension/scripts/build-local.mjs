#!/usr/bin/env node
/**
 * build-local.mjs — one-shot "build the .vsix for my current
 * machine" script.
 *
 * Auto-detects (process.platform, process.arch) → VS Code
 * platform target + Rust triple, installs the Rust target if
 * missing, compiles the LSP in release mode, then hands off to
 * the regular packaging pipeline with `MYLUA_TARGET` set so the
 * resulting `.vsix` is tagged as platform-specific (Marketplace
 * expects this for native-binary extensions).
 *
 * Typical usage:
 *   cd vscode-extension
 *   npm run build:local        # or: node scripts/build-local.mjs
 *
 * Output:
 *   vscode-extension/mylua-<target>-<version>.vsix
 *
 * This only produces a .vsix for the **current** host. To cover
 * another OS you need a machine/CI runner of that OS — see
 * `.github/workflows/release.yml` for the full multi-platform
 * matrix.
 */

import { readFileSync } from 'node:fs';
import { join } from 'node:path';

import {
  detectHostTarget,
  ensureRustTarget,
  buildLspRelease,
  packageVsix,
  extensionRoot,
} from './lib/host-target.mjs';

const pkg = JSON.parse(readFileSync(join(extensionRoot, 'package.json'), 'utf8'));
const { target, triple } = detectHostTarget();

console.log('--------------------------------------------------');
console.log(`[build-local] host     : ${process.platform}-${process.arch}`);
console.log(`[build-local] target   : ${target}`);
console.log(`[build-local] triple   : ${triple}`);
console.log(`[build-local] version  : ${pkg.version}`);
console.log('--------------------------------------------------');

ensureRustTarget(triple);
buildLspRelease(triple);
packageVsix(target);

const expected = `${pkg.name}-${target}-${pkg.version}.vsix`;
console.log('');
console.log(`[build-local] done!`);
console.log(`[build-local] output: ${join(extensionRoot, expected)}`);
console.log('');
console.log(`To install locally:`);
console.log(`  code --install-extension "${join(extensionRoot, expected)}"`);
