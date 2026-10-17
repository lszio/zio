# 2026-10-16 — ZIO condition/restart minimal semantics (D09)

## 1. Status & scope

This is the minimal condition/handler/restart/cleanup contract (D09) for M2, referenced by ZIO-006 (synchronous local restart implementation). Parent design: `docs/superpowers/specs/2026-10-11-zio-language-toolchain-design.md` (§4 semantics, §6 image save/restore, §7.4 execution limits). It fixes: dynamic-extent handler search, handler selection, restart parameters and visibility, return/exit/cleanup ordering, and the boundary between synchronous handling and cross-request suspension. Only synchronous, in-executor handling is specified here; cross-request suspension (an image saved mid-condition that resumes at a later request) is explicitly deferred to ZIO-012. `docs/feature-matrix.md` baseline is not changed by this document; ZIO-006 updates the matrix after implementation.

## 2. Condition

* A **condition** is a typed, named, exception-like value produced by the evaluator or by user code. It is an ordinary first-class value (can be constructed, passed, inspected) but is *signalled* rather than returned to the caller.

* Predicate: `condition?` — true iff the value is a condition.

* Constructors: `signal-condition!` and `error!` (both raise; `error!` additionally marks the condition as an error for session-level reporting).

* A condition carries:

  * `condition-name` — a symbol identifying the condition type (e.g. `read-failure`, `fuel-exhaustion`, `cancelled`); dispatch key for handlers;

  * an optional payload — zero or more values/args supplied at signalling;

  * `span` — source span, present when raised by the evaluator itself; span type follows `langs/core/src/span.rs` (source provenance). User-raised conditions may carry a span when the signalling form is at a known source location.

## 3. Handler

* Form: `(with-handlers ((<condition-name> <handler-fn>) ...) <body> ...)` — handler clauses (name → function) plus a protected body. The handler-fn receives the condition as its single argument.

* The handler binds `condition-name` to a function whose result-domain is a *restart* (see §4). When a condition matching a name is signalled inside `<body>`, the outermost (most recent) matching handler on the dynamic chain is invoked; whatever it does determines recovery or re-raise.

* Unmatched conditions re-raise unchanged through the handler to the next outer handler.

* **Handler scope is dynamic** (Common Lisp `handler-bind` model): the handler is found by walking the dynamic chain of active `with-handlers` frames, NOT the lexical chain. This reuses the existing `Arc<Env>` / `EvalContext` chain (`langs/core/src/eval.rs:115`, `langs/core/src/bootstrap.rs:251`, `langs/core/src/env.rs`) with handler frames as a distinct stack alongside value frames; no new binding mechanism is invented.

## 4. Restart

* A **restart** is a function that a handler may invoke to recover from a condition. It is the handler's way of saying "continue, but with X". Restarts are invoked by name; a restart function may take arguments supplied by the handler (e.g. the fallback value to use).

* Explicit API:

  * `find-restart 'name` → the restart function or a not-found marker;

  * `invoke-restart 'name arg ...` → calls the restart, transferring control out of the condition signalling stack.

* `restart-case` macro form: `(restart-case <body> (name (arg ...) <body2 ...>) ...)` binds restart names with an explicit argument contract (arity and, where useful, type) for the duration of `<body>`. Calling `invoke-restart` with the wrong arity is a contract error.

* Visibility: a restart is visible only inside the dynamic extent of the `restart-case` (or `with-handlers`-supplied restart) that bound it, matched by walking the same dynamic chain.

## 5. Cleanup

* Form: `(unwind-protect <body> <cleanup> ...)`.

* Cleanup forms run **always** when the protected `<body>` exits — normally, or via non-local exit: condition signalling, restart invocation, `return`, error.

* Cleanup order: innermost-first (the closest `unwind-protect` cleans up before any outer one).

* If a cleanup form itself fails while unwinding for an existing exit cause, the cleanup failure does NOT suppress the original exit: both are reported (original cause first, cleanup error attached), matching the existing `EvalError` reporting path (`langs/core/src/eval.rs:136` for one example of error-propagation).

## 6. The example walked three ways

Example (the paca-required walkthrough): *read failure → substitute value → continue*.

```zio
;; source, identical for all three executors
(define (read-input)                      ; host read; raises on failure
  (error! 'read-failure "input source unavailable"))
(define fallback '(0 1 2 0))

(with-handlers ((read-failure (λ (c) (invoke-restart 'use-fallback c))))
  (unwind-protect
    (restart-case (sum (read-input))      ; (sum ...) = user reduction step
      (use-fallback (c) fallback))        ; restart arity: 1 (the condition)
    (log-cleanup! "reads done")))         ; must run despite the detour
```

Observable result on all three: `(sum (read-input))` never completes normally; `read-failure` is signalled; the inner handler invokes restart `use-fallback`; the restart returns `fallback`, which replaces the body value; `unwind-protect` cleanup `(log-cleanup! "reads done")` still runs (innermost-first); outer continuation proceeds with `sum` of the fallback list. If the handler instead declines (no matching restart), the condition re-raises; with no outer handler the session reports `uncaught-condition: read-failure`.

### 6a. AST executor (`langs/core/src/eval.rs`)

* `error!` lowers to a signalling path that raises a condition value (carried by `EvalError` or a sibling carry path) instead of returning.

* `with-handlers` pushes a handler frame onto the evaluator's dynamic context; on signal, `eval_expr` (`langs/core/src/eval.rs:20`) walks the handler chain from innermost outward, invoking the first name-matching fn.

* `invoke-restart` pops the signalling frame via the captured continuation-equivalent (the evaluator's control stack at the `restart-case` binding), substitutes the restart's return, and resumes evaluation; intervening frames (including the failed `(read-input)` call) are unwound with `unwind-protect` cleanups run.

* Cleanup runs in eval's normal unwinding loop; cleanup errors attach to the primary error.

### 6b. Explicit VM (`langs/core/src/bytecode.rs`)

* Conditions cross the bytecode boundary explicitly: a `signal` opcode raises (rather than a plain error opcode), leaving a condition on the value stack; `invoke-restart` opcode performs the control transfer.

* The handler table lives **on the VM stack frame** (a per-frame handler slot), so frame pop = dynamic-extent end for its handlers; no separate registry.

* `restart-case` compiles to a frame with restart entries (name + target PC + arity); `invoke-restart` validates arity and `jump`s to the restart body.

* Cleanup is frame metadata: frame unwind (`return`/`signal`/`invoke-restart`) first runs the frame's cleanup list innermost-first.

* Budget checks (§10) happen at the same per-instruction point as today's fuel accounting (`langs/core/src/observer.rs:203`).

### 6c. Zio metacircular (`langs/core/assets/interpreter/evaluator.zio`)

* The handler is stored in a **dynamic env cell** (a special name in the environment chain, e.g. `%handlers`, pushed by `with-handlers` expansion); lookup walks the env chain, not lexical bindings. The interpreter's env API is `interpreter/env :new/:get/:set` (`langs/core/assets/interpreter/evaluator.zio:76-88`).

* The restart is a **closure over the body env**: `restart-case` expands to binding a closure that captures the continuation env at the restart site, so invoking it re-enters evaluation at that point with the substitute value.

* `unwind-protect` expands to a form whose cleanup the meta-evaluator runs on any non-local exit of the protected body (meta-level matching of eval's exit paths); macro lowering in `langs/core/assets/interpreter/expand.zio`.

## 7. Dynamic vs lexical scope

* Handlers are **dynamic**: `with-handlers` affects the protected body's *dynamic* chain, visible to anything the body calls, including lambdas lexically defined elsewhere.

* Lexical `let` bindings (or a lexical closure's captured variables) inside a handler body do NOT extend into the protected body: the protected body sees exactly the dynamic chain captured at `with-handlers` entry, plus its own lexical env. A handler may still call closures — value passing is unchanged; only handler *selection* is dynamic.

* This reuses the repo's existing dynamic/context binding concept (`langs/core/src/env.rs`; `docs/glossary.md` "dynamic extent"); no new binding mode is introduced.

## 8. Return / exit / cleanup ordering

* **Restart returns**: when a handler invokes a restart that returns a value, the protected body is unwound — all `unwind-protect` cleanups between the restart site and the body run innermost-first — and the restart's return value **replaces** the body's value. Execution continues after the `with-handlers`/`restart-case` instead of resuming the failed computation (no automatic retry; a retry is just a restart that calls the failing fn again).

* **Decline**: a handler that does not invoke a restart returns normally from the handler fn; the condition then propagates outward to the next handler on the dynamic chain. A handler may also explicitly re-raise (`signal-condition!` the same or another name).

* **No handler**: the condition reaches the session boundary; the session observes `uncaught-condition: <name>` (payload and span attached), with all intervening cleanups already run.

* Cleanup runs in every exit direction: normal return, `return` escape, condition propagation, restart transfer — always innermost-first, and always before the exit cause is delivered outward.

## 9. Capability interaction

* A handler runs under the capability set of the *protected body* (the dynamic capability context in effect at signalling). Handling a condition grants nothing: `with-handlers` cannot widen capabilities, and a restart body executes under the caller's current capability set.

* Inside a handler/cleanup, calling a host primitive the current capability set denies returns the loom contract's denial marker `capability-denied:` (`contribs/native/loom/src/lib.rs:166, 198`) rather than raising an unhandled host error; the denial is a value the handler may inspect or convert to a condition.

## 10. Budget interaction

* Condition signalling, handler dispatch, and cleanup execution all **consume fuel** (execution budget, `docs/superpowers/specs/2026-10-11-zio-language-toolchain-design.md` §7.4).

* Fuel exhaustion **is itself a condition** (`fuel-exhaustion`) and may be handled: a handler can run bounded recovery (e.g. produce a partial result) using remaining fuel.

* A handler **cannot extend fuel**: fuel is an external budget owned by the host at request/image level, not a restartable resource. `invoke-restart 'extend-fuel ...` therefore does not exist; any attempt to allocate beyond the budget still terminates with exhaustion (reported after cleanups).

* Cleanup that exceeds the remaining fuel runs as far as fuel allows; the fuel error is reported as the exit cause when it preempts cleanups.

## 11. Cancellation interaction

* Cancellation is distinct from condition signalling: it is a runtime request to stop, not a value-producing mechanism. When the runtime receives a cancel request, it raises a `cancelled` **condition** at the next safe point (between steps — never by tearing mid-instruction).

* Handlers MAY catch `cancelled` (e.g. to run a bounded graceful finish), but cleanup runs in BOTH caught and uncaught cases: all `unwind-protect` frames between the safe point and the cancellation root are unwound innermost-first before the results/error are delivered (`cancelled` handled → value path; unhandled → session-level `cancelled` report).

* After a handled `cancelled`, execution may continue only within the same request and only under the remaining fuel; cancellation does not reset fuel.

## 12. Three executor implementation boundaries

| Aspect                            | AST (`langs/core/src/eval.rs`)                                                                    | Explicit VM (`langs/core/src/bytecode.rs`)                                                     | Zio meta (`assets/interpreter/evaluator.zio`)                          |
| --------------------------------- | ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| Handler storage                   | dynamic handler frame stack in eval context (alongside `Arc<Env>` in `env.rs`)                    | handler table on VM stack frame                                                                | dynamic env cell (`%handlers`) in env chain (`interpreter/env :set`)  |
| Restart invocation                | captured control transfer at `restart-case` site; value substitution                              | `invoke-restart` opcode; arity check vs frame restart entries; jump to restart body            | closure over body env invoked to re-enter evaluation                   |
| Cleanup timing                    | in eval unwind loop, innermost-first, before exit delivery                                        | frame cleanup list run on any frame pop (return/signal/restart)                                | meta `unwind-protect` exit hooks on all non-local exits                |
| Span propagation                  | condition carries span; `EvalError` path preserves it (`langs/core/src/eval.rs`, `span.rs`)       | source/debug metadata via source provenance (`langs/core/src/span.rs`) attached at `signal`    | span via reader/debug metadata (`langs/core/src/reader`)               |
| Not implemented (all three alike) | — cross-request suspension, async restarts, debugger/editor integration, retry policies (see §14) | same                                                                                           | same                                                                   |

Boundary invariant: all three must agree on the observable dynamic-chain semantics of §3/§7 — a handler bound in any executor is found by the same innermost-first walk within that executor's run; there is no cross-executor handler sharing within one request (executors are alternative implementations of the same language, chosen per run as in `langs/core/tests/self_hosting.rs`).

## 13. Verification matrix

Style mirrors the language-toolchain spec §10 table. Each row: same Zio source, run on AST / VM / Zio meta, identical observable outcome.

| #  | Scenario (Zio source)                                                                         | Expected (all three executors)                                                                                                                                                        |
| -- | --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1  | §6 example: error! `read-failure`; handler invokes `use-fallback`; restart returns `fallback` | final value = result of continuing with `fallback`; cleanup trace exactly `["reads done"]`; no error reported                                                                         |
| 2  | §6 example, handler removed                                                                   | `uncaught-condition: read-failure`; cleanup trace still `["reads done"]` (cleanup before delivery)                                                                                    |
| 3  | Handler declines (returns without invoking restart)                                           | condition propagates to outer handler; outer restart applies; inner cleanup ran                                                                                                       |
| 4  | Nested `with-handlers`, inner name matches                                                    | innermost handler wins; outer handler not invoked                                                                                                                                     |
| 5  | Nested `unwind-protect` around same body                                                      | cleanup order innermost-first: `["inner" "outer"]` for every exit direction (normal, condition, restart, return)                                                                      |
| 6  | Cleanup form errors during unwinding for a condition                                          | original condition reported first, cleanup error attached; execution does not resume                                                                                                  |
| 7  | Handler calls capability-denied host primitive                                                | handler observes `capability-denied:` as a value (no unhandled host error); can signal it as a named condition                                                                        |
| 8  | Restart arity mismatch via `invoke-restart`                                                   | contract error at the `invoke-restart` / `restart-case` boundary; cleanups still run                                                                                                  |
| 9  | Fuel exhausted inside protected body                                                          | `fuel-exhaustion` condition raised; a fuel handler may run bounded recovery; recovery cannot extend fuel; cleanups run                                                                |
| 10 | Cancel request at a safe point                                                                | `cancelled` condition; caught → value path after cleanups; uncaught → session `cancelled` report; both cases run cleanups                                                             |
| 11 | Lexical `let` inside handler body referenced from protected body                              | name resolution error (lexical bindings of the handler do not leak); dynamic chain DOES extend                                                                                       |
| 12 | Same §6 source through AST, VM, and meta executors                                            | identical final value, identical cleanup trace, identical reported spans                                                                                                             |

Permanent test placement after implementation (by ZIO-006): rows 1, 3, 4, 5, 9, 10, 12 in `langs/core/tests/runtime_contract.rs` style; row 6, 8 as error-path tests; meta rows follow `langs/core/tests/meta_interpreter_contract.rs` conventions; cross-executor row 12 uses the `langs/core/tests/self_hosting.rs` triple-path pattern.

## 14. Explicit non-goals

* **Async restarts** — no asynchronous/threaded condition or deferred restart; all handling is synchronous within one evaluation. Right home: ZIO-012.

* **Cross-request suspension** — an image saved mid-condition (condition + handler frame state serialized for a later request) is NOT part of this slice; image save/restore (`langs/core/src/image/{save,restore}.rs`) round-trips whole images, not in-flight conditions. Right home: ZIO-012.

* **Debugger integration** — no debugger-facing condition protocol (break on signal, interactive restart menu). Right home: future ZIO-005 slices / debugger task.

* **Editor integration** — no LSP/editor surfaces for restart selection. Right home: editor integration task.

* **Retry policies** — no automatic retry/backoff policy layer; a retry is expressed as a restart that re-invokes the failing call, bounded by fuel as always. Right home: future slice if AI-harness needs it.

* **Handler inheritance across requests** — dynamic chain is per-request; no persisted handler registry.
