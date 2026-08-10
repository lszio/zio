#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_dir="$(mktemp -d)"
trap 'rm -rf "$test_dir"' EXIT
output="$test_dir/failed-workload.json"

if ZIO_AST_BASELINE_BINARY=/bin/false \
  "$root/tools/record-ast-baseline.sh" --output "$output"; then
  printf 'expected recorder to fail when a workload process fails\n' >&2
  exit 1
fi

if [[ -e "$output" ]]; then
  printf 'recorder created an artifact after a workload process failed\n' >&2
  exit 1
fi
