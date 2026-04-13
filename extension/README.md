# capnprotols VS Code extension

Thin client that launches the [`capnprotols`](../README.md) language server.

## Install (development)

```sh
cd extension
npm install
npm run compile
# then in VS Code: F5 from this directory to launch a dev host with the extension loaded,
# or symlink/copy to ~/.vscode/extensions/capnprotols/
```

## Settings

| Key                          | Default        | Description                                          |
|------------------------------|----------------|------------------------------------------------------|
| `capnprotols.serverPath`     | `capnprotols`  | Path to the `capnprotols` binary.                    |
| `capnprotols.compilerPath`   | `capnp`        | Path to the `capnp` binary used by the server.       |
| `capnprotols.importPaths`    | `[]`           | Extra `-I` import paths.                             |
| `capnprotols.trace.server`   | `off`          | Trace LSP traffic to the Output panel.               |
