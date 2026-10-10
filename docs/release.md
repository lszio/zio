# Releasing Zio

Two artifacts come out of a release: a native distribution built by
`tools/install.sh`, and the browser engine built by
`tools/build-wasm.sh`. Both are produced by the same tagged commit, and
the tag is checked against the workspace version before anything is
built, because a release whose binaries announce the wrong version is a
wrong answer rather than a failed one.

## Before tagging

```bash
cargo test --workspace                        # must be green
cargo test -p zio-core --test self_hosting    # the compiler compiles itself
python3 tools/lsp_probe.py target/debug/zio-lsp   # a real LSP session
./tools/install.sh dist/zio                   # assemble locally and run it
```

The distribution is meant to run without the checkout. Verify that
rather than assume it:

```bash
cd "$(mktemp -d)"
/path/to/dist/zio/bin/zio script.zio          # language
/path/to/dist/zio/bin/grove --help            # product launcher
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
| `crates` | Publishes `zio-core`, `zio-host`, `zio-cli`, `zio-lsp` |
| `github-release` | Attaches the archives, checksums, and the WASM payload |

Every job is gated on `verify`, so a broken test run publishes nothing.
`workflow_dispatch` with `dry_run: true` runs everything except the
publishing steps, which is the way to check a release without cutting
one.

## The native archive

```
bin/zio          the language: REPL, script, bounded candidate runner, app launcher
bin/zio-lsp      the language server
bin/grove        the product launcher; grants come from its own flags
bin/zio-env.sh   PATH and ZIO_PATH for the installed tree
share/zio/       stdlib, libraries, compiler, applications
share/tensor/    the tensor backend resource
```

`grove` decides capabilities from its own flags and hands them to the
Zio entry as one keyword map. It never reads them out of the source it
is about to run — a candidate that could name its own tensor backend
would make the isolation profile advisory.

## Crates

`cargo publish` needs a `CARGO_REGISTRY_TOKEN` repository secret. The
`crates` job is `continue-on-error`, because a package already published
at this version is a skip rather than a failure, and a token without
publish scope should not stop the GitHub release from happening.

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
