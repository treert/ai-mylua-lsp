import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

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

export function activate(context: vscode.ExtensionContext) {
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

  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('mylua') && client) {
        client.sendNotification('workspace/didChangeConfiguration', {
          settings: collectLspConfig(),
        });
      }
    }),
  );

  client.start();
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
