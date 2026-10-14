#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$(mktemp -d "${TMPDIR:-/tmp}/zio-self-build.XXXXXX")"
trap 'rm -rf -- "$out"' EXIT
cd "$root"
cargo run -p zio-cli -- self-build --verify --out-dir "$out"
cmp "$out/stage-1.json" "$out/stage-2.json"
cmp "$out/stage-2.json" "$out/stage-3.json"
printf 'Portable compiler stages are byte-identical.\n'
