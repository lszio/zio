# Zio Feature Matrix

This is the authoritative implementation-status table for public Zio
documentation. Repository counts are generated in [Project Status](status.md),
and the reproducible AST evaluator measurement is documented in the
[AST Evaluator Baseline](../benchmarks/README.md).

Status meanings:

- **Stable**: covered by the referenced unit or executable-example contract.
- **Experimental**: runnable, but its API and behavior may change.
- **Demo**: loads and runs, but demonstrates a data shape only — the named
  capability is not implemented.
- **Planned**: represented only by a placeholder, example data, specification,
  or roadmap; it is not an implemented capability.

| Area | Status | Evidence |
| --- | --- | --- |
| Reader and syntax expansion | Stable | reader unit tests |
| `eval` primitive (code-as-data evaluation) | Experimental | [`core/src/builtins/macroexpand.rs`](../core/src/builtins/macroexpand.rs) — evaluates in the global environment (CL-style); required by the learner's homoiconic loop |
| AST evaluator and closures | Stable | core evaluator tests |
| Core macros and stdlib | Stable | [`examples/macros.zio` contract](../examples/macros.zio) |
| ZOS classes and generic dispatch subset | Experimental | [`examples/zos-concept.zio` contract](../examples/zos-concept.zio) |
| Persistent collection library | Experimental | [`lib/zio/persistent.zio`](../lib/zio/persistent.zio) — all functions runnable; map iteration order is undefined |
| Datalog storage (create-db, transact) | Experimental | [`lib/zio/datalog.zio`](../lib/zio/datalog.zio) — tx-data must be a list of lists |
| Datalog query evaluator | Planned | `q` is a stub: the query form is accepted but not evaluated (no unification, join, or rules) |
| Protocol system (`defprotocol`/`extend-type`) | Experimental | [`lib/zio/protocol.zio`](../lib/zio/protocol.zio) — dispatches via ZOS generic functions |
| Entity model (`defentity`/`make-entity`) | Experimental | [`lib/zio/entity.zio`](../lib/zio/entity.zio) — ids are rendered buffers; unique identity awaits a core primitive |
| Agent framework | Demo | [`lib/zio/agent.zio`](../lib/zio/agent.zio) — data model and history run; `step` echoes canned text, no LLM call or tool execution |
| AI host protocols (`zio-ai`: `LlmHost`/`EmbedHost`, `llm-complete`/`embed`) | Experimental | [ADR-016](adrs.md), [`ai/src/lib.rs`](../ai/src/lib.rs) — external attach via `install` (core untouched); no host → stable `capability-denied:` error; mock record/replay with fail-fast replay-miss ([`ai/tests/contract.rs`](../ai/tests/contract.rs)); OpenAI-compatible HTTP behind the `http` feature ([`ai/src/http.rs`](../ai/src/http.rs)) |
| LLM proposer library (`lib/zio/proposer.zio`) | Experimental | [`cli/tests/libs/proposer.zio`](../cli/tests/libs/proposer.zio) contract + [`ai/tests/proposer_contract.rs`](../ai/tests/proposer_contract.rs) — data prompt template, line response protocol, deterministic correction retry, `make-llm-proposer` over `llm-complete`; parse 对拍 with the Rust mirror in `zio_ai::mock::test_support` |
| Concurrent primitives (`future-call`, `chan`) | Experimental — synchronous placeholder | [ADR-012](adrs.md): nothing spawns a thread; `future-call` evaluates eagerly. Real concurrency is a deferred decision |
| Module system (`module`/`export`/`require`, `ns/name`, `:as`, `:refer`) | Experimental | [`cli/tests/modules.rs`](../cli/tests/modules.rs) — exports enforced on refer and qualified access; file modules via cwd or `ZIO_PATH` |
| File I/O through IoHost (`load`/`slurp`/`spit`/`file-exists?`) | Experimental | [ADR-011](adrs.md); `BufferIoHost` offers an in-memory FS for tests/sandboxes |
| WASM build + landing-page REPL | Experimental | [`core/src/wasm.rs`](../core/src/wasm.rs), [`site/index.html`](../site/index.html) |
| ZIR, bytecode VM, and JIT | Planned | [approved VM design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Process capability and pacman updater | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Numeric kernel and scientific API | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Pipeline DSL (`pipeline` macro) | Experimental (MVP) | [`lib/zio/pipeline.zio`](../lib/zio/pipeline.zio) — filter/map/aggregate-count/emit stages; expansion errors for unknown stages; compile-time ExpandError semantics remain Planned |
| Homoiconic learner (`learn-function`) | Experimental | [`lib/zio/learn.zio`](../lib/zio/learn.zio) — generation loop over the proposer protocol ([ADR-016](adrs.md)): untrusted proposers emit candidate strings; reader + closed-world whitelist gates, canonical dedup, eval budget (`:max-generations`/`:max-evals`) and per-candidate error isolation live in the loop; enumerator demoted to `make-enum-proposer`, `make-llm-proposer`/`make-hybrid-proposer` in [`lib/zio/proposer.zio`](../lib/zio/proposer.zio); contracts in [`cli/tests/libs/learn.zio`](../cli/tests/libs/learn.zio) + [`ai/tests/learn3_contract.rs`](../ai/tests/learn3_contract.rs). Seeded search, compile cache, fuel-budgeted sub-VM remain Planned |
| Concurrency model decision | Adopted (ADR-014) | threads bind to the VM phase; AST interpreter stays single-threaded; `Value` !Send is verified |
| Memory model (Arc cycles) | Known, anchored (ADR-015) | self-referential closures leak by design until the VM-phase GC decision |
| Landing site | Stable | [`site/index.html`](../site/index.html) |

The stable reader row does not include set literals: `#{...}` remains planned
and must not be used in runnable examples.
