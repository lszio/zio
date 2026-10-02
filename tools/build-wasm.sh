#!/usr/bin/env bash
set -euo pipefail
# ── Rebuild site/wasm from core ──────────────────────────────
# The landing-page REPL loads site/wasm/zio_core.js (wasm-bindgen `web`
# glue). site/ is copied verbatim by the Dockerfile, so a stale wasm means
# the page runs an old engine. Run this after any core/src change.
# Requires: rustup target wasm32-unknown-unknown, wasm-bindgen-cli.
# Usage: ./tools/build-wasm.sh

cd "$(dirname "$0")/.."

cargo build -p zio-core --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen --target web \
  target/wasm32-unknown-unknown/release/zio_core.wasm \
  --out-dir site/wasm

echo "site/wasm updated:"
ls -l site/wasm
