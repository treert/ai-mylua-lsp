import * as path from 'path';
import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext) {
  const serverPath = getServerPath(context);

  const serverOptions: ServerOptions = {
    run: { command: serverPath },
    debug: { command: serverPath },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'lua' }],
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
