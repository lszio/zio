# Releasing Zio

Zio is a semantically independent general-purpose Lisp. Rust supplies the
bootstrap implementation, runtime mechanisms and trusted host. The current
release produces two separate artifacts: a native distribution built by
`tools/install.sh`, and the browser engine built by `tools/build-wasm.sh`.
Both come from the same tagged commit; the tag is checked against the workspace
version before anything is built. These are the existing release paths, not
yet the unified complete developer distribution described below.

## Complete distribution target (planned)

The [language and toolchain design](superpowers/specs/2026-10-11-zio-language-toolchain-design.md)
and [implementation plan](superpowers/plans/2026-10-11-zio-language-toolchain.md)
define two separate goals: a Zio metacircular evaluator and self-hosting
compiler; and runtime image save/restore with application bin/WASM export.
The repository already has a bytecode VM, a Zio compiler and self-hosting
contracts, LSP and tree-sitter resources. Their existence is not evidence that
the complete bootstrap or image lifecycle has been delivered.

The planned complete developer package includes the interpreter, compiler,
LSP, syntax resources, WASM and documentation in one release. External
interpreted and compiled languages integrate through engine libraries;
`inline` denotes foreign-code entry, not necessarily immediate `eval`.
It does not promise evaluation of arbitrary Rust source, or mean that external
engines are already bundled. Any new CLI/API in the design is a proposal, not
an available release command.

Minimal application artifacts are a different product:

- The initial bin export means bytecode plus its runtime, not machine-code AOT.
- Application bin/WASM artifacts do not carry LSP or other development tools.
  Dynamic evaluation, modules and foreign engines must be retained according
  to actual application dependencies; they cannot be removed merely because
  the application was compiled.
- Browser WASM and WASI are distinct targets with distinct host capabilities.
  The existing JavaScript bridge is WASM-only, and dotted JavaScript resolution
  is interpreter-only; the native CLI does not supply JavaScript.
- Images must preserve object sharing, cycles and closures. External resources
  require explicit reconstruction or saving must be rejected; restoring an
  image must obtain fresh host grants rather than restoring authority.

Groiz is an optional independent application, not a required component of the
language distribution target. The current installer includes Groiz; the
archive layout and commands below describe that existing fact.

## Before tagging

```bash
cargo test --workspace                        # must be green
cargo test -p zio-core --test self_hosting    # the compiler compiles itself
python3 tools/lsp_probe.py target/debug/zio-lsp   # a real LSP session
./tools/install.sh dist/zio                   # assemble locally and run it
```

The distribution is meant to run without the checkout. Verify that
rather than assume it. The shipped Zio sources are not
formatter-canonical yet: `zio fmt --check` reports which files would
move, and `zio fmt --write share/zio/` is the safe one-liner. The
formatter is delivered as a user tool, not a build step.

```bash
cd "$(mktemp -d)"
/path/to/dist/zio/bin/zio script.zio          # language
/path/to/dist/zio/bin/groiz --help            # product launcher
```

## Cutting a release

```bash
# 1. The version lives in exactly one place.
version=$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)
git commit -am "release: v$version"
git tag "v$version"
git push origin main --tags
```

Pushing the tag triggers `.github/workflows/release.yml`:

| Job | What it does |
|---|---|
| `plan` | Checks the tag equals `v<workspace version>`; stops otherwise |
| `verify` | Full test suite, self-hosting contracts, LSP session |
| `native` | Five targets: linux x86_64/aarch64, macos x86_64/aarch64, windows x86_64 |
| `crates` | Publishes `zio-core`, `zio-js`, `zio-host`, `zio-lsp`, `zio-wasi`, `zio-cli`, in that order |
| `npm` | Publishes `@lszio/zio-core`, the WASM engine |
| `npm-cli` | Publishes `@lszio/zio`, the CLI wrapper that downloads the matching platform archive |
| `docker` | Pushes `ghcr.io/lszio/zio` |
| `github-release` | Attaches the archives, checksums, manifest and the WASM payload |

Every job is gated on `verify`, so a broken test run publishes nothing.
`workflow_dispatch` with `dry_run: true` runs everything except the
publishing steps, which is the way to check a release without cutting
one.

The `crates` job is strict: an unpublished package stops the release
rather than warning about it, because a silently missing Rust library
is a release that claims a surface it did not deliver. The crate named
`loom` (`contribs/native/loom`) is deliberately not published — the name
is taken on crates.io by an unrelated crate, and the in-repo consumers
depend on it by that path — so it carries `publish = false`.

The `native` job passes `ZIO_TARGET` to `tools/install.sh`, which builds
for that triple and installs from `target/<triple>/release`. An
installer that rebuilt for the runner's own host would ship the host's
binary in a cross-compiled archive: an x86_64 macOS archive carrying an
arm64 binary is a wrong answer that looks like success.

The `verify` job treats the Grove submodule as optional: that repository
is separate, and a checkout that could not fetch it warns and skips its
contracts instead of failing a language release that does not need an
optional application.

## The native archive

```
bin/zio            the language: REPL, script, bounded candidate runner, app launcher
bin/zio-lsp        the language server (zio lsp starts this too)
bin/groiz          the product launcher; grants come from its own flags (POSIX targets)
bin/zio-env.sh     PATH and ZIO_PATH for the installed tree
bin/zio-env.ps1    the same, for PowerShell (Windows targets)
share/zio/         stdlib, libraries, toolchain sources, grammar, docs, examples
share/zio/std/     the stdlib source (embedded in the binary at build time)
share/zio/langs/   the metacircular interpreter and compiler sources
share/zio/grammar  Tree-sitter grammar and queries
share/zio/docs     documentation
share/zio/examples runnable and documentary examples
share/tensor/      the tensor backend resource
manifest.json      version, commit, target, and a checksum per component
```

`groiz` decides capabilities from its own flags and hands them to the
Zio entry as one keyword map. It never reads them out of the source it
is about to run — a candidate that could name its own tensor backend
would make the isolation profile advisory.

## Crates

`cargo publish` needs a `CARGO_REGISTRY_TOKEN` repository secret. The
`crates` job is `continue-on-error`, because a package already published
at this version is a skip rather than a failure, and a token without
publish scope should not stop the GitHub release from happening.

## Publish surfaces

| Surface | Artifact | Job |
|---|---|---|
| Executables | Five-platform archives (`bin/zio`, `bin/zio-lsp`, `bin/groiz` on POSIX) with checksums and `manifest.json` | `native` |
| Executable container | `ghcr.io/lszio/zio` with `zio` on PATH | `docker` |
| Rust execution library | [`zio-core`](https://crates.io/crates/zio-core), `zio-js`, `zio-host`, `zio-lsp`, `zio-wasi`, `zio-cli` on crates.io; the `wasm` feature builds the browser engine | `crates` (needs `CARGO_REGISTRY_TOKEN`) |
| npm CLI | `@lszio/zio` — downloads the release archive matching the installed version, checksums it, runs `zio` | `npm-cli` (needs `NPM_TOKEN`) |
| npm engine | `@lszio/zio-core` — the same engine as WebAssembly; `ZioSession` locates and loads the Zio runtime in browsers and Node | `npm` (needs `NPM_TOKEN`) |
| Single-file apps | `zio build INPUT --target native -o APP` | produced by users |

The npm job publishes only after `verify`; the version is synced from the
tag at staging time. `NPM_TOKEN` must exist as a repository secret or
that job fails while every other job stands.

## What CI checks that a local run often skips

- `cargo fmt --check` and `cargo clippy -D warnings`
- the WASM engine actually builds (`wasm-bindgen` is a separate binary
  from the compiler, so CI installs it explicitly)
- the library contracts in `langs/cli/tests/libs/*.zio`
- a real stdio language-server session, including the check that a
  document containing `(eval (spit ...))` is analyzed as text and never
  runs

## Site version and build provenance

The site reads its language version from `[workspace.package].version` in
`Cargo.toml`; there is no separately maintained page version. A build is
labelled a release only when its commit has the matching `v<version>` tag
and its working tree is clean. Untagged, modified or unverifiable builds
are labelled development builds. The footer shows the documentation build's
branch, commit link and whether it includes uncommitted changes.

Local builds read Git metadata. Source archives and Docker builds without
Git show unavailable provenance unless CI supplies these build variables:

| Variable | Value |
|---|---|
| `ZIO_BUILD_COMMIT` | Full hexadecimal commit SHA |
| `ZIO_BUILD_BRANCH` | Branch name; empty for detached HEAD |
| `ZIO_BUILD_TAG` | Exact tag on the built commit, such as `v0.2.0` |
| `ZIO_BUILD_DIRTY` | `true` or `false`; omit if cleanliness is unknown |

CI must derive these values from the checkout it actually builds, not from
the latest branch tip or a requested tag. The site Dockerfile accepts the
same names as build arguments and copies `Cargo.toml` for the version.
Metadata is embedded at build time; rebuilding is required to update it.
Historical documentation-version switching is not provided.

Check release classification with:

```sh
node --test apps/site/src/lib/build-info.test.mjs
```
