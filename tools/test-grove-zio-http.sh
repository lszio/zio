#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
test_dir="$(mktemp -d)"
server_pid=""
http_status=""
http_body=""
trap 'if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi; rm -rf "$test_dir"' EXIT

cd "$root"
cargo build -q -p zio-cli
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
bind="127.0.0.1:$port"
export GROVE_TOKEN_READER=reader-test
export GROVE_TOKEN_OPERATOR=operator-test
export GROVE_TOKEN_PUBLISHER=publisher-test

run_grove() {
  local args=()
  for arg in "$@"; do
    args+=(--args "$arg")
  done
  ./target/debug/zio-cli \
    --app apps/grove/main.zio \
    --app-root apps/grove \
    --app-share . \
    --app-root-dir "$test_dir" \
    --app-bind "$bind" \
    "${args[@]}"
}

fail() {
  printf '%s\n' "$1" >&2
  if [[ -f "$test_dir/server.log" ]]; then
    cat "$test_dir/server.log" >&2
  fi
  exit 1
}

fetch() {
  local response
  response="$(curl -sS --max-time 3 -w $'\n%{http_code}' "$@" 2>/dev/null || true)"
  http_status="${response##*$'\n'}"
  http_body="${response%$'\n'*}"
}

expect() {
  local label="$1" expected_status="$2" expected_body="$3"
  shift 3
  fetch "$@"
  if [[ "$http_status" != "$expected_status" || "$http_body" != *"$expected_body"* ]]; then
    fail "$label: expected HTTP $expected_status containing '$expected_body'; got HTTP $http_status: $http_body"
  fi
}

(
  cd apps/grove
  ../../target/debug/zio-cli web/browser-contract.zio
) || fail "Grove browser controller contract failed"

run_grove demo --case dual --root "$test_dir" >"$test_dir/demo.log" 2>&1 || fail "Grove demo store initialization failed"
run_grove serve --root "$test_dir" >"$test_dir/server.log" 2>&1 &
server_pid=$!

ready=0
for _ in {1..30}; do
  fetch "http://$bind/api/health"
  if [[ "$http_status" != 000 ]]; then
    ready=1
    break
  fi
  if ! kill -0 "$server_pid" 2>/dev/null; then
    fail "Grove server exited before accepting requests"
  fi
  sleep 1
done
[[ "$ready" == 1 ]] || fail "Grove server did not become ready"

expect "public Grove page" 200 "Grove · learning control" "http://$bind/"
expect "public browser bridge" 200 "Grove browser bridge" "http://$bind/bridge.js"
expect "public controller source" 200 "Trusted Zio sources" "http://$bind/controller-source.js"
expect "public stylesheet" 200 "--ink:" "http://$bind/styles.css"
fetch "http://$bind/favicon.ico"
[[ "$http_status" == 204 ]] || fail "public favicon: expected HTTP 204; got HTTP $http_status"
wasm_meta="$(curl -sS --max-time 3 -o "$test_dir/served-wasm" -w '%{http_code} %{content_type}' "http://$bind/wasm/zio_core_bg.wasm" 2>/dev/null || true)"
[[ "$wasm_meta" == "200 application/wasm" ]] || fail "public WASM: expected HTTP 200 application/wasm; got $wasm_meta"
cmp -s apps/site/public/wasm/zio_core_bg.wasm "$test_dir/served-wasm" || fail "public WASM body differs from the checked-in bundle"
expect "public health" 200 '"status":"ok"' "http://$bind/api/health"
expect "private read without a token" 401 '"class":"unauthorized"' "http://$bind/api/runs"
expect "reader run listing" 200 '"runs":[]' -H 'Authorization: Bearer reader-test' "http://$bind/api/runs"
expect "reader module listing without an active publication" 200 '"modules":[]' -H 'Authorization: Bearer reader-test' "http://$bind/api/modules"
expect "reader publication history" 200 '"history":[]' -H 'Authorization: Bearer reader-test' "http://$bind/api/learning/publication"
expect "dynamic path parameter decoding" 200 '"snapshot":"snapshot-1"' -H 'Authorization: Bearer reader-test' "http://$bind/api/evaluations/snapshot%2D1"
expect "literal plus preserved in path parameter" 200 '"snapshot":"snapshot+1"' -H 'Authorization: Bearer reader-test' "http://$bind/api/evaluations/snapshot+1"
expect "percent-encoded plus preserved in path parameter" 200 '"snapshot":"snapshot+1"' -H 'Authorization: Bearer reader-test' "http://$bind/api/evaluations/snapshot%2B1"
expect "known path with unsupported method" 405 '"class":"method-not-allowed"' -X POST -H 'Authorization: Bearer reader-test' -H 'Content-Type: application/json' --data '{}' "http://$bind/api/runs"
expect "reader event query" 200 '"events":[]' -H 'Authorization: Bearer reader-test' "http://$bind/api/events?after_sequence=0&limit=10"
expect "query value preserves equals after first separator" 200 '"root":"snapshot=1"' -H 'Authorization: Bearer reader-test' "http://$bind/api/lineage?root=snapshot=1"
expect "query plus decodes to space" 200 '"root":"snapshot 1"' -H 'Authorization: Bearer reader-test' "http://$bind/api/lineage?root=snapshot+1"
expect "unknown route" 404 '"class":"not_found"' -H 'Authorization: Bearer reader-test' "http://$bind/api/not-a-route"
expect "server remains available after requests" 200 '"status":"ok"' "http://$bind/api/health"

# The service must own one writable epoch and execute queued work itself.
# A 201 queue receipt is not evidence that anything ran.
health="$(curl -sS --max-time 3 "http://$bind/api/health")"
epoch="$(printf '%s' "$health" | sed -n 's/.*"owner_epoch":\([0-9]*\).*/\1/p')"
[[ -n "$epoch" && "$epoch" -ge 1 ]] || fail "service did not claim a coordinator epoch: $health"

# Health must carry the scheduler's last failure: a stalled queue that
# still answered "ok" is the failure these fields exist to prevent.
[[ "$health" == *'"scheduler_failure":null'* ]] \
  || fail "healthy service must report a null scheduler_failure: $health"
for field in owner_pid store_root schema; do
  [[ "$health" == *"\"$field\""* ]] || fail "health payload lost \"$field\": $health"
done

fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' \
  -H 'Content-Type: application/json' \
  --data '{"run_id":"run-1","task_id":"task-1","steps_budget":1,"steps":1,"kind":"training","work":{"steps":1,"graph":{}}}'
[[ "$http_status" == 201 ]] || fail "queue POST: expected HTTP 201; got HTTP $http_status: $http_body"

# Give the owner loop real time to claim, start and settle the run.
state=""
for _ in {1..60}; do
  fetch "http://$bind/api/runs/run-1" -H 'Authorization: Bearer reader-test'
  state="$(printf '%s' "$http_body" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
  case "$state" in
    running|evaluating|accepted|rejected|failed|paused|cancelled) break ;;
  esac
  sleep 0.5
done
[[ -n "$state" && "$state" != queued ]] || fail "queued run never left the queue: $http_body"

# A run the scheduler cannot claim (more steps than the grant) must not
# wedge the queue: it is failed and the next run still executes.
fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' \
  -H 'Content-Type: application/json' \
  --data '{"run_id":"run-poison","task_id":"task-1","steps_budget":1,"kind":"training","work":{"steps":2,"graph":{}}}'
[[ "$http_status" == 201 ]] || fail "poisoned queue POST: expected HTTP 201; got HTTP $http_status: $http_body"

poison_state=""
for _ in {1..60}; do
  fetch "http://$bind/api/runs/run-poison" -H 'Authorization: Bearer reader-test'
  poison_state="$(printf '%s' "$http_body" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
  [[ "$poison_state" == failed ]] && break
  sleep 0.5
done
[[ "$poison_state" == failed ]] || fail "unclaimable run was not drained: $http_body"

# The drain must not wedge the service: health still answers.
expect "server healthy after a drained run" 200 '"status":"ok"' "http://$bind/api/health"

printf 'PASS Grove Zio HTTP health, auth, query, routing and request lifecycle\n'
