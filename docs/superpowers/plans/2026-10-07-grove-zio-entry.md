# Grove Zio application entry implementation plan

> **For agentic workers:** Use executing-plans. Parent owns integration; no new host abstraction or runtime dependency.

**Goal:** Give Grove an actual Zio application entry while keeping the reusable agent loop in Loom and native authority in the host.

**Architecture:** `apps/grove/main.zio` loads `libs/loom/agent.zio` and defines the application's `agent-entry`. The library owns reusable retry/feedback behavior, not an application entry or a default grant. Host budgets, generated-code isolation, storage and publication remain native. This is an application-entry cutover, not a complete Grove business rewrite.

**Tech Stack:** Existing Zio evaluator, Rust host, existing agent and governance contracts.

## 1. Preserve exact grant behavior

- [x] Extend `apps/grove/native/app/tests/agent_contract.rs::the_grants_turn_ceiling_stops_the_loop_not_the_source` to exercise grants 0, 2 and 4 against failing generated programs. Assert `status == "exhausted"`, `turns == max_turns` and `calls_made == max_turns`. The current implementation must fail for 0/2/4 because the library entry reads the keyword budget with a string and falls back to 3.
- [x] Run `cargo test -p grove-app --test agent_contract the_grants_turn_ceiling_stops_the_loop_not_the_source` before the implementation and observe the real mismatch.

## 2. Separate application and library

- [x] Create the complete application entry in `apps/grove/main.zio`:

```clojure
(require :libs.loom.agent :refer [agent-run])

(defn agent-entry [task budget]
  (agent-run task (get budget :max_turns 0)))
```

- [x] Remove `agent-entry` and the load-time println from `libs/loom/agent.zio`; retain `source-of`, `feedback-of`, and `agent-run` unchanged. Remove contradictory string-key map comments; host maps use keywords.
- [x] In `apps/grove/native/app/src/agent.rs`, set `DEFAULT_AGENT_LOGIC` to `"apps/grove/main.zio"` and update its ownership comments. Keep the public host API, budget metering and sandbox restrictions unchanged.

## 3. Keep governance connected

- [x] Add `.with("apps/grove/main.zio", Governance::Governed)` to `apps/grove/native/learning/src/logic.rs::default_governance`. Keep the existing Loom library governed because its generic retry logic remains a real, independently governable module; it is not a compatibility alias.
- [x] Change the application proposal in `tools/smoke-grove-logic-approval.py` to target `apps/grove/main.zio` and define `agent-entry` in the proposed code. Existing generic-library governance contracts remain intentionally applicable to `libs/loom/agent.zio`.

## 4. Exercise and document the cutover

- [x] Run the complete agent contract and workspace all-feature suite serially. Exercise the real Grove CLI against a local OpenAI-compatible HTTP provider returning genuinely executed failing Zio programs; a grant of four must produce four HTTP requests and report four turns, with no publication. No external-model competence is claimed.
- [x] Run the real dual-training/HTTP approval smoke in a uniquely owned scratch directory, preserving previous stores. Check the new application proposal is governed, protected code stays refused, Publisher approval and replay/conflict behavior still hold.
- [x] Update README, architecture and feature evidence with the application/library split, exact observed commands and remaining native business. Do not create empty products or claim self-hosting.

## Impact and commit boundary

The first structural cutover is committed on `refactor/language-first` as `aa75350`. Pre-commit staged analysis reported 245 files, 364 changed symbols and 22 affected indexed processes, CRITICAL risk, without the previous listing-cap warning (Git staging recognized physical renames). New entry symbols and dynamic host callers return UNKNOWN in the graph and have been checked in source; `default_governance` has one resolved API caller and LOW risk. This stage keeps authorization/transaction/isolation code unchanged. No push or production deployment.


## Exercised evidence

- Failing-before regression: a zero-turn grant was reported as three turns. After the cutover, the existing contract exercises 0/2/4 grants and asserts exact turns and charged model calls; all eight agent contracts passed.
- Relative `load` initially resolved against Cargo's native application cwd and failed. The application now uses the existing granted-root `require` loader with an explicit `:refer`, not a cwd mutation, fallback path or new loader. Both the Cargo contract context and the real CLI context executed it successfully.
- `cargo test --workspace --all-features`: 489 passed in 47 suites.
- Real `grove run --max-turns 4` against an owned local OpenAI-compatible HTTP endpoint: four HTTP requests, four genuinely executed failing Zio programs, exhausted status, and the actual `entry-smoke-failure` execution error carried into subsequent request feedback. This proves the transport/dispatch/execution path, not an external model's competence.
- The real dual CPU training / HTTP approval smoke passed with its application proposal now naming `apps/grove/main.zio`: protected code refused, Operator approval refused, Publisher approval moved the pointer once, replay did not mint a version, stale expected version refused. Training did not publish by itself. All scratch directories were uniquely owned; earlier stores were preserved.

The native business implementation remains explicit legacy. Only application entry ownership and its authorized dispatch contract have been cut over in this stage; no claim of complete Grove Zio implementation or compiler self-hosting.

## Follow-on: real candidate-selection policy

- [x] Move quality/cost selection into `apps/grove/selection.zio`. Preserve the existing comparison rule, input order, gate exclusion, and empty result; this migration does not redefine equal-quality/equal-cost ties.
- [x] Replace both native CLI and HTTP selection implementations with one typed adapter over `evaluation::Comparison`. The adapter supplies measured metric pairs, the existing uniform cost of 1, and native gate verdicts; Zio chooses the accuracy metric and selected row indices. Use the existing language bootstrap/evaluator, keyword row keys, and checked integer indices. Do not grant Grove storage, publication, or model bindings.
- [x] Delete native `evaluation::Candidate` and `evaluation::non_dominated`, including their obsolete Rust test; retain the quality/cost/gate behavioral contract against the real Zio strategy. Add a host-boundary check covering metric selection and gate rejection.
- [x] Exercise the strategy in the language CLI and real Grove CLI/HTTP against an owned store; run affected contracts and the all-feature workspace suite. Update current architecture/feature evidence without claiming the remaining native business has migrated. No push or deployment.

### Selection migration evidence

- The new policy contract failed before implementation because no Zio selection strategy existed. Two application contracts now exercise quality/cost alternatives, dominated candidates, gate-failed would-be dominators, empty/all-failed inputs, accuracy rather than another metric, and missing accuracy.
- The typed-boundary contract exposed a language-core defect: `(>= 0.95 0)` returned false and an accuracy-less row survived. `numeric::compare` incorrectly merged `Float,Int` with `Int,Float` without reversing order. Fixed the shared primitive, not the Grove default; also corrected the exact `-2^63` mixed boundary, which the actual CLI showed comparing unequal. The core regression failed first and now covers 64 comparisons across four operators, both operand orders, fractions, `2^53`, and i64 bounds. All 16 runtime contracts passed.
- Actual `zio-cli` execution changed the probe from `numeric-order false true` / `selection (0 1)` to `numeric-order true true` / `selection (0)`; both minimum-i64 equality comparisons are now true.
- Actual Grove dual CPU demo in an owned scratch store measured baseline accuracy 0.53125 and candidate accuracy 1.000. CLI and authenticated `POST /api/learning/select` both selected only the trained candidate and exposed the baseline gate failure. Empty HTTP comparison returned empty rows/selection, an unknown bearer token was refused with 403 `capability-denied`, and inspection still reported `publication: none`. The owned service and scratch were removed.
- `cargo test --workspace --all-features`: 491 passed in 48 suites, 0 failures. Existing native unused-import/variable/result warnings and torch/Python warnings were not suppressed; no warning-free claim.
- `tools/project-status.sh` measured 476 default-feature tests; this is a test inventory, not the all-feature executed total.
- `tools/project-status.sh --check` passed. `tools/build-wasm.sh` rebuilt the committed runtime. Direct shipped-WASM execution passed all 64 mixed comparisons, the existing native-compatible presets/snippets, three offline agent scenarios, and the actual two-episode/40-step Q-learning template. Infinity and undelivered promises remained explicit errors, and macro expansion remained hygienic.
- The raw `eval_zio` export needs an explicit core stdlib prelude for this Grove policy (`not`/`empty?` were absent without it); the parity smoke supplied the same `libs/std/core.zio` that native `language_context` already installs. This stage does not change raw-WASM bootstrap or claim browser-native Grove product execution.
- Real Chromium on the built playground returned `(true true true true)` for fractional/minimum-i64 probes and `(true true)` for both `2^53` operand orders. All six UI presets executed, including actual JS/DOM interop. Multiline Ctrl+Enter worked; 390px playground/Grove pages had no observed horizontal overflow, screenshots were inspected, and no page errors were observed. Astro built 47 pages; its existing MDX head-inject warnings remain.
- Pre-commit graph review: 15 changed files, 23 mapped symbols, four affected HTTP selection flows, MEDIUM risk. The index still reports capped dynamic callable-value walks; this is not an all-callers-resolved claim. Actual CLI/HTTP, core contracts, and shipped-WASM/browser execution provide the runtime evidence above.
