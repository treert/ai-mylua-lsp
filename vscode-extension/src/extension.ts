import * as fs from 'fs';
import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;
let clientNotificationDisposable: vscode.Disposable | undefined;
let readyNotified = false;
let restartInProgress = false;
let restartPromptPending = false;

const RESTART_RELEVANT_CONFIG_KEYS = [
  'mylua.server.path',
  'mylua.debug.fileLog',
  'mylua.runtime.version',
  'mylua.runtime.topKeyword',
  'mylua.require.aliases',
  'mylua.workspace.include',
  'mylua.workspace.exclude',
  'mylua.workspace.library',
  'mylua.workspace.useBundledStdlib',
  'mylua.performance.slowParseKeepTreeThresholdMs',
  'mylua.diagnostics.enable',
  'mylua.diagnostics.undefinedGlobal',
  'mylua.diagnostics.emmyTypeMismatch',
  'mylua.diagnostics.emmyUnknownField',
  'mylua.diagnostics.luaFieldError',
  'mylua.diagnostics.luaFieldWarning',
  'mylua.diagnostics.duplicateTableKey',
  'mylua.diagnostics.unusedLocal',
  'mylua.diagnostics.argumentCountMismatch',
  'mylua.diagnostics.argumentTypeMismatch',
  'mylua.diagnostics.returnMismatch',
  'mylua.diagnostics.scope',
  'mylua.inlayHint.enable',
  'mylua.inlayHint.parameterNames',
  'mylua.inlayHint.variableTypes',
  'mylua.documentSymbol.detailLevel',
  'mylua.gotoDefinition.strategy',
  'mylua.references.strategy',
  'mylua.references.scanComments',
];


type IndexStatusParams = {
  state: 'indexing' | 'diagnosing' | 'ready';
  indexed: number;
  total: number;
  elapsedMs?: number;
  /** Current indexing phase: 'scanning' | 'module_map_ready' | 'parsing' | 'merging' | 'diagnosing'. */
  phase?: 'scanning' | 'module_map_ready' | 'parsing' | 'merging' | 'diagnosing';
  /** Human-readable message for the current phase. */
  message?: string;
  /** Remaining files awaiting background diagnostics. */
  remaining?: number;
};

/// Bundled stdlib fallback chain. Ordered newest→oldest so the most
/// feature-complete stub tree is picked first. Bumped when we ship
/// additional `assets/lua<ver>/` directories.
const BUNDLED_LIBRARY_FALLBACKS = ['5.4'];

/// Absolute path to the bundled Lua stdlib stubs for the selected
/// runtime version. Since the stub tree lives under
/// `<extensionPath>/assets/lua<version>/` in **both** dev and
/// packaged layouts (moved out of the repo root precisely for this
/// reason), a single lookup per candidate covers both cases.
///
/// Behavior:
/// - Try the requested version first. If the exact bundled tree
///   exists, use it.
/// - Otherwise, walk `BUNDLED_LIBRARY_FALLBACKS` and return the
///   first existing tree. This keeps `runtime.version="5.3"` users
///   (the Lua 5.3/5.4 API surface overlaps ~99%) from ending up
///   with an empty library list just because we currently only
///   ship 5.4 stubs.
/// - Returns `undefined` only when the extension has no bundled
///   stubs at all (e.g. a stripped internal build).
function resolveBundledLibrary(
  context: vscode.ExtensionContext,
  version: string,
): string | undefined {
  const candidates = [version, ...BUNDLED_LIBRARY_FALLBACKS.filter((v) => v !== version)];
  for (const v of candidates) {
    const candidate = path.join(context.extensionPath, 'assets', `lua${v}`);
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function collectLspConfig(
  context: vscode.ExtensionContext,
): Record<string, unknown> {
  const cfg = vscode.workspace.getConfiguration('mylua');
  const version = String(cfg.get('runtime.version') ?? '5.4');
  const userLibrary = cfg.get<string[]>('workspace.library') ?? [];
  const useBundled = cfg.get<boolean>('workspace.useBundledStdlib') ?? true;
  const bundled = useBundled ? resolveBundledLibrary(context, version) : undefined;
  // Bundled path is prepended so user entries can shadow specific
  // names later (first-wins at scan time is the server's
  // responsibility, but the array order is preserved through
  // initializationOptions for determinism).
  const library = bundled ? [bundled, ...userLibrary] : userLibrary;
  return {
    runtime: {
      version,
      topKeyword: cfg.get('runtime.topKeyword'),
    },
    require: {
      aliases: cfg.get('require.aliases'),
    },
    workspace: {
      include: cfg.get('workspace.include'),
      exclude: cfg.get('workspace.exclude'),
      library,
    },
    performance: {

      slowParseKeepTreeThresholdMs: cfg.get('performance.slowParseKeepTreeThresholdMs'),
    },
    diagnostics: {
      enable: cfg.get('diagnostics.enable'),
      undefinedGlobal: cfg.get('diagnostics.undefinedGlobal'),
      emmyTypeMismatch: cfg.get('diagnostics.emmyTypeMismatch'),
      emmyUnknownField: cfg.get('diagnostics.emmyUnknownField'),
      luaFieldError: cfg.get('diagnostics.luaFieldError'),
      luaFieldWarning: cfg.get('diagnostics.luaFieldWarning'),
      duplicateTableKey: cfg.get('diagnostics.duplicateTableKey'),
      unusedLocal: cfg.get('diagnostics.unusedLocal'),
      argumentCountMismatch: cfg.get('diagnostics.argumentCountMismatch'),
      argumentTypeMismatch: cfg.get('diagnostics.argumentTypeMismatch'),
      returnMismatch: cfg.get('diagnostics.returnMismatch'),
      scope: cfg.get('diagnostics.scope'),
    },
    documentSymbol: {
      detailLevel: cfg.get('documentSymbol.detailLevel'),
    },
    gotoDefinition: {
      strategy: cfg.get('gotoDefinition.strategy'),
    },
    references: {
      strategy: cfg.get('references.strategy'),
      scanComments: cfg.get('references.scanComments'),
    },
    inlayHint: {
      enable: cfg.get('inlayHint.enable'),
      parameterNames: cfg.get('inlayHint.parameterNames'),
      variableTypes: cfg.get('inlayHint.variableTypes'),
    },
    debug: {
      fileLog: cfg.get('debug.fileLog'),
    },
  };
}

function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} 秒`;
}

function renderStatus(status: IndexStatusParams): void {
  if (!statusBarItem) return;
  if (status.state === 'ready') {
    statusBarItem.text = '💚mylua';
    statusBarItem.tooltip = `MyLua: index ready (${status.total} files) — click to open settings`;
    // Show the one-shot "索引完成" toast exactly once per session —
    // the server only emits a single `ready` with elapsed_ms, but
    // guard here too so a defensive re-emit doesn't spam the user.
    //
    // VS Code's `showInformationMessage` has no auto-dismiss — it
    // stays until the user clicks the close button. We use
    // `withProgress({ location: Notification })` + a timed promise
    // instead, which renders the same kind of notification toast
    // but is torn down as soon as our task promise resolves. ~4s
    // is enough to read a short status line without being intrusive.
    if (!readyNotified && typeof status.elapsedMs === 'number') {
      readyNotified = true;
      const elapsed = formatElapsed(status.elapsedMs);
      vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: `MyLua 索引完成，耗时 ${elapsed}（${status.total} 个文件）`,
          cancellable: false,
        },
        () => new Promise<void>((resolve) => setTimeout(resolve, 4000)),
      );
    }
  } else if (status.state === 'diagnosing') {
    const r = status.remaining ?? 0;
    statusBarItem.text = `💚${r}`;
    statusBarItem.tooltip = `MyLua: diagnosing files (${r} remaining) — click to open settings`;
  } else {
    const total = status.total;
    const phase = status.phase;
    if (phase === 'scanning') {
      statusBarItem.text = '💛scanning…';
      statusBarItem.tooltip = 'MyLua: scanning workspace for Lua files… — click to open settings';
    } else if (phase === 'module_map_ready') {
      statusBarItem.text = `💛parsing ${total}`;
      statusBarItem.tooltip = `MyLua: module map ready, parsing files (${total})… — click to open settings`;
    } else if (phase === 'merging') {
      statusBarItem.text = `💛merging ${total}`;
      statusBarItem.tooltip = `MyLua: building global index (${total} files)… — click to open settings`;
    } else if (total > 0) {
      statusBarItem.text = `💛${status.indexed}/${total}`;
      statusBarItem.tooltip = `MyLua: parsing files (${status.indexed}/${total}) — click to open settings`;
    } else {
      statusBarItem.text = '💛mylua';
      statusBarItem.tooltip = 'MyLua: indexing workspace… — click to open settings';
    }
  }
  statusBarItem.show();
}

function createLanguageClient(
  context: vscode.ExtensionContext,
  luaFileWatcher: vscode.FileSystemWatcher,
): LanguageClient {
  const serverPath = getServerPath(context);
  const serverOptions: ServerOptions = {
    run: { command: serverPath },
    debug: { command: serverPath },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'lua' }],
    initializationOptions: collectLspConfig(context),
    synchronize: {
      fileEvents: luaFileWatcher,
    },
    middleware: {
      provideDefinition: async (document, position, token, next) => {
        // TEMP-DISABLED: QuickPick ordering workaround disabled for investigation.
        // VS Code's built-in peek view sorts multi-candidate `Location[]` and
        // `LocationLink[]` by URI, destroying the server's `UriPriority` order.
        // Uncomment the block below to re-enable the QuickPick workaround.
        /*
        const result = await next(document, position, token);
        if (!result || !Array.isArray(result) || result.length <= 1) {
          return result;
        }

        const items = result.map((item: any) => {
          // LocationLink uses targetUri/targetRange; Location uses uri/range.
          const uri = item.targetUri ?? item.uri;
          const range = item.targetRange ?? item.range;
          return {
            label: path.basename(uri.fsPath),
            description: vscode.workspace.asRelativePath(uri),
            detail: `Line ${range.start.line + 1}`,
            item,
          };
        });

        const picked = await vscode.window.showQuickPick(items, {
          placeHolder: `Select a definition (${items.length} candidates)`,
        });
        return picked ? [picked.item] : undefined;
        */
        return next(document, position, token);
      },
    },
  };
  const next = new LanguageClient(
    'mylua-lsp',
    'MyLua Language Server',
    serverOptions,
    clientOptions,
  );
  clientNotificationDisposable = next.onNotification(
    'mylua/indexStatus',
    (params: IndexStatusParams) => renderStatus(params),
  );
  return next;
}

function handleClientStartError(err: unknown): void {
  if (!statusBarItem) return;
  statusBarItem.text = '⚠️mylua';
  const msg = err instanceof Error ? err.message : String(err);
  statusBarItem.tooltip = `MyLua: failed to start (${msg}) — click to open settings`;
}

async function restartLanguageClient(
  context: vscode.ExtensionContext,
  luaFileWatcher: vscode.FileSystemWatcher,
): Promise<void> {
  if (restartInProgress) return;
  restartInProgress = true;
  readyNotified = false;
  if (statusBarItem) {
    statusBarItem.text = '💛restarting…';
    statusBarItem.tooltip = 'MyLua: restarting language server…';
  }
  const oldClient = client;
  client = undefined;
  clientNotificationDisposable?.dispose();
  clientNotificationDisposable = undefined;
  try {
    await oldClient?.stop();
    const next = createLanguageClient(context, luaFileWatcher);
    client = next;
    await next.start();
  } catch (err: unknown) {
    handleClientStartError(err);
  } finally {
    restartInProgress = false;
  }
}

function affectsRestartRelevantConfig(e: vscode.ConfigurationChangeEvent): boolean {
  return RESTART_RELEVANT_CONFIG_KEYS.some((key) => e.affectsConfiguration(key));
}

async function handleConfigurationChange(
  context: vscode.ExtensionContext,
  luaFileWatcher: vscode.FileSystemWatcher,
  e: vscode.ConfigurationChangeEvent,
): Promise<void> {
  if (!client || restartInProgress || !affectsRestartRelevantConfig(e)) return;
  const autoRestart = vscode.workspace
    .getConfiguration('mylua')
    .get<boolean>('server.autoRestartOnConfigChange') ?? false;
  if (autoRestart) {
    await restartLanguageClient(context, luaFileWatcher);
    return;
  }

  try {
    await client.sendNotification('workspace/didChangeConfiguration', {
      settings: collectLspConfig(context),
    });
  } catch {
    // Best-effort only; the explicit restart prompt below is authoritative.
  }

  if (restartPromptPending) return;
  restartPromptPending = true;
  const restart = '重启 LSP';
  const choice = await vscode.window.showInformationMessage(
    'MyLua 配置已更新，重启 LSP 后可确保所有配置完全生效。',
    restart,
    '稍后',
  );
  restartPromptPending = false;
  if (choice === restart) {
    await restartLanguageClient(context, luaFileWatcher);
  }
}

export function activate(context: vscode.ExtensionContext) {
  statusBarItem = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    100,
  );
  statusBarItem.name = 'MyLua';
  statusBarItem.text = '💛mylua';
  statusBarItem.tooltip = 'MyLua: starting… (click to open settings)';
  // Clicking the status-bar item opens the Settings UI already
  // filtered to this extension's contributed configuration. The
  // `@ext:<publisher>.<name>` filter is resolved from package.json:
  // publisher="onemore" + name="mylua-lsp" → `onemore.mylua-lsp`.
  // No need to register a wrapper command — the built-in
  // `workbench.action.openSettings` accepts a filter argument.
  statusBarItem.command = {
    command: 'workbench.action.openSettings',
    title: 'Open MyLua Settings',
    arguments: ['@ext:onemore.mylua-lsp'],
  };
  statusBarItem.show();
  // Owned by context.subscriptions; VS Code will dispose on extension
  // unload, so `deactivate` does not need to dispose explicitly.
  context.subscriptions.push(statusBarItem);

  const luaFileWatcher = vscode.workspace.createFileSystemWatcher('**/*.lua');
  context.subscriptions.push(luaFileWatcher);

  client = createLanguageClient(context, luaFileWatcher);

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      void handleConfigurationChange(context, luaFileWatcher, e);
    }),
  );

  client.start().catch(handleClientStartError);
}

export function deactivate(): Thenable<void> | undefined {
  clientNotificationDisposable?.dispose();
  return client?.stop();
}

/// `mylua.server.path` accepts either a bare string (legacy form,
/// same path on every OS) or an object mapping Node.js
/// `process.platform` keys to paths. Unknown platforms fall through
/// to the auto-detect chain.
type ServerPathConfig =
  | string
  | Partial<Record<'darwin' | 'linux' | 'win32', string>>
  | undefined
  | null;

function serverBinaryName(): string {
  return process.platform === 'win32' ? 'mylua-lsp.exe' : 'mylua-lsp';
}

/// Platforms with a dedicated key in `mylua.server.path`'s object
/// form. `process.platform` returns a wider union (incl. `freebsd`,
/// `sunos`, etc.) but we only commit schema / UX support to the
/// three Tier-1 targets. Users on other platforms fall through to
/// the auto-detect chain — a console.warn is emitted from
/// `pickConfiguredServerPath` to surface that in the Output panel.
const KNOWN_PLATFORM_KEYS = ['darwin', 'linux', 'win32'] as const;
type KnownPlatform = (typeof KNOWN_PLATFORM_KEYS)[number];

function isKnownPlatform(p: NodeJS.Platform): p is KnownPlatform {
  return (KNOWN_PLATFORM_KEYS as readonly string[]).includes(p);
}

/// Extract a platform-appropriate override path from the raw
/// `mylua.server.path` value, returning `undefined` when nothing
/// applies so the caller can continue the fallback chain. Trimming
/// and empty-string guards live here so the rest of `getServerPath`
/// can treat the result as "user said this exactly".
///
/// Behavior by input shape:
/// - `undefined` / `null` / `""` / `"   "` — returns `undefined`.
/// - bare string — trimmed, applied to every platform.
/// - object — looks up `process.platform` among `KNOWN_PLATFORM_KEYS`;
///   if the current platform is not among them (e.g. `freebsd`),
///   logs a one-liner and returns `undefined` so auto-detect runs.
///   If the current platform is known but its entry is missing /
///   empty, same fallthrough.
function pickConfiguredServerPath(raw: ServerPathConfig): string | undefined {
  if (raw == null) return undefined;
  if (typeof raw === 'string') {
    const trimmed = raw.trim();
    return trimmed.length > 0 ? trimmed : undefined;
  }
  if (typeof raw === 'object') {
    const platform = process.platform;
    if (!isKnownPlatform(platform)) {
      console.warn(
        `[mylua] process.platform=${platform} has no entry in mylua.server.path; ` +
          `falling back to auto-detect. Supported keys: ${KNOWN_PLATFORM_KEYS.join(', ')}.`,
      );
      return undefined;
    }
    const entry = raw[platform];
    if (typeof entry === 'string') {
      const trimmed = entry.trim();
      return trimmed.length > 0 ? trimmed : undefined;
    }
  }
  return undefined;
}

function devServerPath(context: vscode.ExtensionContext): string {
  const buildMode = process.env.MYLUA_LSP_BUILD ?? 'debug';
  return path.resolve(
    context.extensionPath,
    '..',
    'lsp',
    'target',
    buildMode,
    serverBinaryName(),
  );
}

function getServerPath(context: vscode.ExtensionContext): string {
  const config = vscode.workspace.getConfiguration('mylua');
  const configured = pickConfiguredServerPath(
    config.get<ServerPathConfig>('server.path'),
  );
  if (configured) {
    return configured;
  }

  // Non-production (Development via F5, or Test via
  // @vscode/test-electron) deliberately bypasses
  // `<extensionPath>/server/` — that directory is populated only by
  // `npm run prepackage` and frequently lags behind fresh
  // `cargo build` output during active LSP work. Pointing straight
  // at the dev target keeps the edit → cargo build → F5 loop tight
  // and avoids "why aren't my changes taking effect" confusion.
  // Covering Test mode here too keeps extension-level integration
  // tests (if/when added) from inheriting the packaging dependency.
  if (context.extensionMode !== vscode.ExtensionMode.Production) {
    return devServerPath(context);
  }

  // Production: shipped .vsix always has `server/<bin>`. If it
  // somehow got stripped we still try the dev path as a last resort
  // so the extension degrades to a clear "file not found" error
  // from the child_process spawn rather than an undefined command.
  const bundled = path.join(context.extensionPath, 'server', serverBinaryName());
  if (fs.existsSync(bundled)) {
    return bundled;
  }
  return devServerPath(context);
}
