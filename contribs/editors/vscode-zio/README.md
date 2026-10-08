# vscode-zio

Zio language support for Visual Studio Code. Uses a hand-rolled framed stdio
LSP client against the `zio-lsp` binary. No `vscode-languageclient` and no
other third-party LSP libraries.

## Layout

```
contribs/editors/vscode-zio/
├── package.json                   # extension manifest; declares engine, lang id, grammar
├── tsconfig.json                  # TypeScript build config (ES2022 / Node16)
├── language-configuration.json    # comment / bracket / autopair config for .zio
├── syntaxes/
│   └── zio.tmLanguage.json        # baseline TextMate grammar (highlights only)
└── src/
    └── extension.ts               # hand-rolled LSP client + providers
```

## Build / install

```bash
cd contribs/editors/vscode-zio
npm install
npm run compile   # produces out/extension.js
```

Install the packaged `.vsix`:

```bash
npx vsce package
code --install-extension zio-0.1.0.vsix
```

Or for development, point VS Code at this folder with `code --install-extension
contribs/editors/vscode-zio` after `npm run compile`, or symlink the folder
into `~/.vscode/extensions/zio-0.1.0`.

## Configuration

The extension reads two settings:

| Setting | Default | Description |
|---------|---------|-------------|
| `zio.serverPath` | `zio-lsp` | Command used to spawn the language server. Absolute paths or PATH lookups both work. |
| `zio.serverArgs` | `[]` | Extra arguments passed to `zio-lsp`. |

If `zio-lsp` is installed via Cargo, the default `zio-lsp` works as long as
`~/.cargo/bin` is on the PATH VS Code inherits. For workspaces that load the
server from a custom build, set `zio.serverPath` to the absolute path of the
binary.

## LSP wire protocol

`extension.ts` implements the JSON-RPC framed transport in
`LspClient.consume` and `LspClient.send`. Only the `vscode` API and Node's
`child_process` are used; the client reads `Content-Length` headers, decodes
UTF-8 bodies, and dispatches notifications (diagnostics, log, show message)
separately from responses.

The extension registers providers for:

- `textDocument/documentSymbol`
- `textDocument/definition`
- `textDocument/references`
- `textDocument/hover`
- `textDocument/completion` (trigger characters `/` and `:`)
- `textDocument/semanticTokens/full`

Diagnostics arrive via `textDocument/publishDiagnostics`. Document lifecycle
uses full-document open and incremental range-based edits on change.
