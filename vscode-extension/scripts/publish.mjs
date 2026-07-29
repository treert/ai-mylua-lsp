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

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import {
  detectHostTarget,
  ensureRustTarget,
  buildLspRelease,
  packageVsix,
  extensionRoot,
  repoRoot,
  run,
} from './lib/host-target.mjs';

/// Semi-automatic changelog bump: validate the [Unreleased] section
/// is non-empty, then rename it to `[<version>] - <today>` and
/// re-open a fresh empty [Unreleased] on top. The file edit lands
/// before `vsce package` so the marketplace-published .vsix carries
/// the updated Changelog tab. The git commit + tag happen later,
/// only after `vsce publish` succeeds (skip with --no-git).
///
/// Validation guards (all fail fast BEFORE any build work):
///   - CHANGELOG.md must exist next to package.json.
///   - It must contain a `## [Unreleased]` heading.
///   - That section must have at least one real entry (HTML
///     comments are ignored). An empty Unreleased aborts release
///     unless --force-changelog is passed.
///   - The target `## [<version>] - <date>` heading must not already
///     exist (prevents double-releasing the same version).
function bumpChangelog(version) {
  const changelogPath = join(extensionRoot, 'CHANGELOG.md');
  if (!existsSync(changelogPath)) {
    console.error('[publish] CHANGELOG.md not found at', changelogPath);
    console.error('[publish] create it before releasing.');
    process.exit(1);
  }

  const today = new Date().toISOString().slice(0, 10);
  const unreleasedHeader = '## [Unreleased]';
  const releasedHeader = `## [${version}] - ${today}`;

  const content = readFileSync(changelogPath, 'utf8');

  if (content.includes(releasedHeader)) {
    console.error(`[publish] CHANGELOG.md already contains "${releasedHeader}".`);
    console.error('[publish] looks like this version was already released.');
    process.exit(1);
  }

  const idx = content.indexOf(unreleasedHeader);
  if (idx === -1) {
    console.error(`[publish] CHANGELOG.md missing a "${unreleasedHeader}" section.`);
    process.exit(1);
  }

  // Body of [Unreleased] = text between its heading line and the
  // next top-level "## " heading (or EOF). HTML comments are
  // stripped before the emptiness check so a lone guidance comment
  // does not satisfy the "must write entries" requirement.
  const afterHeading = content.indexOf('\n', idx) + 1;
  const nextHeadingRel = content.indexOf('\n## ', afterHeading);
  const bodyEnd = nextHeadingRel === -1 ? content.length : nextHeadingRel;
  const body = content
    .slice(afterHeading, bodyEnd)
    .replace(/<!--[\s\S]*?-->/g, '')
    .trim();

  if (body.length === 0) {
    console.error('[publish] [Unreleased] section is empty.');
    console.error('[publish] add changelog entries under [Unreleased] before releasing,');
    console.error('[publish] or re-run with --force-changelog to bypass (not recommended).');
    if (!process.argv.includes('--force-changelog')) {
      process.exit(1);
    }
    console.warn('[publish] --force-changelog set; proceeding with empty changelog.');
  }

  // `before` = everything up to the [Unreleased] heading (this
  // includes the maintenance-guidance HTML comment, which therefore
  // stays pinned above the freshly reopened [Unreleased]).
  // `rest` = the old [Unreleased] body + all following sections; it
  // now becomes the body of the released version.
  const before = content.slice(0, idx);
  const rest = content.slice(idx + unreleasedHeader.length);
  const newContent = before + `${unreleasedHeader}\n\n` + releasedHeader + rest;
  writeFileSync(changelogPath, newContent);

  console.log(`[publish] CHANGELOG: [Unreleased] -> [${version}] - ${today}`);
}

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

// Fail fast on an empty/missing changelog BEFORE we spend time
// building the Rust server and packaging the .vsix.
bumpChangelog(pkg.version);

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

// By default the script leaves git alone after a successful publish
// — you commit the renamed CHANGELOG.md yourself. Pass --git to
// have the script auto-commit it instead (useful in CI).
if (process.argv.includes('--git')) {
  run(`git add CHANGELOG.md`, { cwd: repoRoot });
  run(`git commit -m "chore(changelog): release ${pkg.version}"`, { cwd: repoRoot });
}

console.log('');
console.log(`[publish] done!`);
console.log(`[publish] Manage: https://marketplace.visualstudio.com/manage/publishers/${pkg.publisher}`);
