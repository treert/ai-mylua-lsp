#!/usr/bin/env node
/**
 * publish.mjs — build the current host's .vsix, then push it to
 * the VS Code Marketplace.
 *
 * Prerequisites:
 *   1. A Marketplace publisher (matches the `publisher` field in
 *      `vscode-extension/package.json`). Create one at
 *      https://marketplace.visualstudio.com/manage.
 *   2. A Personal Access Token from https://dev.azure.com with
 *      `Marketplace: Manage` scope. Provide it via either:
 *        - environment variable:  export VSCE_PAT=<token>
 *        - prior interactive login: npx @vscode/vsce login <publisher>
 *
 * Usage:
 *   cd vscode-extension
 *   export VSCE_PAT=<token>          # or skip if you've `vsce login`ed
 *   npm run release                  # or: node scripts/publish.mjs
 *
 * This pushes **only the current host's** .vsix. Marketplace
 * accepts multiple platform-specific uploads under the same
 * extension ID — run this script on each OS you want to support,
 * or use `.github/workflows/release.yml` for the full matrix.
 */

import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

import {
  detectHostTarget,
  ensureRustTarget,
  buildLspRelease,
  packageVsix,
  extensionRoot,
  run,
} from './lib/host-target.mjs';

const pkg = JSON.parse(readFileSync(join(extensionRoot, 'package.json'), 'utf8'));
const { target, triple } = detectHostTarget();

// `vsce` itself will fall back to an interactive prompt if no
// credentials are available, but that defeats the "manual script
// I can run once" UX. Warn early so the user knows to export
// VSCE_PAT — we don't hard-fail because `vsce login` caches
// credentials elsewhere and works without the env var.
if (!process.env.VSCE_PAT) {
  console.warn(
    '[publish] warning: VSCE_PAT is not set. vsce will fall back to ' +
      'cached credentials from `vsce login`. If publishing fails with ' +
      '"Authentication failed", set VSCE_PAT and retry.',
  );
}

console.log('--------------------------------------------------');
console.log(`[publish] host      : ${process.platform}-${process.arch}`);
console.log(`[publish] target    : ${target}`);
console.log(`[publish] publisher : ${pkg.publisher}`);
console.log(`[publish] name      : ${pkg.name}`);
console.log(`[publish] version   : ${pkg.version}`);
console.log('--------------------------------------------------');

ensureRustTarget(triple);
buildLspRelease(triple);
packageVsix(target);

const vsixName = `${pkg.name}-${target}-${pkg.version}.vsix`;
const vsixPath = join(extensionRoot, vsixName);
if (!existsSync(vsixPath)) {
  console.error(`[publish] expected .vsix not found at ${vsixPath}`);
  console.error(`[publish] did \`npm run package\` succeed with MYLUA_TARGET=${target}?`);
  process.exit(1);
}

// `vsce publish --packagePath` auto-derives the --target from the
// platform tag embedded in the .vsix by our earlier
// `vsce package --target ...` call, so we don't need to pass it
// again here.
run(`npx @vscode/vsce publish --packagePath "${vsixPath}"`, { cwd: extensionRoot });

console.log('');
console.log(`[publish] done!`);
console.log(`[publish] Manage: https://marketplace.visualstudio.com/manage/publishers/${pkg.publisher}`);
