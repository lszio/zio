#!/usr/bin/env bash
# Build the WASI runtime template: wasm32-wasip1, WASI imports only.
# Applications are produced by `zio build --target wasm-wasi`, which
# recompiles this crate with the payload compiled in.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

rustup target add wasm32-wasip1 >/dev/null
cargo build --release -p zio-wasi --target wasm32-wasip1
out="target/wasm32-wasip1/release/zio-wasi.wasm"
[ -f "$out" ] || { echo "missing $out" >&2; exit 1; }
printf 'wasi runtime: %s (%s bytes)\n' "$out" "$(stat -c %s "$out")"