#!/usr/bin/env bash
set -euo pipefail
# ── Rebuild the site's WASM engine from core ────────────────────
# The landing-page REPL loads site/public/wasm/zio_core.js (wasm-bindgen `web`
# glue) and astro copies public/ verbatim into dist/. site/public/wasm is
# committed, so a stale bundle means the page silently runs an old engine.
# Run this after any core/src change, before `docker build` / `bun run build`.
# Requires: rustup target wasm32-unknown-unknown, wasm-bindgen-cli.
# Usage: ./tools/build-wasm.sh

cd "$(dirname "$0")/.."

cargo build -p zio-core --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen --target web \
  target/wasm32-unknown-unknown/release/zio_core.wasm \
  --out-dir site/public/wasm

echo "site/public/wasm updated:"
ls -l site/public/wasm
