# Unified Zio/Grove Deployment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the canonical Zio Grove app a real service — one owner epoch that executes queued work against a writable store — then package it and the Astro site as two Compose services with same-origin routing, dev and isolated MR preview deployments, and CI that proves the whole path.

**Architecture:** `apps/grove/api.zio` currently opens a read-only store and never drives `apps/grove/runner.zio`, so it answers `202 queued` while nothing runs; `apps/grove/runner.zio` also references five symbols that do not exist anywhere in the repository (`events--append`, `learn--decode-gvd1`, `inference--commit-trained`, `host/fs-resolve`, `host/fs-read`, `host/process-id`, `host/poll`). The service loop, runner, and asset resolution are fixed first; then `Dockerfile.grove` plus `docker-compose.dokploy.yml` gain a `grove` service behind the existing Nginx (`site`); then CI, `dev` coverage, and a restricted preview workflow finish the lifecycle.

**Tech Stack:** Zio (`.zio`) application and HTTP/runner code; Rust `zio-cli` + `contribs/native/host` (`zio-host`) primitives; Python CPU worker `apps/grove/workers/torch/worker.py`; Docker/Docker Compose; Nginx; GitHub Actions; Dokploy Compose API.

**Design spec:** `docs/superpowers/specs/2026-10-08-unified-zio-grove-runtime-design.md` (approved 2026-10-08).

**Out of scope:** Loom ACP client/server, new teacher providers, new model families, and the remaining Grove business migration already tracked by `docs/superpowers/plans/2026-10-05-zio-grove-convergence.md`. This plan makes the *current* canonical entry a real deployed service; it does not claim those features exist.

---

## Baseline facts (verified 2026-10-08, do not re-derive)

- Canonical entry: `tools/install.sh:73-108` writes `bin/grove`, which execs `bin/zio --app .../apps/grove/main.zio`. `apps/grove/native/app` is the legacy Rust service and is **not** the deployment target.
- `apps/grove/main.zio:178-184` `grove--serve-handler` calls `api--check-bind` then `api--serve bind root`.
- `apps/grove/api.zio:629-659` `api--serve` opens `store--open root {:id "service" :role "reader"}` and loops on `host/http-next`; it never calls `runner--open`/`runner--tick`.
- `apps/grove/store.zio:152-169`: a reader store is `read-only`, refuses DDL, and cannot be reopened as a writer.
- `apps/grove/runner.zio` references undefined symbols (verified by grep over `libs/` and `apps/`, and by `target/debug/zio-cli --app` probe): `events--append` (line 12), `learn--decode-gvd1` (83, 85), `inference--commit-trained` (154), `host/fs-resolve` (73), `host/fs-read` (75), `host/process-id` (coordinator.zio:23), `host/poll` (203).
- Host primitives that **do** exist: `host/read-bytes`, `host/write-atomic`, `host/stat-path`, `host/canonical-path`, `host/list-dir`, `host/mkdir`, `host/remove`, `host/rename`, `host/http-listen|http-next|http-reply|http-close|http-readable`, `host/process-start|read|readable|write|kill|wait`, `host/db-*`, `host/tensor-*`, `host/evaluate-isolated`, `host/sleep-ms`, `host/sha256`, `host/random-bytes`, `host/clock-ms`, `host/getenv`, `host/argv`.
- The `target/debug/zio` symlink/copy is stale relative to `target/debug/zio-cli`; always run `cargo build -p zio-cli` and invoke `target/debug/zio-cli`.
- `host/http-listen` config keys: `address`, `timeout-ms`, `max-body-bytes`, `max-header-bytes`, `max-queue`. `host/process-start` config keys: `executable`, `argv`, `environment`, `cwd`, `scratch`, `read-only-mounts`, `framing`, `identity`, `timeout-ms`, `write-timeout-ms`, `max-frame-bytes`, `max-output-bytes`, `max-stderr-bytes`, `max-address-space`, `max-cpu-seconds`, `max-processes`.
- Worker protocol (`apps/grove/workers/torch/worker.py:9-16`): in `train` frame with `graph`, `data`, `weights`, `steps`, `seed`, `out`; out frames are `progress` / `checkpoint` / `done` (`weights` = output file path) / `failed`, each carrying `run_id` and `attempt_id`.
- `tools/test-grove-zio-http.sh` is the existing real-service HTTP harness; it builds `-p zio-cli` and uses `target/debug/zio-cli` with `--app-root apps/grove --app-share .`.
- `.github/workflows/ci.yml:3-7` triggers `push` on `main` only, `pull_request` on all, `workflow_dispatch`.
- `apps/site/content/grove/index.mdx` is the existing `/grove/` introduction page; `apps/grove/web/index.html:7` links `styles.css` relatively and `:224` links `/bridge.js` absolutely; `apps/grove/web/bridge.js:18` imports `/wasm/zio_core.js` and `:427` calls `wasmInit('/wasm/zio_core_bg.wasm')`.
- The worker jail (`contribs/native/host/src/transport/isolation.rs:160-192`) builds `unshare -Urnm --pid --fork --kill-child=KILL`, bind-mounts `scratch` at `/scratch`, `pivot_root`s everything else away, and finally `exec`s `executable` under `env -i`. Consequences: `executable` is the interpreter (never the `.py` file), a host path is not visible inside the worker unless it is under `scratch`, and the frame must use `/scratch/...` paths.
- `host/http-next` returns `nil` on timeout (`transport/http.rs:229`), so the service loop can poll before each blocking read; `host/process-readable` is the non-blocking worker check (`transport/process.rs:650-661`).
- `store--as` (`apps/grove/store.zio:1048-1050`) rebinds a request's store to the request principal, so per-request authorization still works under a writer service handle — provided handlers check the request rather than the handle.
- `codec--fields "ModelSnapshot"` (`apps/grove/codec.zio:29`) is the fixed field list: `schema owner entrypoint params libraries preprocessing_version modules ensemble graph`. `training--commit-snapshot` must produce exactly those keys and `ParamRef` entries (`module shape dtype artifact`) or `codec--encode` drops fields it does not know.

---

## Task 1: Grove service owns one writable epoch and drives the runner

**Files:**
- Modify: `apps/grove/api.zio:629-669` (`api--serve`, add `api--service-config`, `api--owner-store`)
- Modify: `apps/grove/main.zio:178-184` (`grove--serve-handler` passes the launcher config map)
- Test: `tools/test-grove-zio-http.sh` (extend with owner-epoch and write-path assertions)

- [ ] **Step 1: Write the failing test**

Append to `tools/test-grove-zio-http.sh`, right after the existing `expect "server remains available after requests" ...` line:

```bash
# The service must own one writable epoch and execute queued work itself.
# A 201 queue receipt is not evidence that anything ran.
health="$(curl -sS --max-time 3 "http://$bind/api/health")"
epoch="$(printf '%s' "$health" | sed -n 's/.*"owner_epoch":\([0-9]*\).*/\1/p')"
[[ -n "$epoch" && "$epoch" -ge 1 ]] || fail "service did not claim a coordinator epoch: $health"

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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `bash tools/test-grove-zio-http.sh`
Expected: FAIL with `service did not claim a coordinator epoch: ...` — the current `api--serve` opens a reader store, `coordinator--owner` returns `{:epoch 0}` and no runner runs.

- [ ] **Step 3: Open the service store as a writer and own the epoch**

In `apps/grove/api.zio`, replace the `api--serve` body (lines 629-659) with:

```zio
(defn api--service-config [config]
  ;; Everything the owner needs comes from the launcher's keyword map, so
  ;; the service reads no environment and no argv to decide its own
  ;; authority. Missing values fall back to the smallest workable loop.
  {:worker-count (get config :workers 2)
   :lease-ms 30000
   :deadline-ms 300000
   :max-frame-bytes 16777216
   :input-root (get config :root "")
   :web-root (get config :web-root "")
   :worker-config (api--worker-config config)})

(defn api--tick [runner]
  ;; One owner, one attempt at failure. A tick that raises must not take
  ;; the HTTP surface down with it: the run is already recorded as
  ;; running, and a dead service reports nothing at all.
  (try (runner--tick runner)
       (catch any
         (do (host/sleep-ms 50) runner))))

(defn api--serve [bind root config]
  (let* [tokens (api--load-tokens)
         listener (host/http-listen {:address bind :timeout-ms 30000})
         ;; One connection, one principal. The service writes: a reader
         ;; handle cannot create the schema, cannot claim an epoch and
         ;; cannot accept a queue POST.
         store (store--open root {:id "service" :role "operator"})
         runner (runner--open store (api--service-config config))]
    (loop [owner {:listener listener :store store :tokens tokens
                  :runner runner :config config}]
      ;; Poll before blocking so queued work advances while no request is
      ;; in flight, and while one is: a single tick per loop iteration is
      ;; the whole scheduler, and HTTP wait must not starve it.
      (let [next (assoc owner :runner (api--tick (get owner :runner)))]
        (let [raw (host/http-next (get owner :listener) 200)]
          (if (nil? raw)
            (recur next)
            (let* [method (get raw :method "GET")
                   path (get raw :path "/")
                   token (api--bearer (get raw :headers {}))
                   role (if token (get (get next :tokens) token) nil)
                   principal (if role {:id (str "token:" (host/sha256 (host/bytes token))) :role role} nil)
                   body-bytes (get raw :body (host/bytes "{}"))
                   request {:method method
                            :path path
                            :headers (get raw :headers {})
                            :body_bytes body-bytes
                            :query_raw (get raw :query "")
                            :store (store--as (get (get next :store) :db) (get (get next :store) :root) principal)
                            :tokens (get next :tokens)
                            :principal principal
                            :listener (get next :listener)
                            :id (get raw :id)}]
              (if (and (nil? role) (not (api--public-path path)))
                (api--error request "unauthorized" "missing or unknown bearer token" nil)
                (try
                  (api--route request)
                  (catch any
                    (api--error request "backend-failed" (str *error*) nil))))
              (recur next)))))))
```

Keep `api--check-bind` unchanged (lines 661-669) so an unauthenticated routable bind is still refused.

Two authorization consequences of the writer handle, both fixed in Task 2 Step 7. A request's store is rebound to the token's role (`store--as`), so a reader token still cannot write — but `api--handle-queue` currently trusts the *handle*'s role for its own check, so under a writer handle the check is a no-op. Every mutating handler must check the **request principal**; that is Task 2's Step 7.

- [ ] **Step 4: Pass the launcher config map to the serve handler**

In `apps/grove/main.zio`, replace `grove--serve-handler` (lines 178-184):

```zio
(def grove--serve-handler [parsed]
  (let [opts (get parsed :options)
        bind (get opts :bind "127.0.0.1:8787")
        config (get parsed :launcher {})
        ;; `--root` wins over the launcher's `--app-root-dir` because it is
        ;; the operator's per-invocation decision; the launcher value stays
        ;; the default for the installed service.
        serve-root (if (= (get opts :root "") "") (get config :root "") (get opts :root))]
    (load "api.zio")
    (api--check-bind bind (count (api--load-tokens)))
    (api--serve bind serve-root config)))
```

`grove--main` (line 209) already merges the launcher config into `parsed :options` before dispatch. `rill-dispatch` takes exactly two arguments (`libs/rill/cli.zio:120-125`) and calls `(handler parsed)`, so the handler cannot receive `config` directly: pass the config through the parsed options map instead, keyed so it cannot collide with a CLI flag.

```zio
(def grove--serve-handler [parsed]
  (let [opts (get parsed :options)
        bind (get opts :bind "127.0.0.1:8787")
        config (get opts :launcher {})
        ;; `--root` wins over the launcher's `--app-root-dir` because it is
        ;; the operator's per-invocation decision; the launcher value stays
        ;; the default for the installed service.
        serve-root (if (= (get opts :root "") "") (get config :root "") (get opts :root))]
    (load "api.zio")
    (api--check-bind bind (count (api--load-tokens)))
    (api--serve bind serve-root config)))
```

and in `grove--main`, put the launcher map under that key before dispatch:

```zio
(let* [parsed (assoc parsed :options opts)
       parsed (assoc parsed :launcher config)]
  ...)
```

Callers unaffected: every other handler still takes `parsed` alone.

- [ ] **Step 5: Run the test to observe the remaining defect**

Run: `bash tools/test-grove-zio-http.sh`
Expected: FAIL, and the failure is the next real defect rather than a test artifact. The epoch assertion passes (the writer store let `runner--open` claim epoch 1) and the queue POST returns 201, then the **service process exits**: `runner--start` fails on the missing worker config, the handler calls `runner--finish`, and `runner--finish` → `runner--event` → the undefined `events--append` raises. That error is not caught by the `catch` in `runner--tick` (`apps/grove/runner.zio:193-197` guards only `runner--start`), so it escapes the service loop. Read `server.log`: it must show `symbol not found: events--append`. A health 404 or a stuck `queued` run instead means the loop never ticked, which is a bug in this task's own code.

- [ ] **Step 6: Commit**

```bash
git add apps/grove/api.zio apps/grove/main.zio tools/test-grove-zio-http.sh
git commit -m "feat(grove): serve with a writable store and one owner epoch"
```

---

## Task 2: Replace the runner's undefined symbols with real operations

**Files:**
- Create: `apps/grove/events.zio` (event append wrapper used by the runner)
- Create: `apps/grove/training.zio` (`training--commit-snapshot`, GVD1 decode via the learning library)
- Modify: `apps/grove/runner.zio:1-204`
- Modify: `apps/grove/coordinator.zio:19-26` (drop `host/process-id`)
- Test: `tools/test-grove-zio-http.sh` (assert terminal state and events)

- [ ] **Step 1: Extend the test to require a settled state and events**

In `tools/test-grove-zio-http.sh`, replace the polling loop added in Task 1 with:

```bash
# `evaluating` is the settled state for a completed training attempt:
# publication is a separate, human-gated API call, so the owner stops
# there. `failed` is also settled and must be honest about why.
state=""
for _ in {1..120}; do
  fetch "http://$bind/api/runs/run-1" -H 'Authorization: Bearer reader-test'
  state="$(printf '%s' "$http_body" | sed -n 's/.*"state":"\([^"]*\)".*/\1/p')"
  case "$state" in
    evaluating|failed|paused|cancelled) break ;;
  esac
  sleep 0.5
done
case "$state" in
  evaluating|failed|paused|cancelled) ;;
  *) fail "run never left queued/running: $http_body" ;;
esac
if [[ "$state" == failed ]]; then
  printf 'NOTE run-1 settled as failed: %s\n' "$http_body" >&2
fi

expect "run events recorded by the owner" 200 '"events"' -H 'Authorization: Bearer reader-test' \
  "http://$bind/api/events?run_id=run-1&limit=100"
expect "reader cannot enqueue" 403 '"class":"capability-denied"' -X POST \
  -H 'Authorization: Bearer reader-test' -H 'Content-Type: application/json' \
  --data '{"run_id":"run-2","task_id":"task-2","steps_budget":1}' \
  "http://$bind/api/learning/queue"
```

A run that settles as `failed` on the first attempt is expected before Task 3 exists: there is no worker configuration yet, so `runner--start` cannot spawn a process. The point of this step is that the state and the events are real and readable — not that training succeeds.

- [ ] **Step 2: Run to verify the new assertions fail**

Run: `bash tools/test-grove-zio-http.sh`
Expected: FAIL on `reader cannot enqueue`. The queue handler runs under a **writer** store handle (`api--serve` binds `:role "operator"`), so a reader token now passes `grove--authorize` and is refused only by the handler's own check — which it does not have. That refusal is the point: authorization must come from the token in the request, not from the handle the service happens to hold.

- [ ] **Step 3: Add the event wrapper**

Create `apps/grove/events.zio`:

```zio
;; The runner writes execution events; `store--append-events` owns the
;; cap, the schema check and the append-only insert. This wrapper only
;; allocates the next sequence for a run, because the primary key is
;; (run_id, sequence) and the store deliberately does not choose it.
(load "store.zio")

(defn events--next-sequence [store run-id]
  (let [rows (store--query store "SELECT COALESCE(MAX(sequence),0) AS last FROM execution_events WHERE run_id=?1" [run-id])]
    (+ (grove--get-key (first rows) :last 0) 1)))

(defn events--append [store event]
  (let [run-id (str (grove--get-key event :run_id ""))
        record (assoc event :sequence (events--next-sequence store run-id))]
    (store--append-events store [record])
    record))
```

- [ ] **Step 4: Add the training commit path**

Create `apps/grove/training.zio`:

```zio
;; Committing a trained run means storing the bytes the worker actually
;; produced and refusing anything that is not a model. The Rust oracle is
;; `commit_trained_snapshot` (apps/grove/native/app/src/demo.rs:234).
(load "artifacts.zio")
(load "store.zio")

(defn training--params [bytes]
  (let [parsed (codec--normalize (json-parse bytes))]
    (grove--require (map? parsed) "backend-failed" "trained weights are not JSON")
    (let [params (get parsed :params nil)]
      (grove--require (map? params) "backend-failed" "trained weights carry no `params` object: this is not a model")
      (grove--require (> (count params) 0) "backend-failed" "trained weights carry no layers: an empty model is not a candidate")
      (reduce (fn [_ layer]
                (let [entry (get params layer nil)]
                  (grove--require (and (map? entry) (not (nil? (get entry :w nil))) (not (nil? (get entry :b nil))))
                    "backend-failed"
                    (str "layer " layer " is missing its weight or bias; a checkpoint without a bias describes a different model than the one trained"))
                  nil))
              nil (keys params))
      params)))

(defn training--modules [graph params]
  ;; One module per trained linear layer, fusion first, head last. A layer
  ;; the worker did not train is not a module. Keys match the ModuleSpec
  ;; field list in apps/grove/codec.zio, because `codec--text` writes only
  ;; the fields it knows and silently drops the rest.
  (let [ops (if (map? graph) (get graph :ops []) [])
        linears (filter (fn [op] (and (map? op) (= (get op :kind "") "linear"))) ops)]
    (loop [index 0 remaining linears acc []]
      (if (empty? remaining) acc
        (let [op (first remaining)
              name (str (get op :output "layer"))]
          (recur (+ index 1) (rest remaining)
                 (if (grove--contains (keys params) name)
                   (conj acc {:name name
                              :input_space "grove.fused/260@1"
                              :output_space "grove.class/2@1"
                              :layers [name]
                              :depends_on (if (= index 0) [] [(str "layer-" index)])
                              :shared_group nil
                              :frozen false
                              :requires nil})
                   acc)))))))

(defn training--rows [params artifact]
  (map (fn [layer]
         {:module layer
          :shape [(count (get (get params layer) :w []))]
          :dtype "float32"
          :artifact (grove--artifact-ref artifact)})
       (keys params)))

(defn training--commit-snapshot [store run-id weights-bytes graph metrics]
  ;; Metrics travel with the run's events, not inside ModelSnapshot: the
  ;; codec's field list has no metrics key, so adding one would be dropped
  ;; on encode and the snapshot would not contain what it appears to.
  (let* [params (training--params weights-bytes)
         artifact (artifact--put store weights-bytes)
         snapshot {:schema 1
                   :owner (grove--principal-id store)
                   :entrypoint "predict"
                   :params (training--rows params artifact)
                   :libraries []
                   :preprocessing_version "geometry-sensor-xor@1.0.0"
                   :modules (training--modules graph params)
                   :ensemble nil
                   :graph graph}
         digest (store--commit-manifest store "ModelSnapshot" snapshot)]
    (store--name-snapshot store "trained-" run-id digest)
    (json-stringify {:snapshot digest :metrics metrics})))
```

- [ ] **Step 5: Rewrite the runner to use existing primitives**

In `apps/grove/runner.zio`:

1. Add `(load "events.zio")`, `(load "training.zio")` and `(require :libs.learning.runner :refer [learn--runner-read-gvd1])` at the top, next to the existing `(defn grove--raise ...)` line.
2. Replace `runner--input-path`/`runner--read-input` (lines 72-75) with:

```zio
(defn runner--read-input [runner raw]
  ;; `host/read-bytes` is the granted filesystem primitive and it resolves
  ;; relative paths against the granted roots, so the input root is a
  ;; capability boundary, not a string prefix this code has to enforce.
  (grove--require (string? raw) "invalid-input" "input must name a permitted file")
  (host/read-bytes raw))
```

3. Replace both `learn--decode-gvd1` call sites (lines 83 and 85) with `learn--runner-read-gvd1`. Task 3 then removes these lines entirely by moving input reading into staging, so this replacement only has to keep the file loadable in between.
4. Replace `inference--commit-trained` (line 154) with `training--commit-snapshot`, reading the worker's output file through the granted read primitive:

```zio
(let* [out (runner--trained-path runner claim)
       digest (training--commit-snapshot store (get claim :run_id)
                  (host/read-bytes out)
                  (get (get claim :work) :graph {})
                  (get f :metrics {}))]
  (store--execute store "UPDATE grove_progress SET result=?1 WHERE attempt=?2" [digest (get claim :attempt_id)])
  (store--record-consumption store digest (get f :consumed [])))
```

`runner--trained-path` is defined in Task 3 Step 4. Task 2 must add that function in the same commit (it is three lines) so `runner.zio` never carries a call to an undefined name, even though nothing invokes it until Task 3 stages a worker output. Add it next to `runner--read-input`:

```zio
(defn runner--trained-path [runner claim]
  (str (get (get (get runner :config) :worker-config) :scratch "")
       "/" (get claim :run_id) "/trained.json"))
```

5. Replace `host/poll` in `runner--drain` (line 203) with the bounded sleep the host actually exposes:

```zio
(do (host/sleep-ms 20)
    (recur (runner--tick owner)))
```

6. Replace the `runner--receive` "done" arm so the state it writes matches the frame it received (the worker ends with a `weights` path plus loss/accuracy fields, not a `model` value and a `metrics` map):

```zio
(if (= type "done")
  (do
    (if (= (get claim :kind) "training")
      (let* [result (codec--normalize (json-parse (training--commit-snapshot
                     store (get claim :run_id)
                     (host/read-bytes (runner--trained-path runner claim))
                     (get (get claim :work) :graph {})
                     {:final_loss (get f :loss nil)
                      :first_loss (get f :first_loss nil)
                      :val_accuracy (get f :val_accuracy nil)})))
             digest (get result :snapshot nil)]
        (store--execute store "UPDATE grove_progress SET result=?1 WHERE attempt=?2" [digest (get claim :attempt_id)])
        (runner--event store claim "trained" (json-stringify (get result :metrics {}))))
      (do
        (coordinator--bill store claim (str (get claim :attempt_id) "-execution") (get f :steps 0))
        (runner--event store claim "execution" (json-stringify (get f :execution {})))))
    (runner--finish runner worker "evaluating"
      (json-stringify {:final_loss (get f :loss nil) :val_accuracy (get f :val_accuracy nil)})))
  ...)
```

The run reaches `evaluating` and stays there: publishing is a separate, human-gated API call (`POST /api/approve`), so an owner must not advance the run past evaluation on the worker's say-so. The `store--record-consumption` call the old code made is dropped: the worker emits no `consumed` vector, and recording an empty consumption under a snapshot digest would be a fact about nothing.

- [ ] **Step 6: Remove the undefined `host/process-id` from the coordinator**

In `apps/grove/coordinator.zio:22-23`, replace the owner record:

```zio
owner {:epoch (+ 1 (grove--get-key previous :epoch 0)) :pid 0}
```

The epoch is the ownership token; the host exposes no process id, and `api--handle-health` already defaults `owner_pid` to 0.

- [ ] **Step 7: Make every mutating handler check the request principal**

Under a writer service handle, `grove--authorize store "operator"` inside a handler checks the *handle*, not the token that made the request — so it would pass for a reader token. Add a request-scoped check in `apps/grove/api.zio`, next to `api--bearer`:

```zio
(defn api--require-role [request required]
  ;; The store handle belongs to the service; the authority for one request
  ;; belongs to the token that made it. Checking the handle would make
  ;; every token an operator.
  (let [role (grove--get-key (get request :principal) :role "")]
    (grove--require (>= (grove--role-rank role) (grove--role-rank required))
      "capability-denied" (str required " role required"))
    true))
```

Then add `(api--require-role request "<role>")` next to the existing `(grove--authorize store "<role>")` line in each mutating handler, keeping that handler's own role: `api--handle-queue` (379), `api--handle-cancel` (405), `api--handle-pause` (423), `api--handle-resume` (442), `api--handle-fork` (464), `api--handle-propose-candidate` (329) — all `operator`; `api--handle-approve` (504) — `publisher`. Two handlers need the same treatment without a matching `grove--authorize` line: `api--handle-select` (484) chooses its required role from the decision (`publisher` for `accept`, `operator` otherwise), and `api--handle-post-signal` (265) authorizes inside `store--submit-signal`, so add `(api--require-role request "annotator")` as its second binding. Read-only handlers keep their existing check.

- [ ] **Step 8: Run the harness**

Run: `bash tools/test-grove-zio-http.sh`
Expected: PASS, with `run-1` settling as `failed` and its event recorded — the owner loop, the event log and the role checks all work, and the run fails honestly because no worker configuration exists yet. Task 3 makes it succeed.

- [ ] **Step 9: Commit**

```bash
git add apps/grove/events.zio apps/grove/training.zio apps/grove/runner.zio apps/grove/coordinator.zio apps/grove/api.zio tools/test-grove-zio-http.sh
git commit -m "feat(grove): drive the runner with real store and learning operations"
```

---

## Task 3: Give the service a real worker configuration and prove training

**Files:**
- Modify: `apps/grove/api.zio` (`api--service-config` builds `worker-config`)
- Modify: `tools/install.sh:73-108` (launcher declares worker paths)
- Test: `tools/test-grove-zio-http.sh` (assert a trained snapshot appears and worker isolation refusal is honest)

- [ ] **Step 1: Add the failing assertion**

In `tools/test-grove-zio-http.sh`, after the settled-state block:

```bash
# A worker that cannot be isolated must refuse training honestly rather
# than report a completed run, and a worker that can must leave a named
# snapshot behind.
if ! command -v unshare >/dev/null 2>&1 || ! unshare -Urn --pid --mount --fork true >/dev/null 2>&1; then
  printf 'SKIP: namespaces unavailable; CPU training not exercised on this host\n' >&2
  [[ "$state" == failed ]] || fail "without namespaces the run must fail honestly, not succeed: $state"
else
  [[ "$state" == evaluating ]] || fail "isolated worker did not reach evaluating: $state"
  expect "named trained snapshot exists" 200 'trained-run-1' \
    -H 'Authorization: Bearer reader-test' "http://$bind/api/learning/publication"
  expect "worker started event recorded" 200 'worker-started' \
    -H 'Authorization: Bearer reader-test' "http://$bind/api/events?run_id=run-1&limit=100"
fi
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash tools/test-grove-zio-http.sh`
Expected: FAIL at `trained snapshot exists` — the state is `failed`, because `(get config :worker-config)` is nil until Step 3 adds it, so `runner--start` cannot spawn a process.

- [ ] **Step 3: Build the worker config from launcher-declared paths**

In `apps/grove/api.zio`, replace `api--service-config`:

```zio
(defn api--service-config [config]
  ;; Worker authority comes from the launcher, never from request data:
  ;; an executable, interpreter and scratch the host granted.
  (let [worker (get config :worker-config {})]
    {:worker-count (get config :workers 2)
     :lease-ms 30000
     :deadline-ms 300000
     :max-frame-bytes 16777216
     :input-root (get config :root "")
     :web-root (get config :web-root "")
     :worker-config worker}))
```

`main.zio`'s `grove--main` already merges the launcher map into `parsed :options`, so `api--service-config` reads the same values from `config`. In `grove--serve-handler`, normalize the root before the service sees it (Step 1 of Task 1 already prefers `--root`); add `:worker-config` to that normalization when the launcher does not supply one:

```zio
serve-root (if (= root "") (get config :root "") root)
```

Keep the launcher flags as the only source: add to `tools/install.sh` inside the generated launcher, after `--app-workers`:

```
  --app-extra worker-config:... \
```

Concretely, extend the launcher `exec` block with two extra launcher flags and teach `main.zio` to read them:

```bash
  --app-worker-script "$SHARE/grove/worker.py" \
  --app-worker-python "$PYTHON" \
```

and in `langs/cli/src/main.rs`, add to `AppLaunch`: `worker_script: Option<PathBuf>`, `worker_python: Option<PathBuf>`, flags `--app-worker-script` / `--app-worker-python`, and in `config()`:

```rust
put("worker-script", self.worker_script.as_ref().map(|p| p.display().to_string()));
put("worker-python", self.worker_python.as_ref().map(|p| p.display().to_string()));
put("worker-scratch", self.data_root.as_ref().map(|root| root.join("worker-scratch").display().to_string()));
```

Then in `apps/grove/api.zio`:

```zio
(defn api--worker-config [config]
  ;; The jail execs `executable` directly under `env -i` with an empty
  ;; environment (contribs/native/host/src/transport/isolation.rs:182), so
  ;; `executable` is the interpreter and the script is argv[1]. Pointing
  ;; `executable` at the .py file would exec a script with no interpreter
  ;; and an empty PATH.
  (let [python (get config :worker-python "")
        script (get config :worker-script "")
        scratch (get config :worker-scratch "")]
    {:executable python
     :argv [script]
     :cwd scratch
     :scratch scratch
     :identity {:run_id "service"}
     :timeout-ms 300000
     :max-frame-bytes 16777216
     :max-address-space 2147483648
     :max-cpu-seconds 300}))
```

`runner--start` overwrites `:identity` with the claim, so the placeholder above only satisfies the non-empty identity check in `ProcessConfig::from_json` (`contribs/native/host/src/transport/process.rs:180-200`).

- [ ] **Step 4: Stage worker inputs into scratch before starting**

Two facts fix the shape of this code. First, the jail binds `scratch` at `/scratch` and pivot-roots everything else away (`contribs/native/host/src/transport/isolation.rs:183`), so a host path such as `/srv/grove/data/worker-scratch/run-1/data.gvd1` does **not** exist inside the worker — the frame must name `/scratch/...`. Second, the worker reads `data`/`weights`/`out` as paths and writes its trained parameters to `out`, so `out` must differ from the input weights file.

Add to `apps/grove/runner.zio`, above `runner--start`:

```zio
(defn runner--stage [runner claim]
  ;; Host paths are staging locations; the frame names jail paths, because
  ;; the jail exposes only /scratch. `out` is separate from `weights` so the
  ;; trained parameters cannot overwrite the file the worker is reading.
  (let* [config (get runner :config) work (get claim :work)
         host-scratch (str (get (get config :worker-config) :scratch "") "/" (get claim :run_id))
         jail (str "/scratch/" (get claim :run_id))
         data (get work :data nil) weights (get work :weights nil)
         steps (get work :steps 1) seed (get work :seed 1)]
    (host/mkdir host-scratch)
    (host/write-atomic (str host-scratch "/data.gvd1") (runner--read-input runner data))
    (host/write-atomic (str host-scratch "/weights.json")
      (if weights (runner--read-input runner weights) (host/json-bytes {:params {}})))
    {:v 1
     :type "train"
     :run_id (get claim :run_id)
     :attempt_id (get claim :attempt_id)
     :graph (get work :graph {})
     :data (str jail "/data.gvd1")
     :weights (str jail "/weights.json")
     :out (str jail "/trained.json")
     :steps steps
     :seed seed}))
```

then make `runner--start` write the staged frame:

```zio
(defn runner--start [runner claim]
  (let* [config (get runner :config)
         frame (if (= (get claim :kind) "training") (runner--stage runner claim)
                  (assoc (get claim :work) :run_id (get claim :run_id)
                                    :attempt_id (get claim :attempt_id)))
         process-config (assoc (get config :worker-config)
                               :identity {:run_id (get claim :run_id)
                                          :attempt_id (get claim :attempt_id)}
                               :max-frame-bytes (get config :max-frame-bytes 16777216))
         proc (host/process-start process-config)]
    (try
      (host/process-write proc frame)
      (catch any (do (host/process-kill proc) (error (str *error*)))))
    (runner--event (get runner :store) claim "worker-started" "isolated worker process admitted")
    {:claim claim :process proc :started_ms (host/clock-ms) :last_seen (host/clock-ms)}))
```

Delete `runner--prepare`'s training branch (lines 79-90): staging is now the single place that reads inputs, and a second reader would decode a dataset the worker never sees. Keep its `zio` branch, which returns the claim unchanged for the bounded agent runner.

- [ ] **Step 5: Make the launcher ship the worker**

In `tools/install.sh`, after the `share/zio` copy loop (lines 49-54) add:

```bash
if [ -d "$ROOT/apps/grove/workers/torch" ]; then
  mkdir -p "$TARGET/share/grove/workers"
  cp -R "$ROOT/apps/grove/workers/torch" "$TARGET/share/grove/workers/torch"
  find "$TARGET/share/grove" -name '__pycache__' -prune -exec rm -rf {} + 2>/dev/null || true
fi
```

and in the generated launcher, name the staged worker and its interpreter, passing both as flags (no environment grant is needed — `app_context` grants only the four `GROVE_TOKEN_*` names):

```bash
WORKER="${GROVE_WORKER_SCRIPT:-$SHARE/grove/workers/torch/worker.py}"
PYTHON="${GROVE_TENSOR_PYTHON:-python3}"
```

with two lines added to the `exec` block:

```
  --app-worker-script "$WORKER" \
  --app-worker-python "$PYTHON" \
```

`PYTHON` already exists in the launcher (`tools/install.sh:86`); keep one definition and delete the duplicate.

- [ ] **Step 6: Run the harness with a real CPU worker**

Run:

```bash
cargo build -p zio-cli && \
GROVE_TOKEN_READER=reader-test GROVE_TOKEN_OPERATOR=operator-test \
GROVE_TOKEN_PUBLISHER=publisher-test \
GROVE_WORKER_SCRIPT=apps/grove/workers/torch/worker.py \
GROVE_TENSOR_PYTHON=.venv/bin/python bash tools/test-grove-zio-http.sh
```

Expected: PASS. `run-1` must settle as `evaluating` rather than `failed`, the `worker-started` event must be present, and a named `trained-run-1` snapshot must exist in `/api/learning/publication`. If the host cannot provide namespaces, the harness prints `SKIP: namespaces unavailable` and the run legitimately settles as `failed` with an isolation error event — report that verbatim instead of weakening the assertion.

- [ ] **Step 7: Commit**

```bash
git add apps/grove/api.zio apps/grove/runner.zio tools/install.sh langs/cli/src/main.rs tools/test-grove-zio-http.sh
git commit -m "feat(grove): run the CPU worker from a launcher-granted configuration"
```

---

## Task 4: Serve Grove UI from an install root and drop duplicated WASM routes

**Files:**
- Modify: `apps/grove/api.zio:534-589,616-626` (`api--static-file` root, remove `/wasm/*` routes and handlers)
- Modify: `apps/grove/main.zio:178-184` (web root resolution)
- Test: `tools/test-grove-zio-http.sh`

- [ ] **Step 1: Make the harness assert the install-root behavior**

In `tools/test-grove-zio-http.sh`, change the two WASM assertions to assert that Grove no longer serves WASM and that it serves from the granted root:

```bash
wasm_meta="$(curl -sS --max-time 3 -o /dev/null -w '%{http_code}' "http://$bind/wasm/zio_core_bg.wasm" 2>/dev/null || true)"
[[ "$wasm_meta" == 404 ]] || fail "Grove must not serve WASM any more (site does): HTTP $wasm_meta"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash tools/test-grove-zio-http.sh`
Expected: FAIL with `Grove must not serve WASM any more (site does): HTTP 200`.

- [ ] **Step 3: Resolve assets from the granted web root**

In `apps/grove/api.zio`, replace `api--static-file` (lines 534-540) and the static handlers (542-555):

```zio
(defn api--web-root [request]
  ;; Assets resolve from the root the host granted, never from the process
  ;; working directory: an installed service is launched from wherever
  ;; the supervisor happens to be.
  (let [root (grove--get-key (get request :web_root) "")]
    (grove--require (not (= root "")) "invalid-input" "no web root was granted to the service")
    root))

(defn api--static-file [request relative content-type]
  (let [path (str (api--web-root request) "/" relative)]
    (host/http-reply
      (get request :listener)
      (get request :id)
      {:status 200
       :headers {"content-type" content-type}
       :body (host/read-bytes path)})))

(defn api--handle-static-index [request]
  (api--static-file request "index.html" "text/html; charset=utf-8"))
(defn api--handle-static-bridge [request]
  (api--static-file request "bridge.js" "text/javascript; charset=utf-8"))
(defn api--handle-static-controller [request]
  (api--static-file request "controller-source.js" "text/javascript; charset=utf-8"))
(defn api--handle-static-styles [request]
  (api--static-file request "styles.css" "text/css; charset=utf-8"))
(defn api--handle-static-favicon [request]
  (api--no-content request))
```

Delete `api--handle-static-wasm-loader`, `api--handle-static-wasm`, and their two entries in `api--routes` (lines 586-587), and delete the two `/wasm/*` entries from `api--public-path` (lines 624-625). Keep `/wasm/*` served by `site`.

- [ ] **Step 4: Pass the web root through the request and the launcher**

In `api--serve`, add `:web_root (get (get config :web-root) (get config :web-root ""))` to the `request` map; in `grove--serve-handler` normalize it exactly like the store root (prefer `--root`'s sibling `web` directory when the launcher did not grant one):

```zio
web-root (get config :web-root (str serve-root "/web"))
```

In `tools/install.sh`, add `--app-web-root "$SHARE/grove/web" \` to the generated launcher and `--app-resource-root` already points at `$SHARE`; add `web_root: Option<PathBuf>` to `AppLaunch` with flag `--app-web-root` and

```rust
put("web-root", self.web_root.as_ref().map(|p| p.display().to_string()));
```

in `config()`. Copy the UI into the install tree next to the worker:

```bash
if [ -d "$ROOT/apps/grove/web" ]; then
  mkdir -p "$TARGET/share/grove"
  cp -R "$ROOT/apps/grove/web" "$TARGET/share/grove/web"
fi
```

- [ ] **Step 5: Run the harness from a directory that is not the repo**

Run: `cd /tmp && bash /home/lszio/Projects/zio/tools/test-grove-zio-http.sh`
Expected: PASS. Any `artifact-unavailable: open authorized path` means the web root is still resolved from the working directory — fix the resolution, do not re-add a CWD fallback.

- [ ] **Step 6: Commit**

```bash
git add apps/grove/api.zio apps/grove/main.zio tools/install.sh langs/cli/src/main.rs tools/test-grove-zio-http.sh
git commit -m "feat(grove): serve UI assets from the granted install root"
```

---

## Task 5: Build the Grove runtime image

**Files:**
- Create: `Dockerfile.grove`
- Modify: `tools/install.sh` (install target used by the image)

- [ ] **Step 1: Write the image**

Create `Dockerfile.grove`:

```dockerfile
# Grove runtime: the installed launcher, its Zio share tree, the Grove UI
# and a locked CPU worker venv. No bun and no nginx — the site owns the
# wasm bundle and the public origin (see Dockerfile and nginx.conf).
FROM rust:1-bookworm AS zio-build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY langs ./langs
COPY contribs ./contribs
COPY libs ./libs
COPY apps ./apps
COPY tools ./tools
COPY examples/manifest.tsv ./examples/manifest.tsv
RUN ZIO_PROFILE=release ./tools/install.sh /opt/zio

FROM python:3.12-slim-bookworm AS worker-venv
COPY apps/grove/workers/torch/requirements.txt /tmp/requirements.txt
RUN python -m venv /opt/grove-venv && \
    /opt/grove-venv/bin/pip install --no-cache-dir \
      --index-url https://download.pytorch.org/whl/cpu -r /tmp/requirements.txt

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      util-linux ca-certificates bash && rm -rf /var/lib/apt/lists/*
# `unshare` from util-linux is the isolation primitive Grove refuses to train
# without; installing the binary is not the same as being granted namespaces.
COPY --from=worker-venv /opt/grove-venv /opt/grove-venv
COPY --from=zio-build /opt/zio /opt/zio
RUN useradd --system --create-home --home-dir /srv/grove grove && \
    mkdir -p /srv/grove/data /srv/grove/data/worker-scratch && \
    chown -R grove:grove /srv/grove
USER grove
ENV GROVE_ROOT=/srv/grove/data \
    GROVE_WORKERS=2 \
    GROVE_BIND=0.0.0.0:8787 \
    GROVE_TENSOR_PYTHON=/opt/grove-venv/bin/python \
    GROVE_WORKER_SCRIPT=/opt/zio/share/grove/workers/torch/worker.py
EXPOSE 8787
# Health asks the running service, it does not start a second one: two
# owners on one store would fight over the coordinator epoch.
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
  CMD ["/opt/grove-venv/bin/python", "-c", "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:8787/api/health', timeout=3).status == 200 else 1)"]
CMD ["/opt/zio/bin/grove", "serve", "--root", "/srv/grove/data"]
```

`CMD` names the launcher, never a bare `serve`: `tools/install.sh:97-107` execs `bin/zio` with every argument re-passed as `--args`, so `grove serve --root X` works but a `command: [serve]` form does not.

`GROVE_WORKER_SCRIPT` and `GROVE_TENSOR_PYTHON` are read by the **bash launcher**, an ordinary process, not by the Zio program — so they need no entry in the host's environment allowlist (`langs/cli/src/main.rs:379-386`), which covers `host/getenv` and worker process environments only.

- [ ] **Step 2: Keep the installer's positional target**

`tools/install.sh:23` already reads `TARGET="${1:-$ROOT/dist/zio}"`, so the image passes `/opt/zio` as the first argument — no installer change is needed. Change only the usage comment at line 12 so it names the two call sites that matter:

```bash
# Usage:
#   tools/install.sh [target-dir]        # default: dist/zio
#   tools/install.sh /opt/zio            # what Dockerfile.grove runs
```

- [ ] **Step 3: Verify the image starts and serves**

Run:

```bash
docker build -f Dockerfile.grove -t zio-grove:plan .
docker run --rm -d --name grove-plan -p 18791:8787 \
  -e GROVE_TOKEN_READER=r -e GROVE_TOKEN_OPERATOR=o -e GROVE_TOKEN_PUBLISHER=p \
  zio-grove:plan
sleep 25
curl -sS --max-time 3 http://127.0.0.1:18791/api/health
curl -sS --max-time 3 -o /dev/null -w 'ui=%{http_code}\n' http://127.0.0.1:18791/
docker rm -f grove-plan
```

Expected: health `{"status":"ok",...,"owner_epoch":1,...}` and `ui=200`. A `read-only database does not exist` error means `/srv/grove/data` is not writable by `grove`; a `ui=502` means the granted web root is missing. Fix the image — do not pass `--user root`.

- [ ] **Step 4: Record what the image can actually isolate**

Run both, and report the real exit codes — do not assume either outcome:

```bash
docker run --rm zio-grove:plan unshare -Urn --pid --mount --fork true; echo "default=$?"
docker run --rm --cap-drop ALL zio-grove:plan unshare -Urn --pid --mount --fork true; echo "no-caps=$?"
```

`default=0` with `no-caps=1` means the container's own default seccomp/capability set already permits user namespaces, so real CPU training works in the deployed image without `privileged`. `default=1` means it does not, and the deployed service will refuse training with an isolation error — an honest refusal, not a defect to hide with `privileged: true`.

- [ ] **Step 5: Commit**

```bash
git add Dockerfile.grove tools/install.sh
git commit -m "build: add the Grove runtime image"
```

---

## Task 6: Split Compose into `site` and `grove`

**Files:**
- Modify: `docker-compose.dokploy.yml`
- Modify: `nginx.conf`
- Create: `docker-compose.preview.yml` (template used by MR previews)

- [ ] **Step 1: Write the two-service stack**

Replace `docker-compose.dokploy.yml`:

```yaml
services:
  site:
    build:
      context: .
      dockerfile: Dockerfile
    restart: unless-stopped
    expose:
      - "80"
    ports:
      # Local end-to-end verification only; Dokploy routes the domain
      # through the internal network.
      - "127.0.0.1:8080:80"
  grove:
    build:
      context: .
      dockerfile: Dockerfile.grove
    restart: unless-stopped
    environment:
      GROVE_ROOT: /srv/grove/data
      GROVE_BIND: 0.0.0.0:8787
      GROVE_WORKERS: "2"
      GROVE_TENSOR_PYTHON: /opt/grove-venv/bin/python
    expose:
      - "8787"
    ports:
      # Local end-to-end verification only. Dokploy routes the domain
      # through the internal network and never publishes this port.
      - "127.0.0.1:8787:8787"
    volumes:
      - grove-data:/srv/grove/data

volumes:
  grove-data:
```

Health lives in `Dockerfile.grove`'s `HEALTHCHECK`, not here: the image already has the interpreter and the URL, and a second definition in Compose would drift from it. Keep both loopback mappings out of `docker-compose.preview.yml` — a preview is reached through its own Dokploy domain, so a host mapping would only expose a temporary stack.

- [ ] **Step 2: Write the same-origin routes**

Append to `nginx.conf`, inside the existing `server` block before its closing brace:

```nginx
    # Grove control plane. One origin, no CORS: the browser talks to
    # /api/* on the same host that served this page.
    location = /grove/app {
        return 301 /grove/app/;
    }
    location /grove/app/ {
        proxy_pass http://grove:8787/;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_read_timeout 600s;
    }
    location /api/ {
        proxy_pass http://grove:8787/api/;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_read_timeout 600s;
    }
```

The site keeps `/`, `/grove/`, `/wasm/*`, docs and examples exactly as today, so the introduction page is not shadowed by the control plane.

- [ ] **Step 3: Create the preview template**

Create `docker-compose.preview.yml`:

```yaml
# MR preview stack. One stack per pull request, its own volume, its own
# tokens, deleted when the pull request closes or merges.
services:
  site:
    build:
      context: .
      dockerfile: Dockerfile
    restart: unless-stopped
    expose:
      - "80"
  grove:
    build:
      context: .
      dockerfile: Dockerfile.grove
    restart: unless-stopped
    environment:
      GROVE_ROOT: /srv/grove/data
      GROVE_BIND: 0.0.0.0:8787
      GROVE_WORKERS: "1"
      GROVE_TENSOR_PYTHON: /opt/grove-venv/bin/python
      GROVE_TOKEN_READER: ${GROVE_PREVIEW_TOKEN_READER}
      GROVE_TOKEN_OPERATOR: ${GROVE_PREVIEW_TOKEN_OPERATOR}
      GROVE_TOKEN_PUBLISHER: ${GROVE_PREVIEW_TOKEN_PUBLISHER}
    expose:
      - "8787"
    volumes:
      - preview-data:/srv/grove/data
    deploy:
      resources:
        limits:
          cpus: "2"
          memory: 4G

volumes:
  preview-data:
```

- [ ] **Step 4: Prove the stack end to end locally**

Run:

```bash
GROVE_TOKEN_READER=r GROVE_TOKEN_OPERATOR=o GROVE_TOKEN_PUBLISHER=p \
docker compose -f docker-compose.dokploy.yml up -d --build
site_port="$(docker compose -f docker-compose.dokploy.yml port site 80 | sed 's/.*://')"
for _ in {1..60}; do
  curl -sS --max-time 3 "http://127.0.0.1:$site_port/api/health" >/dev/null 2>&1 && break
  sleep 2
done
curl -sS --max-time 3 -o /dev/null -w 'site=%{http_code}\n' "http://127.0.0.1:$site_port/grove/"
curl -sS --max-time 3 -o /dev/null -w 'wasm=%{http_code}\n' "http://127.0.0.1:$site_port/wasm/zio_core_bg.wasm"
curl -sS --max-time 3 -o /dev/null -w 'app=%{http_code}\n' "http://127.0.0.1:$site_port/grove/app/"
curl -sS --max-time 3 "http://127.0.0.1:$site_port/api/health"
docker compose -f docker-compose.dokploy.yml down -v
```

Expected: `site=200`, `wasm=200`, `app=200`, health `200` with `owner_epoch` >= 1. Record the exact outputs; if `app` is 502, `grove` is unhealthy — read `docker compose logs grove` before touching Nginx.

- [ ] **Step 5: Commit**

```bash
git add docker-compose.dokploy.yml nginx.conf docker-compose.preview.yml
git commit -m "deploy: run site and Grove as one Compose unit"
```

---

## Task 7: Point the browser at `/grove/app/`

**Files:**
- Modify: `apps/grove/web/index.html:7,218-224`
- Modify: `apps/grove/web/bridge.js:18,427`
- Modify: `apps/site/content/grove/index.mdx` (link to the control plane)
- Test: `apps/grove/web/browser-contract.zio` (unchanged contract, plus the URL assertions)

- [ ] **Step 1: Add the URL assertions to the browser contract**

Append to `apps/grove/web/browser-contract.zio`:

```zio
;; The control plane is served under /grove/app/ by the site's proxy, so
;; absolute browser URLs must name that prefix.
(let [index (grove--read-web-file "index.html")]
  (browser-check (str-contains? index "href=\"/grove/app/styles.css\"")
    "stylesheet must use the /grove/app/ prefix")
  (browser-check (str-contains? index "src=\"/grove/app/bridge.js\"")
    "bridge module must use the /grove/app/ prefix"))

(let [bridge (grove--read-web-file "bridge.js")]
  (browser-check (str-contains? bridge "'/wasm/zio_core.js'")
    "WASM loader is served by the site at /wasm/")
  (browser-check (str-contains? bridge "wasmInit('/wasm/zio_core_bg.wasm')")
    "WASM binary is served by the site at /wasm/"))
```

Add the helper at the top of the same file, next to `browser-check`:

```zio
;; The contract runs as a plain script (`cd apps/grove && zio-cli
;; web/browser-contract.zio`), so it uses `slurp` — the script-level reader —
;; rather than a host capability the script context deliberately lacks.
(defn grove--read-web-file [name]
  (slurp (str "web/" name)))
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash -c 'cd apps/grove && ../../target/debug/zio-cli web/browser-contract.zio'`
Expected: FAIL with `CONTRACT FAIL: stylesheet must use the /grove/app/ prefix`.

- [ ] **Step 3: Rewrite the absolute URLs**

In `apps/grove/web/index.html`: line 7 becomes
`<link rel="stylesheet" href="/grove/app/styles.css">` and line 224 becomes
`<script type="module" src="/grove/app/bridge.js"></script>`; update the comment at lines 218-219 to say the site serves `/wasm/*`.

In `apps/grove/web/bridge.js`: line 18 becomes
`import wasmInit, { ZioSession } from '/wasm/zio_core.js';` (unchanged — the site owns that path) and line 22 becomes
`} from '/grove/app/controller-source.js';`.

- [ ] **Step 4: Link the control plane from the introduction page**

In `apps/site/content/grove/index.mdx`, after the first paragraph add:

```mdx
运行中的控制面在 [`/grove/app/`](/grove/app/)：同一个 origin 上的 Zio/WASM 控制器、队列执行、检查点与人工批准都在那里完成。
```

- [ ] **Step 5: Run the contract and the site build**

Run:

```bash
bash -c 'cd apps/grove && ../../target/debug/zio-cli web/browser-contract.zio' && \
(cd apps/site && bun run build)
```

Expected: contract PASS and an Astro build that emits `/grove/index.html` plus unchanged `/wasm/*` copies.

- [ ] **Step 6: Commit**

```bash
git add apps/grove/web/index.html apps/grove/web/bridge.js apps/grove/web/browser-contract.zio apps/site/content/grove/index.mdx
git commit -m "feat(grove): serve the control plane under /grove/app"
```

---

## Task 8: Prove the deployed path in CI and cover `dev`

**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `tools/test-grove-deployment.sh`

- [ ] **Step 1: Write the deployment verification script**

Create `tools/test-grove-deployment.sh`:

```bash
#!/usr/bin/env bash
# Build both images from one commit and prove the deployed surface:
# site routes, same-origin Grove routing, role authorization, a writable
# store, real owner execution and restart persistence.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
project="zio-deploy-check-$$"
# `down -v` removes the named volumes Compose created for this project;
# there is no volume named after the project, so nothing else to clean.
cleanup() {
  docker compose -p "$project" -f docker-compose.dokploy.yml down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

export GROVE_TOKEN_READER=reader-test
export GROVE_TOKEN_OPERATOR=operator-test
export GROVE_TOKEN_PUBLISHER=publisher-test
docker compose -p "$project" -f docker-compose.dokploy.yml up -d --build
docker compose -p "$project" -f docker-compose.dokploy.yml ps

site_port="$(docker compose -p "$project" -f docker-compose.dokploy.yml port site 80 | sed 's/.*://')"
for _ in {1..60}; do
  if curl -sS --max-time 3 "http://127.0.0.1:$site_port/api/health" >/dev/null 2>&1; then break; fi
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
docker compose -p "$project" -f docker-compose.dokploy.yml restart grove
for _ in {1..60}; do
  if curl -sS --max-time 3 "http://127.0.0.1:$site_port/api/health" >/dev/null 2>&1; then break; fi
  sleep 2
done
check "run survives restart" 200 -H 'Authorization: Bearer reader-test' "http://127.0.0.1:$site_port/api/runs/ci-run"

printf 'PASS unified site+grove deployment\n'
```

- [ ] **Step 2: Add the CI job**

Append to `.github/workflows/ci.yml`:

```yaml
  grove-service:
    name: grove service
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
      - name: CPU worker runtime
        run: |
          python -m venv .venv
          .venv/bin/python -m pip install \
            --index-url https://download.pytorch.org/whl/cpu \
            -r apps/grove/workers/torch/requirements.txt
      - name: Grove HTTP contract
        run: bash tools/test-grove-zio-http.sh
      - name: Browser controller contract
        run: bash -c 'cd apps/grove && ../../target/debug/zio-cli web/browser-contract.zio'
      - name: Unified deployment
        run: bash tools/test-grove-deployment.sh
```

`bash -c` for the controller contract: a `run:` line has no shell, so a bare `cd ... && ...` would be parsed by the YAML-aware runner as a command with arguments, not as a shell line.

- [ ] **Step 3: Widen the push trigger to `dev`**

Replace lines 4-5 of `.github/workflows/ci.yml`:

```yaml
  push:
    branches: [main, dev]
```

- [ ] **Step 4: Run both harnesses locally**

Run:

```bash
bash tools/test-grove-zio-http.sh && bash tools/test-grove-deployment.sh
```

Expected: both PASS on this host, or the deployment script prints an explicit FAIL line naming the missing step. Report the exact output; do not claim Docker coverage you did not run.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ci.yml tools/test-grove-deployment.sh
git commit -m "ci: prove the Grove service and unified deployment"
```

---

## Task 9: Restricted MR preview lifecycle

**Files:**
- Create: `.github/workflows/preview.yml`
- Create: `tools/preview-stack.sh`

- [ ] **Step 1: Write the stack manager**

Create `tools/preview-stack.sh`:

```bash
#!/usr/bin/env bash
# Create, update or delete one pull request's Compose stack and domain.
#
# The caller supplies the Dokploy API base URL and token. The script never
# receives repository code: Dokploy builds the pull request commit, so the
# only trust boundary is the workflow permission gate that calls this.
set -euo pipefail

api="${DOKPLOY_API_URL:?DOKPLOY_API_URL is required}"
token="${DOKPLOY_API_TOKEN:?DOKPLOY_API_TOKEN is required}"
project="${PREVIEW_PROJECT:?PREVIEW_PROJECT is required, e.g. zio-pr-123}"
domain="${PREVIEW_DOMAIN:?PREVIEW_DOMAIN is required}"
action="${1:?usage: preview-stack.sh up|delete}"

call() {
  curl -sS --fail -X "$1" "$api/$2" \
    -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
    ${3:+-d "$3"}
}

case "$action" in
  up)
    call POST compose/create "{\"name\":\"$project\",\"source\":\"github\",\"gitRepository\":\"lszio/zio\",\"gitBranch\":\"pull/${PR_NUMBER}/head\",\"composeFile\":\"docker-compose.preview.yml\",\"composeType\":\"file\",\"envFile\":\"/etc/dokploy/compose/$project.env\"}" >/dev/null
    call POST domain/create "{\"applicationType\":\"compose\",\"composeId\":\"$project\",\"domain\":\"$domain\"}" >/dev/null
    printf 'preview stack %s is building from pull request %s\n' "$project" "$PR_NUMBER"
    ;;
  delete)
    call POST domain/deleteByComposeId "{\"composeId\":\"$project\"}" >/dev/null || true
    call POST compose/delete "{\"composeId\":\"$project\",\"deleteVolumes\":true}" >/dev/null
    printf 'preview stack %s deleted with its volume\n' "$project"
    ;;
  *)
    printf 'unknown action %s\n' "$action" >&2
    exit 2
    ;;
esac
```

Verify every endpoint name against the live API before relying on it: `curl -sS -H "Authorization: Bearer $DOKPLOY_API_TOKEN" "$DOKPLOY_API_URL/openapi.json" | jq -r '.paths | keys[]' | grep -E 'compose|domain'`. Replace the paths above with what the API actually exposes and record the final paths in this file's header comment.

- [ ] **Step 2: Write the workflow with the permission gate**

Create `.github/workflows/preview.yml`:

```yaml
name: MR preview
# Pull requests build an untrusted Dockerfile inside Dokploy, so the
# credential lives here and never runs PR code. Deployments are limited to
# same-repository pull requests from users with write access, or to a
# maintainer's explicit approval.
on:
  pull_request_target:
    types: [opened, reopened, synchronize, closed]
  workflow_dispatch:
    inputs:
      pull_request:
        description: pull request number
        required: true

permissions:
  contents: read

jobs:
  preview:
    runs-on: ubuntu-latest
    if: >-
      github.event_name == 'workflow_dispatch' ||
      (github.event.pull_request.head.repo.full_name == github.repository &&
       contains(fromJSON('["OWNER","MEMBER","COLLABORATOR"]'), github.event.pull_request.author_association))
    steps:
      - uses: actions/checkout@v4
        with:
          ref: ${{ github.event.pull_request.head.sha || 'main' }}
      - name: Resolve preview identity
        id: preview
        env:
          PR_NUMBER: ${{ github.event.pull_request.number || inputs.pull_request }}
        run: |
          echo "project=zio-pr-$PR_NUMBER" >> "$GITHUB_OUTPUT"
          echo "domain=zio-pr-$PR_NUMBER.${{ vars.PREVIEW_ZONE }}" >> "$GITHUB_OUTPUT"
      - name: Delete preview
        if: >-
          github.event_name == 'workflow_dispatch' ||
          github.event.action == 'closed'
        env:
          DOKPLOY_API_URL: ${{ secrets.DOKPLOY_API_URL }}
          DOKPLOY_API_TOKEN: ${{ secrets.DOKPLOY_API_TOKEN }}
          PREVIEW_PROJECT: ${{ steps.preview.outputs.project }}
          PREVIEW_DOMAIN: ${{ steps.preview.outputs.domain }}
        run: bash tools/preview-stack.sh delete
      - name: Create preview
        if: >-
          github.event_name != 'workflow_dispatch' && github.event.action != 'closed'
        env:
          DOKPLOY_API_URL: ${{ secrets.DOKPLOY_API_URL }}
          DOKPLOY_API_TOKEN: ${{ secrets.DOKPLOY_API_TOKEN }}
          PREVIEW_PROJECT: ${{ steps.preview.outputs.project }}
          PREVIEW_DOMAIN: ${{ steps.preview.outputs.domain }}
          GROVE_PREVIEW_TOKEN_READER: ${{ secrets.GROVE_PREVIEW_TOKEN_READER }}
          GROVE_PREVIEW_TOKEN_OPERATOR: ${{ secrets.GROVE_PREVIEW_TOKEN_OPERATOR }}
          GROVE_PREVIEW_TOKEN_PUBLISHER: ${{ secrets.GROVE_PREVIEW_TOKEN_PUBLISHER }}
          PR_NUMBER: ${{ github.event.pull_request.number }}
        run: bash tools/preview-stack.sh up
```

- [ ] **Step 3: Verify the permission gate refuses outsiders**

Run: `gh api repos/lszio/zio/actions/workflows/preview.yml/runs --jq '.workflow_runs[0].conclusion' 2>/dev/null || true`, then open a test pull request from a fork and confirm the job is `skipped`, not run. Expected: `skipped`. Record the observed conclusion.

- [ ] **Step 4: Verify cleanup removes the volume**

Run: `docker volume ls | grep zio-pr-` after a preview delete. Expected: no matching volume. If one survives, the delete call is missing `deleteVolumes` or is hitting the wrong id.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/preview.yml tools/preview-stack.sh
git commit -m "ci: manage isolated MR preview stacks"
```

---

## Task 10: Document the environment contract and protect production data

**Files:**
- Modify: `docs/superpowers/specs/2026-10-08-unified-zio-grove-runtime-design.md` (status line)
- Create: `docs/deployment.md`
- Modify: `README.md:78-103` (point at the deployment doc)

- [ ] **Step 1: Write the deployment runbook**

Create `docs/deployment.md` with these sections and exact commands:

1. **Environments** — table from the spec: `main` → `zio.lszio.space`, `dev` → `zio-dev.lszio.space`, `zio-pr-<n>.<zone>` per pull request.
2. **What runs where** — `site` (Nginx + Astro + wasm) and `grove` (Zio app + CPU worker), same commit, one origin.
3. **Tokens** — one set per environment; previews never reuse production values.
4. **First production cutover** — `docker compose` smoke test locally, then dev, then `main`; production stays on `main` only.
5. **Backup and restore** — the exact commands, run once before the first production write:

```bash
docker compose -p zio -f docker-compose.dokploy.yml stop grove
docker run --rm -v zio_grove-data:/data -v "$PWD":/backup alpine \
  tar czf /backup/grove-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz -C /data .
docker compose -p zio -f docker-compose.dokploy.yml start grove
# Restore drill, into a throwaway volume:
docker volume create grove-restore-drill
docker run --rm -v grove-restore-drill:/data -v "$PWD":/backup alpine \
  tar xzf /backup/grove-data-<timestamp>.tar.gz -C /data
docker volume rm -f grove-restore-drill
```

6. **Honest capability report** — namespaces unavailable in the image ⇒ training refused with an error, never a fake completion.

- [ ] **Step 2: Update the spec status and README**

In the spec, change the status line to record that the design is approved and that Tasks 1-10 implement it; in `README.md`, extend the "落地页" section with one line: `运行与预览环境见 [docs/deployment.md](deployment.md)。`

- [ ] **Step 3: Run the docs check**

Run: `bash tools/project-status.sh --check`
Expected: exit 0. A dangling link fails the check — fix the path rather than the checker.

- [ ] **Step 4: Commit**

```bash
git add docs/deployment.md docs/superpowers/specs/2026-10-08-unified-zio-grove-runtime-design.md README.md
git commit -m "docs: record the deployment and environment contract"
```

---

## Completion evidence to report

Run and paste real output for:

1. `bash tools/test-grove-zio-http.sh` — Grove service: epoch, queue POST, settled state, events, role refusal.
2. `bash -c 'cd apps/grove && ../../target/debug/zio-cli web/browser-contract.zio'` — browser contract.
3. `bash tools/test-grove-deployment.sh` — images, routing, roles, owner execution, restart persistence.
4. `docker run --rm zio-grove:plan unshare -Urn --pid --mount --fork true; echo "default=$?"` and the same command with `--cap-drop ALL` — what the deployed image can actually isolate.
5. `cargo test --workspace` — no regression from the `AppLaunch` change.
6. `bash tools/project-status.sh --check` — documentation links still resolve.

If step 1 or 3 fails, report the failing assertion verbatim. Do not mark a task done on a skipped or unrun check.