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
export GROVE_TOKEN_ANNOTATOR=annotator-test
export GROVE_TOKEN_OPERATOR=operator-test
export GROVE_TOKEN_PUBLISHER=publisher-test

run_grove() {
  local args=()
  for arg in "$@"; do
    args+=(--args "$arg")
  done
  local python_path="${GROVE_TENSOR_PYTHON:-$root/.venv/bin/python}"
  local worker_script="${GROVE_WORKER_SCRIPT:-$root/apps/grove/workers/torch/worker.py}"
  [[ "$python_path" == /* ]] || python_path="$root/$python_path"
  [[ "$worker_script" == /* ]] || worker_script="$root/$worker_script"
  local py_real base_dir alias_dir mount_dir
  py_real="$(readlink -f "$python_path")"
  # The venv python symlinks through uv's alias directory, so both the
  # real install tree and its parent alias directory are granted: the
  # interpreter is exec'd by the un-resolved path inside the jail.
  base_dir="$(readlink -f "$(dirname "$py_real")/..")"
  alias_dir="$(readlink -f "$base_dir/..")"
  # Anything under /usr is covered by the service's whole-/usr grant; a
  # duplicate or nested mount target is refused by the jail.
  mount_dir() {
    case "$1" in /usr|/usr/*|/) return ;; esac
    MOUNTS+=(--app-worker-mount "$1")
  }
  local MOUNTS=()
  mount_dir "$(cd "$(dirname "$worker_script")" && pwd)"
  mount_dir "$(readlink -f "$(dirname "$python_path")/..")"
  # The alias directory contains the real install tree, so granting it
  # covers both; fall back to the base tree when the alias is /usr-side.
  case "$alias_dir" in
    /|/usr|/usr/*) mount_dir "$base_dir" ;;
    *) mount_dir "$alias_dir" ;;
  esac
  "$root/target/debug/zio-cli" \
    --app "$root/apps/grove/main.zio" \
    --app-root "$root/apps/grove" \
    --app-share "$root" \
    --app-web-root "$root/apps/grove/web" \
    --app-root-dir "$test_dir" \
    --app-bind "$bind" \
    --app-worker-script "$worker_script" \
    --app-worker-python "$python_path" \
    "${MOUNTS[@]}" \
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
# The site owns /wasm/*; the Grove service must not duplicate it.
expect "Grove must not serve the wasm loader" 404 '"class":"not_found"' \
  -H 'Authorization: Bearer reader-test' "http://$bind/wasm/zio_core.js"
expect "Grove must not serve WASM any more (site does)" 404 '"class":"not_found"' \
  -H 'Authorization: Bearer reader-test' "http://$bind/wasm/zio_core_bg.wasm"
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

# Run-1 carries a real GVD1 split and a real minimal graph: the worker
# must train tensors, not die on a placeholder frame.
training_data="$test_dir/training-data.gvd1"
python3 - "$training_data" <<'PYGEN'
import json, struct, sys
path = sys.argv[1]
sample = struct.Struct("<IBBBBff")
pixels = bytes(256)
rows = b""
for i in range(64):
    label = i % 2
    rows += sample.pack(i, 1, 1, label, 0, 1.0 if label else -1.0, 0.5) + pixels
header = json.dumps({"record_bytes": sample.size + 256}).encode()
blob = b"GVD1" + struct.pack("<I", len(header)) + header + rows
open(path, "wb").write(blob)
PYGEN
graph='{"inputs":{"x":{"shape":[258],"space":"generic"}},"ops":[{"kind":"linear","inputs":["x"],"output":"logits","attrs":{"out":2}}],"outputs":{"logits":"logits"},"trainable":["logits"]}'
fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' \
  -H 'Content-Type: application/json' \
  --data "{\"run_id\":\"run-1\",\"task_id\":\"task-1\",\"steps_budget\":8,\"steps_consumed\":0,\"steps\":8,\"kind\":\"training\",\"work\":{\"steps\":8,\"seed\":1,\"graph\":$graph,\"data\":\"$training_data\",\"val_data\":\"$training_data\"}}"
[[ "$http_status" == 201 ]] || fail "queue POST: expected HTTP 201; got HTTP $http_status: $http_body"

# Authority comes from the token that made the request, not from the
# operator handle the service holds: under a writer handle a reader token
# would otherwise be an operator.
expect "reader cannot enqueue" 403 '"class":"capability-denied"' -X POST \
  -H 'Authorization: Bearer reader-test' -H 'Content-Type: application/json' \
  --data '{"run_id":"run-reader","task_id":"task-1","steps_budget":1,"operation_id":"op-reader-queue"}' \
  "http://$bind/api/learning/queue"

# An annotator may file a correction and must be able to record the
# receipt for it: the route requires only the annotator role, so a stricter
# gate on receipt recording would refuse the annotator on its own path
# AFTER the signal was already filed.
expect "reader cannot file a signal" 403 '"class":"capability-denied"' -X POST \
  -H 'Authorization: Bearer reader-test' -H 'Content-Type: application/json' \
  --data '{"id":"corr-reader","kind":"human-correction","operation_id":"op-sig-reader","target_field":"state"}' \
  "http://$bind/api/signals"
signal_body='{"schema":1,"id":"corr-1","idempotency_key":"corr-1","kind":"human-correction","target_field":"state","content":"clear","task_id":"task-1","received_at_ms":1,"operation_id":"op-sig-ann"}'
# A signal without an idempotency key would violate a NOT NULL column the
# store inserts into; it must be refused, not answered as if it were stored.
expect "signal without idempotency key is refused" 400 '"class":"invalid-input"' -X POST \
  -H 'Authorization: Bearer annotator-test' -H 'Content-Type: application/json' \
  --data '{"schema":1,"id":"corr-nokey","kind":"human-correction","target_field":"state","content":"clear","task_id":"task-1","received_at_ms":1}' \
  "http://$bind/api/signals"
fetch -X POST "http://$bind/api/signals" \
  -H 'Authorization: Bearer annotator-test' -H 'Content-Type: application/json' --data "$signal_body"
[[ "$http_status" == 201 && "$http_body" == *'"id":"corr-1"'* ]] \
  || fail "annotator signal POST: expected HTTP 201 naming corr-1; got HTTP $http_status: $http_body"
# The replay must come back from the ledger the annotator wrote, not from a
# second write: same answer, recorded by the annotator's own token.
expect "annotator signal replay" 201 '"id":"corr-1"' -X POST \
  -H 'Authorization: Bearer annotator-test' -H 'Content-Type: application/json' --data "$signal_body" \
  "http://$bind/api/signals"

# An operation id means one thing. The identical replay returns the first
# answer verbatim; a replay that differs under the same id is refused
# rather than quietly producing a second answer.
replay_body='{"run_id":"run-2","task_id":"task-1","steps_budget":1,"kind":"training","work":{"steps":1,"graph":{}},"operation_id":"op-replay-queue"}'
fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' -H 'Content-Type: application/json' --data "$replay_body"
first_status="$http_status"
first_body="$http_body"
[[ "$first_status" == 201 ]] || fail "replay queue POST: expected HTTP 201; got HTTP $first_status: $first_body"
fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' -H 'Content-Type: application/json' --data "$replay_body"
[[ "$http_status" == "$first_status" && "$http_body" == "$first_body" ]] \
  || fail "identical replay changed the answer: first HTTP $first_status $first_body, replay HTTP $http_status $http_body"
fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' -H 'Content-Type: application/json' \
  --data '{"run_id":"run-2-other","task_id":"task-1","steps_budget":9,"kind":"training","work":{"steps":1,"graph":{}},"operation_id":"op-replay-queue"}'
[[ "$http_status" == 409 && "$http_body" == *'"class":"conflict"'* ]] \
  || fail "conflicting replay under one operation id: expected HTTP 409 conflict; got HTTP $http_status: $http_body"
# The refusal must leave the FIRST answer standing: a second writer whose
# insert was ignored must not have replaced the stored receipt, or the id
# would now mean two things and a later replay would answer the wrong one.
fetch -X POST "http://$bind/api/learning/queue" \
  -H 'Authorization: Bearer operator-test' -H 'Content-Type: application/json' --data "$replay_body"
[[ "$http_status" == "$first_status" && "$http_body" == "$first_body" ]] \
  || fail "a refused conflicting replay changed the stored answer: HTTP $http_status $http_body"

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

# A worker that cannot be isolated must refuse training honestly rather
# than report a completed run, and a worker that can must leave a named
# snapshot whose content-addressed params artifact round-trips.
if ! command -v unshare >/dev/null 2>&1 || ! unshare -Urn --pid --mount --fork true >/dev/null 2>&1; then
  printf 'SKIP: namespaces unavailable; CPU training not exercised on this host\n' >&2
  for _ in {1..60}; do
    fetch "http://$bind/api/runs/run-1" -H 'Authorization: Bearer reader-test'
    state="$(printf '%s' "$http_body" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
    [[ "$state" == failed ]] && break
    sleep 0.5
  done
  [[ "$state" == failed ]] || fail "without namespaces the run must fail honestly, not succeed: $state"
else
  for _ in {1..120}; do
    fetch "http://$bind/api/runs/run-1" -H 'Authorization: Bearer reader-test'
    state="$(printf '%s' "$http_body" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
    case "$state" in evaluating|failed) break ;; esac
    sleep 0.5
  done
  [[ "$state" == evaluating ]] || fail "isolated worker did not reach evaluating: $state"
  fetch "http://$bind/api/events?run_id=run-1&limit=100" -H 'Authorization: Bearer reader-test'
  [[ "$http_body" == *'"kind":"worker-started"'* ]] \
    || fail "worker started event not recorded for run-1: $http_body"
  trained_params="$(python3 - "$test_dir" <<'PYART'
import hashlib, pathlib, sqlite3, sys
db = sqlite3.connect(sys.argv[1] + "/grove.db")
row = db.execute(
    "SELECT digest FROM named_snapshots WHERE name='trained-run-1'").fetchone()
hex_ = row[0].split(":")[-1]
# Content-addressed round-trip: the stored object must exist and hash
# under grove--digest (sha256 of "grove-artifact-v1\\0" + bytes) to its
# own digest, i.e. artifact--get returns exactly what was put.
obj = pathlib.Path(sys.argv[1]) / "artifacts" / "objects" / hex_[:2] / hex_
if not obj.is_file():
    print(f"missing object for {hex_}")
else:
    blob = obj.read_bytes()
    got = hashlib.sha256(b"grove-artifact-v1\x00" + blob).hexdigest()
    if got != hex_:
        print(f"corrupt object {hex_}: hashed {got}")
    else:
        print(f"round-trip ok sha256:{hex_} bytes={len(blob)} params={blob[:120].decode(errors='replace')}")
PYART
)"
  [[ "$trained_params" == "round-trip ok"* ]] \
    || fail "trained params artifact does not round-trip: $trained_params"
  printf 'trained-run-1 %s\n' "$trained_params"
fi

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

# The owner writes the run's execution events with a per-run sequence of
# its own; a settled run must leave a readable trail.
expect "run events recorded by the owner" 200 '"events"' -H 'Authorization: Bearer reader-test' \
  "http://$bind/api/events?run_id=run-1&limit=100"
fetch "http://$bind/api/events?run_id=run-1&limit=100" -H 'Authorization: Bearer reader-test'
[[ "$http_body" == *'"sequence":'* ]] \
  || fail "owner recorded no sequenced event for run-1: $http_body"

printf 'PASS Grove Zio HTTP health, auth, query, routing and request lifecycle\n'
