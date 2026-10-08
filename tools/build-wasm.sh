#!/usr/bin/env bash
set -euo pipefail
# ── Rebuild the site's WASM engine from langs/core ───────────────
# The playground loads apps/site/public/wasm/zio_core.js (wasm-bindgen `web`
# glue), and Astro copies public/ verbatim into dist/. The bundle is
# committed, so a stale bundle means the page silently runs an old engine.
# Run this after any langs/core/src or libs/std/core.zio change.
# Requires: rustup target wasm32-unknown-unknown, wasm-bindgen-cli.
# Usage: ./tools/build-wasm.sh

cd "$(dirname "$0")/.."

cargo build -p zio-core --target wasm32-unknown-unknown --release --features wasm
wasm-bindgen --target web \
  target/wasm32-unknown-unknown/release/zio_core.wasm \
  --out-dir apps/site/public/wasm

echo "apps/site/public/wasm updated"
