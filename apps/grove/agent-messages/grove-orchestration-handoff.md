# GrovePersistence → GroveOrchestration handoff

The GroveOrchestration agent is archived. This message is left on disk
for the parent to pass forward when a new dispatcher agent starts.

From GrovePersistence: stable store.zio surface in
`/home/lszio/Projects/zio/apps/grove/store.zio`. `feedback.zio` rewritten
as a thin pass-through over the new store API.

## Open / close
- `(store--open root principal)` -> store handle; reader-open opens DB
  read-only and never reclaims leases / never DDLs.
- `(store--close store)` ; `(store--read-only? store)`

## Generic primitives
- `(store--query store sql params)` ; `(store--execute store sql params)` ;
  `(store--transaction store callback)` [zero-arg cb; nested=savepoint]
- `(store--raw store kind id)` -> body TEXT ; `(grove--get store kind id)` ;
  `(grove--list store kind)`
- `(store--manifest-body store digest)` ; `(store--load-manifest store digest)` ;
  `(store--commit-manifest store kind record)` -> digest
- `(store--require-owner store digest)` ; `(store--insert store table columns params)`

## Domain writes (single writer per kind)
- `(store--put-observation store record)` — reader, owned
- `(store--put-prediction store record)` — reader
- `(store--put-checkpoint store record)` — operator, owned; commits
  manifest + row atomically
- `(store--put-branch store record)` — operator, owned
- `(store--put-population store record)` — operator, owned
- `(store--put-run store record)` — operator
- `(store--set-run store run)` — state+body update
- `(store--put-recipe store record)` — operator
- `(store--consume-steps store run-id steps)`
- `(store--record-consumption store source-id consumed-vec)` — per-source
  training provenance under `root/training/`

## Signals & frozen datasets
- `(store--signal-target signal)`
- `(store--submit-signal store signal)` -> `"accepted" | "duplicate"`
- `(store--active-signals store target)` ; `(store--signal-status store id)` ;
  `(store--withdraw-signal store id)`
- `(store--resolve-conflict store keep supersede)` ;
  `(store--signal-conflicts store target)`
- `(store--freeze-dataset store revision)` ;
  `(store--pending-signals store task)`

## Evaluations, protocols, acceptance budget
- `(store--put-protocol store protocol)` ; `(store--get-protocol store id)`
- `(store--put-evaluation store record)` — refuses non-finite metrics
- `(store--evaluations-for store snapshot)`
- `(store--provision-acceptance-budget store protocol amount)`
- `(store--consume-acceptance-budget store protocol amount)` -> remaining

## Publication, candidates, approvals, rejections
- `(store--active-publication store)`
- `(store--require-deployable store snapshot)` ;
  `(store--require-resumable store run snapshot)` — a nil snapshot means
  the run has no base lineage to check against invalidations; the gate
  then refuses when the run's `dataset_revision` carried a retracted
  signal. A non-nil snapshot defers to `store--require-deployable`.
- `(store--publish store snapshot expected-version)` -> version — publisher
- `(store--insert-candidate store candidate digest)` ;
  `(store--candidate-state store id)` ;
  `(store--set-candidate-state store id state)`
- `(store--active-candidate-id store)`
- `(store--put-approval store approval)` ; `(store--get-approval store id)`
- `(store--approval-evaluation-refs store id)`
- `(store--approval-is-current store approval)` ;
  `(store--approval-version-is-current store approval)`
- `(store--note-rejection store subject reason now-ms)` ;
  `(store--rejection store subject)`

## Lineage
- `(store--record-derivation store child-id parents derivation)`
- `(store--parents-of store child-id)`
- `(store--name-snapshot store name digest)` ;
  `(store--named-snapshot store name)` ;
  `(store--named-snapshots store)`

## Run queue / attempts / billing / queued work
- `(store--queued-runs store)`
- `(store--epoch-check store epoch)`
- `(store--claim-queued-run store run-id attempt-id reserve-steps lease-ms now-ms)` -> claim map
- `(store--finish-claimed-run store run-id attempt-id epoch final-state now-ms)`
- `(store--request-cancel store run-id)`
- `(store--put-queued-work store run-id kind payload)` ;
  `(store--queued-work store run-id)`
- `(store--put-receipt store operation-id status body)` ;
  `(store--get-receipt store operation-id)`
- `(store--record-billing store message-id steps outcome detail)` ;
  `(store--billing store message-id)`

## Execution events (append-only)
- `(store--append-events store events)` — refuses if detail > 8192 bytes
  or `(run_id, sequence)` already written
- `(store--read-events store run-id after)`

## Retraction
- `(store--retraction-datasets store signal-id)` ;
  `(store--retraction-affected-runs store dataset-ids)` ;
  `(store--retraction-affected-checkpoints store run-ids)`
- `(store--retraction-snapshots store run-ids ckpt-ids)`
- `(store--propagate-retraction store signal-id)` -> retraction report
- `(store--snapshot-invalidation store snapshot)`
- `(store--retract-signal store signal-id)`

## Retention
- `(store--retention-horizon-ms store)` ;
  `(store--set-retention-horizon-ms store horizon-ms)`
- `(store--live-artifacts store)` -> live set
- `(store--collect-artifacts store)` -> cleanup report
  (no-op when horizon is undeclared)

## High-level callers (composite + policy)
- `(store--gate-failures records protocol)` -> [string]
- `(store--compare-snapshot store protocol snapshot)` -> comparison row
- `(store--evaluate-candidate store protocol candidate now-ms)` -> verdict
  (commits evaluation manifests so the approval can bind to them)
- `(store--propose-candidate store candidate parent-source parent-deps governance)`
  -> digest; refuses on protected path
- `(store--record-approval store approval)`
- `(store--publish-approved store approval expected-version)` -> version
- `(store--approve-and-publish store protocol candidate expected-version now-ms)`
  -> `{version approval verdict}`
- `(store--decline store subject reason now-ms)`

## Receipt replay (operation-id is principal+route+payload bound)
- `(store--receipt-lookup store operation principal route payload)` -> `{status body}|nil`
- `(store--receipt-store store operation principal route payload status response)`

## Authority boundary (enforced, not just named)
1. Every `store--open` takes the principal; the open principal is the
   only identity source. Request-body `approved=true` / `actor` is never
   read for an authority decision.
2. Every writer calls `(grove--authorize store required-role)` at its head.
3. Reader role opens DB read-only (`host/db-open :read-only true`); a
   reader-open performs NO DDL and NEVER reclaims owner leases.
4. `(store--publish ...)` is gated by `publisher` role AND
   `(store--require-deployable ...)` which checks retraction
   invalidations. It is reachable only via `(store--approve-and-publish ...)`
   or `(store--publish-approved ...)` — both first record a
   `HumanApproval` bound to live evaluation refs AND verify the version
   race is current.
5. `(store--put-approval ...)` refuses a duplicate approval id (conflict),
   so "one decision, one row".
6. Retraction walks signal -> frozen datasets -> runs -> checkpoints ->
   snapshots and inserts into `invalidations`; the publication step
   refuses any snapshot touched by a retraction.
7. `(store--collect-artifacts ...)` refuses to delete anything without
   an explicit `retention.horizon_ms` and always retains any hex reached
   from active publication / non-terminal run / candidate / approval.

## Caveats
- Did not invoke the Zio interpreter (parent verifies once after
  parallel edits settle).
- `feedback.zio` rewritten to delegate; `coordinator.zio` / `runner.zio` /
  `checkpoint.zio` / `main.zio` are owned by other agents and untouched
  here.
- For candidate / library execution the trusted caller MUST go through
  `host/evaluate-isolated` (per the project directive). The store never
  grants candidate code a `host/db` handle.
