# @lszio/zio

The Zio language toolchain. The package carries no binaries of its own:
on first run it downloads the release archive that matches the installed
version, verifies it against the published checksum, and runs the `zio`
inside it.

## Install

v0.2.2 is an alpha preview; the npm `latest` tag is still 0.2.1. Install
the alpha channel to get it:

```bash
npm install -g @lszio/zio@alpha
```

Tracking the `latest` tag (`npm install -g @lszio/zio`) installs the most
recent non-alpha release instead.

```bash
zio program.zio          # run a script
zio                     # REPL
zio fmt program.zio     # format (--write writes back, --check checks)
zio lsp                 # language server on stdio
zio-lsp                 # the same server, directly
```

Note: `zio` runs scripts by file path or stdin (`-`); there is no
`--eval` mode.

## Platforms

Release archives are published for **Linux x86_64 and Linux arm64 only**;
macOS and Windows are not yet delivered. The download needs `curl`
(or a curl-compatible tool) and `tar` on PATH. Outside those platforms
the wrapper reports the matching-archive lookup failure rather than
pretending to run.

The engine as WebAssembly shares the same core — bytecode VM, macros and
the self-hosting compiler frontend — but runs on its own host surface
(session API in browser and Node, not the native host). It is published
separately as [`@lszio/zio-core`](https://www.npmjs.com/package/@lszio/zio-core).

Documentation and the language reference live in the
[Zio repository](https://github.com/lszio/zio).
