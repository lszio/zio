#!/usr/bin/env bash
# Artifact smoke: save in one process, inspect and run in another, from
# outside the source tree, with grants. Proves the image carries no
# authority (a canary file and an env token do not travel), that restore
# needs a grant to touch the filesystem, and that a granted file is
# readable through the restored context.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

fail() {
  printf 'FAIL %s\n' "$1" >&2
  exit 1
}

cargo build -q -p zio-cli
bin="$root/target/debug/zio-cli"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/granted"
printf 'granted payload' > "$work/granted/resource.txt"
printf 'top secret canary\n' > "$work/canary-secret.txt"

# ── Process A: save ──────────────────────────────────────────────
token='super-secret-token'
ZIO_SESSION_TOKEN="$token" "$bin" image save \
  examples/toolchain/image-session.zio \
  -o "$work/session.zio-image" \
  || fail 'image save failed'
grep -q "$token" "$work/session.zio-image" \
  && fail 'env token leaked into the image'
grep -q 'canary-secret' "$work/session.zio-image" \
  && fail 'canary path leaked into the image'

# ── Process B: inspect (metadata only) ───────────────────────────
inspect_out="$(cd "$work" && "$bin" image inspect "$work/session.zio-image")" \
  || fail 'image inspect failed'
case "$inspect_out" in
  *"format: zio-image"*) ;;
  *) fail "inspect did not report the format: $inspect_out" ;;
esac
case "$inspect_out" in
  *"root: root-0"*) ;;
  *) fail "inspect did not report the root: $inspect_out" ;;
esac
case "$inspect_out" in
  *"module: session"*) ;;
  *) fail "inspect did not report the module: $inspect_out" ;;
esac
printf 'inspect ok\n'

# ── Process B: run outside the source tree, empty module roots ────
run_out="$(cd "$work" && "$bin" image run "$work/session.zio-image" \
  --eval '(do ((get root-0 :bump)) ((get root-0 :get)))')" \
  || fail 'image run failed'
[ "$run_out" = "1" ] || fail "restored shared binding did not carry: $run_out"

canary_out="$(cd "$work" && "$bin" image run "$work/session.zio-image" \
  --eval "(slurp \"$work/canary-secret.txt\")" 2>&1)" \
  && fail 'canary file was readable without a grant'
case "$canary_out" in
  *"outside every granted root"*) ;;
  *) fail "canary refusal did not name the grant boundary: $canary_out" ;;
esac

granted_out="$(cd "$work" && "$bin" image run "$work/session.zio-image" \
  --grant-dir "$work/granted" \
  --eval "(slurp \"$work/granted/resource.txt\")")" \
  || fail 'granted file was not readable'
[ "$granted_out" = '"granted payload"' ] \
  || fail "granted resource returned: $granted_out"

# ── compile to a portable module ─────────────────────────────────
"$bin" compile examples/toolchain/semantics.zio -o "$work/semantics.zioc" \
  || fail 'compile failed'
[ -s "$work/semantics.zioc" ] || fail 'compiled module is empty'

# ── bundled applications ─────────────────────────────────────────
# Source app: static output plus explicit dynamic eval, run with no zio
# or rustc on PATH and no checkout in sight.
"$bin" build examples/toolchain/app.zio --target native -o "$work/app" \
  || fail 'bundle build failed'
app_out="$(cd "$work" && env -i PATH=/usr/bin:/bin HOME="$HOME" ./app)" \
  || fail 'bundled app failed'
case "$app_out" in
  *"49"*) ;;
  *) fail "bundled app did not print 49: $app_out" ;;
esac
case "$app_out" in
  *"50"*) ;;
  *) fail "bundled app dynamic eval did not print 50: $app_out" ;;
esac

# Image app: the entry restores the still image and keeps its state.
"$bin" build "$work/session.zio-image" --target native -o "$work/img-app" \
  --entry '((get root-0 :bump))' || fail 'image bundle build failed'
img_out="$(cd "$work" && env -i PATH=/usr/bin:/bin HOME="$HOME" ./img-app)" \
  || fail 'bundled image app failed'
[ "$img_out" = "1" ] || fail "bundled image app returned: $img_out"

# Damaged payload: one flipped byte must refuse the run by name.
cp "$work/app" "$work/broken-app"
python3 - "$work/broken-app" <<'PY'
import sys
path = sys.argv[1]
data = bytearray(open(path, 'rb').read())
data[len(data) - 36 - 8] ^= 0xff
open(path, 'wb').write(data)
PY
broken_out="$(cd "$work" && env -i PATH=/usr/bin:/bin HOME="$HOME" ./broken-app 2>&1)" \
  && fail 'damaged payload ran'
case "$broken_out" in
  *"checksum mismatch"*) ;;
  *) fail "damage was not named: $broken_out" ;;
esac

printf 'PASS toolchain artifacts: save/inspect/run, grants, canary, compile, bundled apps\n'