#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 || "$1" != "--output" || -z "$2" ]]; then
  printf 'usage: %s --output PATH\n' "$0" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="$2"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
cd "$root"

printf '%s\n' \
  '(loop [i 0 total 0]' \
  '  (if (= i 10000) total (recur (inc i) (+ total i))))' \
  > "$work_dir/sum-loop.zio"
printf '%s\n' \
  '(defn identity-loop [i]' \
  '  (if (= i 10000) i (identity-loop (inc i))))' \
  '(identity-loop 0)' \
  > "$work_dir/function-call.zio"
printf '%s\n' \
  '(loop [i 0 xs (list 1 2 3 4 5 6 7 8 9 10)]' \
  '  (if (= i 1000) xs (recur (inc i) (map inc xs))))' \
  > "$work_dir/map-loop.zio"

cargo build --release --bin zio-cli
binary="$root/target/release/zio-cli"
measure() {
  local name="$1" program="$2" elapsed
  local TIMEFORMAT='%R'
  { time "$binary" "$program" >/dev/null; } 2> "$work_dir/$name.time"
  elapsed="$(tr -d '\n' < "$work_dir/$name.time")"
  case "$elapsed" in
    *[!0-9.]*|'') printf 'invalid elapsed value for %s: %s\n' "$name" "$elapsed" >&2; exit 1 ;;
  esac
  printf '%s' "$elapsed"
}

sum_loop="$(measure sum-loop "$work_dir/sum-loop.zio")"
function_call="$(measure function-call "$work_dir/function-call.zio")"
map_loop="$(measure map-loop "$work_dir/map-loop.zio")"
mkdir -p "$(dirname "$output")"
exec > "$output"
printf '{\n'
printf '  "engine": "ast",\n'
printf '  "command": "target/release/zio-cli PROGRAM",\n'
printf '  "revision": "%s",\n' "$(git rev-parse HEAD)"
printf '  "machine": "%s",\n' "$(uname -srm)"
printf '  "units": "seconds",\n'
printf '  "benchmarks": {\n'
printf '    "sum-loop": %s,\n' "$sum_loop"
printf '    "function-call": %s,\n' "$function_call"
printf '    "map-loop": %s\n' "$map_loop"
printf '  }\n'
printf '}\n'
