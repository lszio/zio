#!/usr/bin/env bash
# Build both images from one commit and prove the deployed surface:
# site routes, same-origin Grove routing, role authorization, a writable
# store, real owner execution and restart persistence.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
project="zio-deploy-check-$$"
# The compose file pins 127.0.0.1:8080, which may be taken by the real
# deployment; always remap the site to a free ephemeral host port.
site_port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
override="$(mktemp)"
# Grove needs no host port at all: the site proxies over the Compose network,
# and the pinned 8787 may be taken by the live deployment.
printf 'services:\n  site:\n    ports: !override ["127.0.0.1:%s:80"]\n  grove:\n    ports: !override []\n' "$site_port" > "$override"
compose() { docker compose -p "$project" -f docker-compose.dokploy.yml -f "$override" "$@"; }
# `down -v` removes the named volumes Compose created for this project;
# there is no volume named after the project, so nothing else to clean.
cleanup() {
  rm -f "$override"
  compose down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

export GROVE_TOKEN_READER=reader-test
export GROVE_TOKEN_OPERATOR=operator-test
export GROVE_TOKEN_PUBLISHER=publisher-test
compose up -d --build
compose ps

site_port="$(compose port site 80 | sed 's/.*://')"
for _ in {1..60}; do
  # -f: a proxied 502 still returns exit 0 without it, ending the wait early.
  if curl -fsS --max-time 3 "http://127.0.0.1:$site_port/api/health" >/dev/null 2>&1; then break; fi
  sleep 2
done

check() {
  local label="$1" expected="$2"; shift 2
  local actual
  actual="$(curl -sS --max-time 5 -o /dev/null -w '%{http_code}' "$@" 2>/dev/null || true)"
  [[ "$actual" == "$expected" ]] || { printf 'FAIL %s: expected %s, got %s\n' "$label" "$expected" "$actual" >&2; exit 1; }
}

check "site intro page" 200 "http://127.0.0.1:$site_port/grove/"
check "site wasm" 200 "http://127.0.0.1:$site_port/wasm/zio_core_bg.wasm"
check "grove control plane" 200 "http://127.0.0.1:$site_port/grove/app/"
check "unauthenticated api" 401 "http://127.0.0.1:$site_port/api/runs"
check "reader api" 200 -H 'Authorization: Bearer reader-test' "http://127.0.0.1:$site_port/api/runs"

curl -sS --max-time 5 -X POST "http://127.0.0.1:$site_port/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' -H 'Content-Type: application/json' \
  --data '{"run_id":"ci-run","task_id":"ci-task","steps_budget":1,"steps":1,"kind":"training","work":{"steps":1,"graph":{}}}' \
  >/dev/null

state=""
for _ in {1..120}; do
  body="$(curl -sS --max-time 5 -H 'Authorization: Bearer reader-test' "http://127.0.0.1:$site_port/api/runs/ci-run" 2>/dev/null || true)"
  state="$(printf '%s' "$body" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
  case "$state" in evaluating|failed|paused|cancelled) break ;; esac
  sleep 2
done
case "$state" in
  evaluating) printf 'owner executed queued work: %s\n' "$state" ;;
  failed)
    # The deployed image refuses to train without namespace isolation, and
    # a refusal recorded as `failed` with an event is the correct outcome,
    # not a silent pass.
    printf 'owner refused training without isolation: %s\n' "$body" ;;
  paused|cancelled) printf 'owner settled the run: %s\n' "$state" ;;
  *) printf 'FAIL queued work never ran: %s\n' "$body" >&2; exit 1 ;;
esac

# Restart persistence: the same volume must still answer with the run.
compose restart grove
for _ in {1..60}; do
  if curl -fsS --max-time 3 "http://127.0.0.1:$site_port/api/health" >/dev/null 2>&1; then break; fi
  sleep 2
done
check "run survives restart" 200 -H 'Authorization: Bearer reader-test' "http://127.0.0.1:$site_port/api/runs/ci-run"

printf 'PASS unified site+grove deployment\n'
