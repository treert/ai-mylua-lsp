import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;
let statusBarItem: vscode.StatusBarItem | undefined;
let readyNotified = false;

type IndexStatusParams = {
  state: 'indexing' | 'ready';
  indexed: number;
  total: number;
  elapsedMs?: number;
};

function collectLspConfig(): Record<string, unknown> {
  const cfg = vscode.workspace.getConfiguration('mylua');
  return {
    runtime: {
      version: cfg.get('runtime.version'),
    },
    require: {
      paths: cfg.get('require.paths'),
      aliases: cfg.get('require.aliases'),
    },
    workspace: {
      include: cfg.get('workspace.include'),
      exclude: cfg.get('workspace.exclude'),
      indexMode: cfg.get('workspace.indexMode'),
    },
    index: {
      cacheMode: cfg.get('index.cacheMode'),
    },
    diagnostics: {
      enable: cfg.get('diagnostics.enable'),
      undefinedGlobal: cfg.get('diagnostics.undefinedGlobal'),
      emmyTypeMismatch: cfg.get('diagnostics.emmyTypeMismatch'),
      emmyUnknownField: cfg.get('diagnostics.emmyUnknownField'),
      luaFieldError: cfg.get('diagnostics.luaFieldError'),
      luaFieldWarning: cfg.get('diagnostics.luaFieldWarning'),
    },
    gotoDefinition: {
      strategy: cfg.get('gotoDefinition.strategy'),
    },
    references: {
      strategy: cfg.get('references.strategy'),
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
  } else {
    const total = status.total;
    if (total > 0) {
      statusBarItem.text = `💛${status.indexed}/${total}`;
    } else {
      statusBarItem.text = '💛mylua';
    }
    statusBarItem.tooltip = `MyLua: indexing workspace (${status.indexed}/${total}) — click to open settings`;
  }
  statusBarItem.show();
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
  // publisher="mylua" + name="mylua" → `mylua.mylua`. No need to
  // register a wrapper command — the built-in
  // `workbench.action.openSettings` accepts a filter argument.
  statusBarItem.command = {
    command: 'workbench.action.openSettings',
    title: 'Open MyLua Settings',
    arguments: ['@ext:mylua.mylua'],
  };
  statusBarItem.show();
  // Owned by context.subscriptions; VS Code will dispose on extension
  // unload, so `deactivate` does not need to dispose explicitly.
  context.subscriptions.push(statusBarItem);

  const serverPath = getServerPath(context);

  const serverOptions: ServerOptions = {
    run: { command: serverPath },
    debug: { command: serverPath },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'lua' }],
    initializationOptions: collectLspConfig(),
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.lua'),
    },
  };

  client = new LanguageClient(
    'mylua-lsp',
    'MyLua Language Server',
    serverOptions,
    clientOptions,
  );

  // Register the notification handler BEFORE start() to avoid any
  // race where an early `mylua/indexStatus` could fire before a
  // post-start `.then()` callback runs. `vscode-languageclient`
  // buffers handler registrations until the connection is up.
  context.subscriptions.push(
    client.onNotification(
      'mylua/indexStatus',
      (params: IndexStatusParams) => renderStatus(params),
    ),
  );

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('mylua') && client) {
        client.sendNotification('workspace/didChangeConfiguration', {
          settings: collectLspConfig(),
        });
      }
    }),
  );

  client.start().catch((err: unknown) => {
    if (statusBarItem) {
      statusBarItem.text = '⚠️mylua';
      const msg = err instanceof Error ? err.message : String(err);
      statusBarItem.tooltip = `MyLua: failed to start (${msg}) — click to open settings`;
    }
  });
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}

function getServerPath(context: vscode.ExtensionContext): string {
  const config = vscode.workspace.getConfiguration('mylua');
  const configPath = config.get<string>('server.path');
  if (configPath && configPath.trim().length > 0) {
    return configPath;
  }

  const isWin = process.platform === 'win32';
  const binaryName = isWin ? 'mylua-lsp.exe' : 'mylua-lsp';

  const bundled = path.join(context.extensionPath, 'server', binaryName);
  try {
    require('fs').accessSync(bundled);
    return bundled;
  } catch {
    // Fall through to dev mode path
  }

  const devPath = path.resolve(
    context.extensionPath,
    '..',
    'lsp',
    'target',
    'debug',
    binaryName,
  );
  return devPath;
}
