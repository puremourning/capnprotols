import * as vscode from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(_context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration("capnprotols");
  const serverPath = config.get<string>("serverPath", "capnprotols");

  const serverOptions: ServerOptions = {
    run: { command: serverPath, transport: TransportKind.stdio },
    debug: { command: serverPath, transport: TransportKind.stdio },
  };

  const initializationOptions = {
    compilerPath: config.get<string>("compilerPath"),
    importPaths: config.get<string[]>("importPaths"),
    format: {
      enabled: config.get<boolean>("format.enabled"),
      maxWidth: config.get<number>("format.maxWidth"),
      warnLongLines: config.get<boolean>("format.warnLongLines"),
    },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "capnp" }],
    initializationOptions,
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher("**/*.capnp"),
    },
  };

  client = new LanguageClient(
    "capnprotols",
    "Cap'n Proto",
    serverOptions,
    clientOptions
  );

  client.start();
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
