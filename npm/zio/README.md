# @lszio/zio

The Zio language toolchain. The package carries no binaries of its own:
on first run it downloads the release archive that matches the installed
version, checks it against the published checksum, and runs the `zio`
inside it.

```bash
npm install -g @lszio/zio
zio program.zio          # run a script
zio                     # REPL
zio fmt program.zio     # format
zio lsp                 # language server on stdio
zio-lsp                 # the same server, directly
```

Supported platforms are the ones the release publishes archives for:
linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64. The download
needs `curl` (or a curl-compatible tool) and `tar` on PATH; on Windows
the archive unpacks through PowerShell.

The engine as WebAssembly — interpreter, compiler and the same session
the CLI runs — is published separately as
[`@lszio/zio-core`](https://www.npmjs.com/package/@lszio/zio-core), for
browser and Node embedding.

Documentation and the language reference live in the
[Zio repository](https://github.com/lszio/zio).
