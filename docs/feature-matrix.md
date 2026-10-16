# Zio Feature Matrix

This is the authoritative implementation-status table for public Zio
documentation. Repository counts are generated in [Project Status](status.md),
and the reproducible AST evaluator measurement is documented in the
[AST Evaluator Baseline](../benchmarks/README.md).

The current layout contract is the [language-first plan](superpowers/plans/2026-10-07-language-first-layout.md) and [architecture](zio-architecture.md).
Zio libraries live in `libs/`; Numa, Rill and Loom are Zio library directions, not a target of independent Rust business libraries. Existing generic Rust host/transport contracts remain in `contribs/native/loom/` (crate `loom`), distinct from `libs/loom/`.
Groiz is an independent project ([lszio/groiz](https://github.com/lszio/groiz)) and does not live in this repository; neither its business code nor its landing page is carried here. `apps/site/` hosts language introduction/docs/playground. The structural cutover adds no new runtime capability, self-hosted compiler, Tree-sitter or LSP evidence; post-move runtime verification is separate from the historical checks below. The ordinary language CLI does not expose `--llm-replay`.

The current language/toolchain goals and execution order are the
[2026-10-11 design](superpowers/specs/2026-10-11-zio-language-toolchain-design.md)
and [work-package plan](superpowers/plans/2026-10-11-zio-language-toolchain.md).
The old structural cutover statement above describes that migration only:
the current tree now contains a bytecode runtime, Zio compiler, self-hosting
checks, LSP and Tree-sitter resources. L00–L02 provide AST, explicit VM and
Zio metacircular contracts, native/portable-artifact smoke and rebuilt WASM
smoke; see the [language reference](language-reference.md). L02 verifies
first-stage portable compiler self-hosting. Runtime images (M00–M02),
independent native/browser/WASI application export (A00–A02) and the native JS
engine (J00) are no longer mere targets — each is a verified work package with a
dated acceptance record in the work-package table below. Still separate targets:
J01 execution-parity, the Rust engine and `inline`/raw blocks (R00/E00), and the
complete one-install distribution (D00/D01).

Status meanings:

- **Stable**: covered by the referenced unit or executable-example contract.
- **Experimental**: runnable, but its API and behavior may change.
- **Demo**: loads and runs, but demonstrates a data shape only — the named
  capability is not implemented.
- **Planned**: represented only by a placeholder, example data, specification,
  or roadmap; it is not an implemented capability.

Evidence baseline recorded 2026-10-16 against HEAD
`acdbd48cca70cac6301172442a101a61623b8134` (workspace 0.2.1), branch
`feat/paca-m1-query`, with pre-existing uncommitted README/blog/design documents.
The historical records below describe their original acceptance, not an
unmodified release checkout.

- **Acceptance below is historical.** A row's date/version is the work-package
  acceptance record from the [toolchain plan](superpowers/plans/2026-10-11-zio-language-toolchain.md);
  source presence and unit tests prove a contract exists, not that it passes
  today. The documentation audit checked source paths and historical anchors.
  Current CLI smoke observations are recorded separately below; they do not
  imply a full test suite, browser, WASI or clean-install revalidation.
- Status vocabulary: **Stable/Experimental/Demo/Planned** as defined above;
  **historically accepted** = the row anchors a dated work-package record;
  **partially implemented** = the row mixes delivered and target parts (e.g.
  A01, J00 with J01 pending); **undecided design** = a proposal not yet adopted
  (the ZIO-00x design tasks below, and the [ecosystem review](language-ecosystem-review.md)
  recommendations).
- **Outstanding rechecks are named below** — notably A01 real-browser application
  acceptance, A02's pinned wasmtime run, clean developer-install resource closure
  and long-session lifecycle. Current-run observations remain separately qualified.

| Area | Status | Evidence |
| --- | --- | --- |
| Reader and syntax expansion | Stable | reader unit tests |
| `eval` primitive (code-as-data evaluation) | Experimental | [`langs/core/src/builtins/macroexpand.rs`](../langs/core/src/builtins/macroexpand.rs) — evaluates in the global environment (CL-style); required by the learner's homoiconic loop |
| AST evaluator and closures | Stable | core evaluator tests |
| Structured query and human-readable help | Experimental / ZIO-004 native smoke verified | [`query.rs`](../langs/core/src/builtins/query.rs), [`query_metadata.rs`](../langs/core/src/builtins/query_metadata.rs): shared describe/help/apropos/module-exports/capabilities; live binding/native identity, exact registered source revisions, source-comment examples, no target/example/module-loader execution. [`query_contract.rs`](../langs/core/tests/query_contract.rs): 11 passing behavior contracts, including AST/meta/explicit VM, portable-source provenance and metadata boundaries. Actual CLI script and interactive REPL exercised via [`query.zio`](../examples/toolchain/query.zio); remote restart remains false. See [query contract](ai-language-interaction.md#4-查询合同-v1zio-003). |
| Numeric comparison and arithmetic | Stable | [`langs/core/tests/runtime_contract.rs`](../langs/core/tests/runtime_contract.rs) — `Int`/`Int` compares exactly (no f64 widening, so `(< 9007199254740993 9007199254740994)` is true); mixed `Int`/`Float` compares without converting the integer, with both operand orders and exact `-2^63` equality covered across `<`, `>`, `<=`, `>=`; non-finite operands are refused rather than ordered; integer division still truncates; scalar `sqrt` takes one finite non-negative number and returns a Float |
| Shared runtime assembly (`bootstrap`) | Experimental | [`langs/core/src/bootstrap.rs`](../langs/core/src/bootstrap.rs) — `language_context` / `eval_source` are the single assembly path for CLI, compiler front end, and generated-candidate execution; stdlib load failure returns `Err` instead of a warning; `require` resolves only inside explicitly granted `ModuleRoots` and refuses a symlink out of a root; a nested module registers its source in the caller's SourceMap, so its errors name its own file ([`langs/core/tests/runtime_contract.rs`](../langs/core/tests/runtime_contract.rs)) |
| Execution observer (opt-in port on the evaluator) | Experimental | [`langs/core/src/observer.rs`](../langs/core/src/observer.rs) — reports branches taken, calls made, macro expansions performed, and errors raised, each with the span it happened at. **Off costs nothing**: the payload is not constructed, the string is not built, the counter is not touched, and the observed and unobserved runs produce identical values and errors (measured, not asserted — 9 payloads with an observer, 0 without). A program that *prints* the word "trace" produces no events: only the evaluator can write one ([`langs/core/tests/observer_contract.rs`](../langs/core/tests/observer_contract.rs), 10 contracts) |
| Source-preserving syntax bridge (`SyntaxNode`) | Experimental | [`langs/core/src/syntax.rs`](../langs/core/src/syntax.rs) — tagged `kind`/`value`/`children`/`span`/`origin` map; `Source`/`Quoted`/`Generated`/`MacroCall` stay distinct; a value that is not a node (or lies about its shape) is refused rather than converted |
| Set literal `#{…}` (map-backed `set`) | Experimental | the reader expands `#{a b c}` to `(set a b c)` and the bootstrap stdlib provides the map-backed `set` (`langs/core/assets/std/core.zio`); dedup and membership covered in [`langs/core/tests/runtime_contract.rs`](../langs/core/tests/runtime_contract.rs); set operations (`conj`/`disj`/`merge`) live in [`libs/std/persistent.zio`](../libs/std/persistent.zio) |
| Quasiquote `` ` `` / unquote `~` / unquote-splicing `~@` | Experimental | reader dispatch wraps to `(quasiquote …)`/`(unquote …)`/`(unquote-splicing …)` ([`langs/core/src/reader/reader.rs`](../langs/core/src/reader/reader.rs)); the evaluator walks the template with depth tracking — `~` evaluates at depth 1, `~@` splices a list/vector, nested backquotes protect their own unquotes, and bare `~`/`~@` outside a template is refused ([`langs/core/src/special/data.rs`](../langs/core/src/special/data.rs), contracts in [`langs/core/tests/runtime_contract.rs`](../langs/core/tests/runtime_contract.rs)); [`examples/macros.zio`](../examples/macros.zio) templates use it |
| Core macros and stdlib | Stable | [`examples/macros.zio` contract](../examples/macros.zio) |
| ZOS classes and generic dispatch subset | Experimental | [`examples/zos-concept.zio` contract](../examples/zos-concept.zio) |
| Persistent collection library | Experimental | [`libs/std/persistent.zio`](../libs/std/persistent.zio) — all functions runnable; map iteration order is undefined |
| Datalog storage (create-db, transact) | Experimental | [`libs/std/datalog.zio`](../libs/std/datalog.zio) — tx-data must be a list of lists |
| Datalog query evaluator | Experimental | [`libs/std/datalog.zio`](../libs/std/datalog.zio) — conjunctive triple unification with joins, `_` wildcards, repeated-variable and nil unification, `:in` scalar inputs, distinct projection in datom/input order; rules, predicates, aggregates and negation are refused, not silently ignored ([`langs/cli/tests/libs/datalog.zio`](../langs/cli/tests/libs/datalog.zio)) |
| Protocol system (`defprotocol`/`extend-type`) | Experimental | [`libs/std/protocol.zio`](../libs/std/protocol.zio) — dispatches via ZOS generic functions |
| Entity model (`defentity`/`make-entity`) | Experimental | [`libs/std/entity.zio`](../libs/std/entity.zio) — ids are process-unique counter strings; globally unique identity awaits a core primitive |
| Agent framework | Experimental | [`libs/loom/agent.zio`](../libs/loom/agent.zio) — the agent's own decision logic in Zio: `agent-run` loops, asks for a program, checks the execution outcome, retries on failure and stops at the grant's turn ceiling. A provider failure and a grant refusal are turn *outcomes*, so the Zio logic — not an unwound error — decides whether to continue; the host bindings it calls are supplied by the embedding application ([`langs/cli/tests/libs/agent.zio`](../langs/cli/tests/libs/agent.zio)) |
| AI host protocols (now Loom: `ModelHost`/`LlmHost`/`EmbedHost`, `llm-complete`/`embed`) | Experimental | [ADR-016](adrs.md), [`contribs/native/loom/src/lib.rs`](../contribs/native/loom/src/lib.rs) — external attach via `install` (core untouched); no host → stable `capability-denied:` error; mock record/replay with fail-fast replay-miss ([`contribs/native/loom/tests/contract.rs`](../contribs/native/loom/tests/contract.rs)); OpenAI-compatible HTTP behind the `http` feature ([`contribs/native/loom/src/http.rs`](../contribs/native/loom/src/http.rs)). `llm-complete` is an extension binding Loom installs from outside — core knows nothing about it |
| LLM proposer library (`libs/loom/proposer.zio`) | Experimental | [`langs/cli/tests/libs/proposer.zio`](../langs/cli/tests/libs/proposer.zio) contract + [`contribs/native/loom/tests/proposer_contract.rs`](../contribs/native/loom/tests/proposer_contract.rs) — data prompt template, line response protocol, deterministic correction retry, `make-llm-proposer` over `llm-complete`; parse 对拍 with the Rust mirror in `loom::mock::test_support` |
| Threading macros (`->`/`->>`) | Stable | [`langs/core/assets/std/core.zio`](../langs/core/assets/std/core.zio) — first/last-argument threading over any value (ZOS objects, maps, JS handles, functions); zero runtime semantics, expansion-time construction; contracts in [`langs/core/tests/runtime_contract.rs`](../langs/core/tests/runtime_contract.rs); chaining design is [ADR-020](adrs.md) |
| First-class JS values + dotted `js/*` calls | Experimental | [`langs/core/src/wasm.rs`](../langs/core/src/wasm.rs) — wasm-only; `js/eval` maps primitives to Zio values and wraps remaining JS values as opaque `#<js-object>` ZOS objects with unique identity, chainable via `->`; dotted `js/<global.path>` symbols (`(js/console.log "x")`, `(js/Math.max 1 2)`) resolve against globalThis with `this` bound to the parent; composite Zio data deep-converts to plain JS data via JSON rules while ZOS objects are refused at the boundary; verified in-browser against the real DOM and global functions |
| Concurrent primitives (`future-call`, `chan`) | Experimental — synchronous placeholder | [ADR-012](adrs.md): nothing spawns a thread; `future-call` evaluates eagerly. Real concurrency is a deferred decision |
| Module system (`module`/`export`/`require`, `ns/name`, `:as`, `:refer`) | Experimental | [`langs/cli/tests/modules.rs`](../langs/cli/tests/modules.rs) — exports enforced on refer and qualified access; file modules resolve inside granted roots (`--lib-dir`, `ZIO_PATH`, script dir, cwd — assembled by the CLI, never read by core) |
| File I/O through IoHost (`load`/`slurp`/`spit`/`file-exists?`) | Experimental | [ADR-011](adrs.md); `BufferIoHost` offers an in-memory FS for tests/sandboxes |
| WASM build + landing-page REPL | Experimental | [`langs/core/src/wasm.rs`](../langs/core/src/wasm.rs), [`apps/site/src/pages/index.astro`](../apps/site/src/pages/index.astro), [`apps/site/src/lib/engine.ts`](../apps/site/src/lib/engine.ts) |
| Bytecode runtime and Zio compiler | Experimental | [`langs/core/tests/toolchain_contract.rs`](../langs/core/tests/toolchain_contract.rs): 29 AST/explicit-VM/Zio-meta and source contracts. [`self_hosting.rs`](../langs/core/tests/self_hosting.rs): 4 contracts, including complete frontend installation, Stage 1/2/3 canonical byte equality, known answers and malformed artifacts. [`bytecode.rs`](../langs/core/src/bytecode.rs) preserves source revisions across serialization. Portable modules are not runtime images. |
| Optimizing ZIR, JIT and direct machine-code code generation | Planned | Separate from the existing bytecode runtime and first-stage bytecode/runtime application packaging; see the [current design](superpowers/specs/2026-10-11-zio-language-toolchain-design.md). |
| Zio metacircular reader/evaluator/expander | Experimental | [`langs/core/assets/interpreter/reader.zio`](../langs/core/assets/interpreter/reader.zio), [`evaluator.zio`](../langs/core/assets/interpreter/evaluator.zio), [`expand.zio`](../langs/core/assets/interpreter/expand.zio); explicit `zio-meta-read` / `zio-meta-eval` entries with unchanged default executor. [`langs/core/tests/meta_interpreter_contract.rs`](../langs/core/tests/meta_interpreter_contract.rs) has 17 contracts for Unicode/source revisions, callbacks without target Stage 0 eval, recursion, macro semantics, module authorization and synchronous future/chan/ZOS behavior. [`syntax_atom_contract.rs`](../langs/core/tests/syntax_atom_contract.rs) covers strict syntax atoms and registered UTF-8 byte spans. [Language reference](language-reference.md) records runtime evidence and limits. |
| Complete first-stage compiler self-hosting | Experimental / L02 verified (accepted 2026-10-11) | Zio reader/expander/evaluator plus [`analyze.zio`](../langs/core/assets/compiler/analyze.zio) and compiler build byte-identical Stage 1/2/3 artifacts in fresh contexts. Actual `self-build --verify` confirms compiled entries, sum=10 and closure=15; omitted helpers cannot fall back to Stage0. Rust bootstrap/VM remain; no Rust-free or full-distribution claim. |
| Runtime images | Experimental / M00–M02 verified (accepted 2026-10-11–12) | [`langs/core/src/image/`](../langs/core/src/image/): wire format with bounded validation, identity interning, save of explicit roots with parent chains and the module table, two-pass restore committing into a fresh context. `zio image save/inspect/run` and `zio compile -o` in [`langs/cli/src/commands.rs`](../langs/cli/src/commands.rs); restore runs under host grants (root-limited I/O), outside the source tree. [`image_contract.rs`](../langs/core/tests/image_contract.rs): 23 contracts; [`tools/smoke-toolchain-artifacts.sh`](../tools/smoke-toolchain-artifacts.sh) proves cross-process save/inspect/run, canary refusal and granted file access. Bundled applications (A00–A02) are separate. |
| Independent native/browser/WASI application export | A00–A02 verified (accepted 2026-10-12–13; A01 real-browser DOM acceptance outstanding) | A00: `zio build INPUT --target native -o APP` produces one executable (runtime + JSON payload + checksummed footer) from source, `.zioc` or `.zio-image`; [`bundle_contract.rs`](../langs/cli/tests/bundle_contract.rs) runs it with no checkout and no zio on PATH (49 + dynamic eval 50, image entry keeps restored state), refuses damaged payloads and undeclared modules. A01: the same payload contract runs in the browser — `--target browser` emits one HTML (glue URL + inline payload) and `ZioSession.run_bundle` executes it in the wasm engine ([`tools/a01-browser-harness.mjs`](../tools/a01-browser-harness.mjs) proves 49/50 through the shipped engine). A02: [`langs/wasi`](../langs/wasi) builds a WASI-imports-only runtime (payload compiled in per app); `wasmtime run` produces 49/50, the image entry prints 1, file access needs preopen **and** `--grant`, and browser-only `js/*` dependencies are refused at build time. AOT compilation remains a separate target. |
| Groiz application | Maintained in its own repository | Groiz code, docs and its landing page live in [lszio/groiz](https://github.com/lszio/groiz); this repository neither carries nor builds them. The Zio site links to that repository instead of carrying Groiz content. |
| External JS/Rust engine libraries and `inline` | Native JS delivered (J00, accepted 2026-10-13, rquickjs 0.11); J01/R00/E00 Planned | J00: [`contribs/native/js`](../contribs/native/js) binds a QuickJS realm (rquickjs 0.11, MIT) with identity-checked handles, exact BigInt crossing, non-finite refusal, interrupt and heap-cap limits, and no host authority; [`libs/interop/js.zio`](../libs/interop/js.zio) is the Zio wrapper and [`examples/interop/js.zio`](../examples/interop/js.zio) the acceptance program (16 engine + Zio contracts). Compiled-execution parity, browser DOM/Worker isolation and the Rust engine (R00) remain separate targets. |
| Complete one-install Zio development distribution | Partially implemented; complete acceptance pending | D00/D01: existing installer includes interpreter/compiler, LSP, syntax resources, libraries, docs and native JS; browser WASM ships separately. One-install resource closure, optional offline Rust engine pack and clean-environment acceptance remain targets. See [release scope](release.md). |
| Source formatter (`zio fmt`) | Experimental | [`langs/core/src/fmt.rs`](../langs/core/src/fmt.rs) is a layout pass over the token stream — atoms are reproduced exactly as read, `;` comments are attached to the forms they follow, and the output is idempotent (contracts in `fmt.rs` and [`langs/cli/tests/fmt_contract.rs`](../langs/cli/tests/fmt_contract.rs), including a whole-repository idempotence walk). `zio fmt` formats stdin or files with `--write`/`--check`; the LSP answers `textDocument/formatting` with the same formatter. |
| `zio lsp` language-server delegation | Experimental | `zio lsp` execs the `zio-lsp` installed beside the `zio` binary and passes the arguments through; editors may still start `zio-lsp` directly — both paths run the same server. |
| LSP and Tree-sitter implementation resources | Experimental | [`contribs/language-server/src/main.rs`](../contribs/language-server/src/main.rs), [`contribs/tree-sitter-zio/grammar.js`](../contribs/tree-sitter-zio/grammar.js); current source exists, complete installation/inline injection/protocol coverage remains separately verified. |
| Process capability and pacman updater | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Numeric kernel and scientific API | Planned | [approved applications design](superpowers/specs/2026-08-10-zio-vm-applications-site-design.md) |
| Pipeline DSL (`pipeline` macro) | Experimental (MVP) | [`libs/std/pipeline.zio`](../libs/std/pipeline.zio) — filter/map/aggregate-count/emit stages; expansion errors for unknown stages; compile-time ExpandError semantics remain Planned |
| Homoiconic learner (`learn-function`) | Experimental | [`libs/learning/learn.zio`](../libs/learning/learn.zio) — generation loop over the proposer protocol ([ADR-016](adrs.md)): untrusted proposers emit candidate strings; reader + closed-world whitelist gates, canonical dedup, eval budget (`:max-generations`/`:max-evals`) and per-candidate error isolation live in the loop; enumerator demoted to `make-enum-proposer`, `make-llm-proposer`/`make-hybrid-proposer` in [`libs/loom/proposer.zio`](../libs/loom/proposer.zio); contracts in [`langs/cli/tests/libs/learn.zio`](../langs/cli/tests/libs/learn.zio) + [`contribs/native/loom/tests/learn3_contract.rs`](../contribs/native/loom/tests/learn3_contract.rs). Seeded search, compile cache, fuel-budgeted sub-VM remain Planned |
| Feedback policy and teacher protocol | Experimental | Feedback policy is pure Zio ([`libs/learning/learn/feedback.zio`](../libs/learning/learn/feedback.zio)): field-scoped precedence, human-over-teacher, conflict isolation, abstain as a first-class value, frozen dataset revisions. Teacher protocol in [`contribs/native/loom/src/teacher.rs`](../contribs/native/loom/src/teacher.rs) with capability declaration, soft-output vocabulary alignment, license checks and proposal-typed programs; HTTP adapter + 16 contracts in [`contribs/native/loom/tests/teacher_contract.rs`](../contribs/native/loom/tests/teacher_contract.rs). The store, artifact and role-gated persistence layer this policy was originally exercised against lived in the separate Groiz project |
| Self-learning acceptance task (geometry image + sensor XOR) | Experimental | [`examples/self-learning/generate.py`](../examples/self-learning/generate.py) — grouped splits (0 leakage), missing-modality abstain contract, holdout container stores no label byte; `python examples/self-learning/generate.py --self-check`; frozen gates in [`examples/self-learning/task.json`](../examples/self-learning/task.json) and [`examples/self-learning/protocol.json`](../examples/self-learning/protocol.json) |
| Joint code/weight learning data (Zio) | Experimental | the model description is Zio data and structural candidates are rewrites of it ([`libs/learning/learn/model.zio`](../libs/learning/learn/model.zio)); recipes declare consumable signals, budgets and init policy ([`libs/learning/learn/recipes.zio`](../libs/learning/learn/recipes.zio)); allocation policy for parallel populations is pure Zio ([`libs/learning/learn/population.zio`](../libs/learning/learn/population.zio)). The training driver that consumed them, with its measured lift numbers, lived in the separate Groiz project |
| Experience indexes, abstraction and reuse (Zio) | Experimental | structural, behavioural and semantic indexes over the existing signal/observation/prediction records — no second fact store; behavioural fingerprints carry a named probe set and are never compared across sets; semantic vectors carry an encoder id and a **space version**, and a query at a different version is refused without an explicit migration. Four refusals are enforced and tested: a semantic near-neighbour never merges into a different AST, probe sets never mix, retrieval never crosses a licence, and a capability derived from a retracted source refuses to load. Abstraction promotion is gated on original-task regression AND a new-task evaluation ([`libs/learning/memory.zio`](../libs/learning/memory.zio), [`libs/numa/vector.zio`](../libs/numa/vector.zio)); 28 + 23 library markers in [`langs/cli/tests/libs/memory.zio`](../langs/cli/tests/libs/memory.zio) and [`vector.zio`](../langs/cli/tests/libs/vector.zio). The record store and the negative description-length measurement lived in the separate Groiz project |
| ZOS retained as core; toolchain self-hosting direction | Adopted / Further implementation planned | Existing compiler/VM/staged checks are listed above. [Current design](superpowers/specs/2026-10-11-zio-language-toolchain-design.md) and L00–L02 require the Zio reader/evaluator/front end and full first-stage semantic coverage; editor or code-as-data examples do not replace that acceptance. |
| Numa official standalone computation library | Planned | [Architecture boundaries](zio-architecture.md), plan C00–C02. Independently versioned/installed arrays, numeric/vector/matrix and backend interfaces, consumed by applications and a numerical program. Existing vector/CPU components are starting points; current float ordering/cosine scale need correction. No GPU/native-speed claim. |
| Rill CLI library component | Experimental source / Standalone release planned | [`libs/rill/cli.zio`](../libs/rill/cli.zio) implements data-driven options/subcommands/help parsing. This documentation update did not run its contract; independent versioned release remains a distinct target, not evidence the library directory is absent. |
| Loom native adapter and structured harness contract | Experimental | [ADR-019](adrs.md), plan H00. Historical H00 migrated `ai/` to `loom/`; the native adapter now resides in `contribs/native/loom/` (no compatibility re-export, no parallel transport). [`contribs/native/loom/src/harness.rs`](../contribs/native/loom/src/harness.rs): `ModelHost::respond(&ChatRequest, &Budget)` is the only provider port, with `ToolCall`/`ChatMessage`/`Usage`/`ChatResponse`; external content cannot become an instruction *structurally* (the caller role set is user/assistant/tool and `system` is the harness's own, in first position only) — a mock echo is not the evidence, a real controlled HTTP endpoint is ([`contribs/native/loom/tests/harness_contract.rs`](../contribs/native/loom/tests/harness_contract.rs), 14 contracts) |
| Loom budgets and cancellation | Experimental | [`contribs/native/loom/src/harness.rs`](../contribs/native/loom/src/harness.rs) — a call is admitted only if its worst case (`max(expected, per_call_ceiling)`) is already reserved; `settle` releases the unused part and `settle_unknown` keeps the whole hold and reports `unknown-cost` (an unpriced provider is not a free call); `Budget` is deliberately not `Clone` because a copy is a second ledger with the same ceiling, and `with_shared_budget` makes several views charge the same counters; a cancelled session makes no further provider call |
| Loom transcript-keyed replay | Experimental | [`contribs/native/loom/src/mock.rs`](../contribs/native/loom/src/mock.rs) — a recording is keyed on the whole conversation (every role/content/tool call/result), the tool schemas, the output cap, and the sampling options, so a replayed call that silently used a different temperature is not mistaken for the recorded one; a miss is still a hard `replay-miss` |
| Loom real HTTP transport | Experimental | [`contribs/native/loom/src/http.rs`](../contribs/native/loom/src/http.rs) — sends the whole conversation (system role included, as the harness's own), reads tool calls and `finish_reason` back, and maps every failure to a typed error. Verified against a controlled OpenAI-compatible endpoint: the second request on the wire carried `user` → `assistant(tool_calls=[search])` → `tool(for=call-abc)`, i.e. the tool result is correlated to the call that produced it |
| Loom bidirectional ACP | Planned | [ADR-019](adrs.md), plan H00/G06/G07. Use the native adapter for hosted contracts; models/tools/sessions/budgets/cancel, ACP client then server. Loom does not own evaluation/approval. |
| Concurrency model decision | Adopted (ADR-014) | threads bind to the VM phase; AST interpreter stays single-threaded; `Value` !Send is verified |
| Memory model (Arc cycles) | Known, anchored (ADR-015) | self-referential closures leak by design until the VM-phase GC decision |
| Landing site | Stable | Astro static build: [`apps/site/src/pages/index.astro`](../apps/site/src/pages/index.astro) (landing), [`apps/site/src/pages/agent/index.astro`](../apps/site/src/pages/agent/index.astro) (Agent & LLM), wasm engine committed at `apps/site/public/wasm`, rebuilt with `tools/build-wasm.sh` |
| Groiz landing page | Maintained in lszio/groiz | The Groiz exposition page left this repository with the product rename; the site links to [lszio/groiz](https://github.com/lszio/groiz) instead of carrying Groiz content |

The stable reader row now includes the set literal: `#{…}` reads as a
map-backed `set` from the bootstrap stdlib, and quasiquote (`` ` ``/`~`/`~@`)
is an Experimental evaluator feature — both are contract-covered (see the
rows above).

## Work-package acceptance and recheck baseline (L00–L02 / M00–M02 / A00–A02 / J00–J01 / R00 / E00 / D00–D01)

Dates and versions are the work-package acceptance records from the
[toolchain plan](superpowers/plans/2026-10-11-zio-language-toolchain.md);
the recheck commands reproduce the recorded observation. This table retains
the historical record; the separate current-run section identifies which
commands were exercised on 2026-10-16.

| Package | Status | Accepted (date / version) | Primary evidence | Recheck entrypoints |
| --- | --- | --- | --- | --- |
| L00 semantics & execution baseline | Historically accepted | 2026-10-11 | [`langs/core/tests/toolchain_contract.rs`](../langs/core/tests/toolchain_contract.rs) (28 contracts), [`examples/toolchain/semantics.zio`](../examples/toolchain/semantics.zio) (16-line output); 238 tests | `cargo test -p zio-core --test toolchain_contract`; `cargo run -p zio-cli -- examples/toolchain/semantics.zio` |
| L01 metacircular reader/eval/expand | Historically accepted | 2026-10-11 | [`langs/core/assets/interpreter/`](../langs/core/assets/interpreter/) `reader.zio`/`evaluator.zio`/`expand.zio`; [`meta_interpreter_contract.rs`](../langs/core/tests/meta_interpreter_contract.rs) (17); 258 tests | `cargo test -p zio-core --test meta_interpreter_contract`; `cargo run -p zio-cli -- examples/toolchain/meta-eval.zio` |
| L02 staged self-hosting | Historically accepted | 2026-10-11 | [`self_hosting.rs`](../langs/core/tests/self_hosting.rs) (4); `self-build --verify`: Stage 1/2/3 canonical byte-equal (739,890 B each), sum=10, closure=15 | `cargo run -p zio-cli -- self-build --verify`; `bash tools/smoke-self-build.sh` |
| M00 image graph format | Historically accepted | 2026-10-11 | [`langs/core/src/image/mod.rs`](../langs/core/src/image/mod.rs), `wire.rs`; [`image_contract.rs`](../langs/core/tests/image_contract.rs) (13) | `cargo test -p zio-core --test image_contract` |
| M01 save/restore | Historically accepted | 2026-10-12 | `image/` `save.rs`/`restore.rs`; `image_contract.rs` (22); [`examples/toolchain/image-session.zio`](../examples/toolchain/image-session.zio) | `cargo test -p zio-core --test image_contract` |
| M02 image CLI + restore grants | Historically accepted | 2026-10-12 | [`langs/cli/src/commands.rs`](../langs/cli/src/commands.rs) (`image save/inspect/run`, `compile -o`); `image_contract.rs` (23); [`tools/smoke-toolchain-artifacts.sh`](../tools/smoke-toolchain-artifacts.sh) (save 538,764 B, 350 nodes; canary refusal; grant file read) | `bash tools/smoke-toolchain-artifacts.sh`; `zio image save/inspect/run` against `examples/toolchain/image-session.zio` |
| A00 native bundle | Historically accepted | 2026-10-12 | [`langs/cli/src/bundle.rs`](../langs/cli/src/bundle.rs); [`bundle_contract.rs`](../langs/cli/tests/bundle_contract.rs) (4): 49 + dynamic eval 50, checksum refusal, undeclared-module build refusal | `zio build examples/toolchain/app.zio --target native -o /tmp/zio-app && /tmp/zio-app`; `cargo test -p zio-cli --test bundle_contract` |
| A01 browser bundle | Partially accepted — real-browser DOM acceptance **outstanding** | 2026-10-12 | [`langs/core/src/bundle.rs`](../langs/core/src/bundle.rs) + `ZioSession.run_bundle`; [`tools/a01-browser-harness.mjs`](../tools/a01-browser-harness.mjs) (49/50 through the shipped engine, Node WASM channel — not browser rendering) | Build HTML with `cargo run -p zio-cli -- build examples/toolchain/app.zio --target browser -o /tmp/zio-app.html`, then `node tools/a01-browser-harness.mjs "$PWD/apps/site/public/wasm/zio_core.js" "$PWD/apps/site/public/wasm/zio_core_bg.wasm" /tmp/zio-app.html`; outstanding: load HTML in a real browser and observe DOM + console |
| A02 WASI bundle | Historically accepted (wasmtime-cli 24.0.0) | 2026-10-13 | [`langs/wasi`](../langs/wasi) (`zio-wasi`, WASI-imports-only); [`tools/build-wasi.sh`](../tools/build-wasi.sh) (43 KB runtime template); `bundle_contract.rs` (9: native 4, engine 1, WASI 3, refusal 1) | `zio build examples/toolchain/app.zio --target wasm-wasi -o /tmp/zio-app.wasm && wasmtime run /tmp/zio-app.wasm`; `bash tools/build-wasi.sh` |
| J00 native QuickJS engine | Historically accepted (rquickjs 0.11 / QuickJS, MIT) | 2026-10-13 | [`contribs/native/js`](../contribs/native/js) (`zio-js`; identity-checked handles, BigInt, limits, no host authority); [`js_contract.rs`](../contribs/native/js/tests/js_contract.rs) (11) + [`interop_contract.rs`](../contribs/native/js/tests/interop_contract.rs) (5); [`examples/interop/js.zio`](../examples/interop/js.zio) | `cargo run -p zio-cli -- examples/interop/js.zio`; `cargo test -p zio-js` |
| J01 JS cross-execution parity | Not implemented | — | plan J01 checklists unstarted (compiled-parity, browser DOM/Worker isolation) | none yet; continuation → ZIO-015 |
| R00 Rust engine + stable ABI | Not implemented | — | plan R00 checklists unstarted (versioned C ABI, offline engine pack) | none yet; continuation → ZIO-021 |
| E00 inline / raw blocks / tooling | Not implemented | — | plan E00 checklists unstarted (inline macro, `#"TAG` raw strings, injections, LSP) | none yet; continuation → ZIO-021 |
| D00 complete default distribution | Partially implemented; complete acceptance pending | — | `tools/install.sh` and release workflow assemble native development resources; browser assets remain separate, optional Rust pack absent | `ZIO_PROFILE=debug bash tools/install.sh /tmp/zio-install`; clean-install/full-resource checks → ZIO-016 |
| D01 all-target release acceptance | Acceptance outstanding | — | Existing release CI is not a completed all-target/offline acceptance table | `bash tools/smoke-toolchain-artifacts.sh` covers native only; full matrix → ZIO-017 |

## Paca task alignment (ZIO-001..025)

The following is the baseline snapshot before new task execution, not a second live task tracker. Current task statuses and evidence live only in paca (zio project `84831034-2dfc-4d7b-8983-a2e2027d6530`); [`zio-task-list.md`](zio-task-list.md) mirrors requirements and prerequisites.

Old-package continuation per [task-list §6](zio-task-list.md): J01→ZIO-015, D00→ZIO-016, A01/A02/D01→ZIO-017, R00/E00→ZIO-021. L00–L02, M00–M02, A00, A02 and J00 are historically accepted above and are not restarted; their remaining gaps (A01 real-browser acceptance, A02 pinned wasmtime run, per-app WASI compilation without local rustc) live in the continuation tasks.

| Task | Scope | Status in this matrix |
| --- | --- | --- |
| ZIO-001 | Status/evidence baseline — this document | In progress (this edit; acceptance = traceability + recheck gap list) |
| ZIO-002 | Semantics batch decisions (D01–D08, D12–D18) | Undecided design — recommendations in [ecosystem review](language-ecosystem-review.md) §4.1–4.3 not adopted |
| ZIO-003 | Query metadata contract | Undecided design — no adopted spec |
| ZIO-004 | Structured query + REPL display | Not implemented (design pending ZIO-003) |
| ZIO-005 | condition/handler/restart/cleanup design | Undecided design (review §4.1 structured errors) |
| ZIO-006 | Synchronous local restart + diagnostics | Not implemented (design pending ZIO-005) |
| ZIO-007 | Execution harness design | Undecided design (anchors: L00 three execution surfaces, M02 CLI) |
| ZIO-008 | Minimal harness implementation | Not implemented (design pending ZIO-007) |
| ZIO-009 | Interpreter MCP service | Not implemented (pending ZIO-004 + ZIO-008) |
| ZIO-010 | GC & resource lifecycle design | Undecided design (review §4.4; ADR-014/015 deferred to VM phase) |
| ZIO-011 | Reclamation + long-session validation | Not implemented (design pending ZIO-010) |
| ZIO-012 | Pause/resume + remote restart | Not implemented (pending ZIO-006/008/011; static M00–M02 images are not activity-stack snapshots) |
| ZIO-013 | Project config, deps, lockfile | Undecided design (review §4.5) |
| ZIO-014 | LSP/format/test/diagnostics integration | Not implemented (pending ZIO-003; reuse existing `fmt.rs`/LSP rows) |
| ZIO-015 | JS native/browser + compiled parity | Not implemented (J01 continuation) |
| ZIO-016 | Complete distribution (D00) | Not implemented |
| ZIO-017 | Multi-target product + release regression (A01/A02/D01) | Not implemented; A01 real-browser DOM acceptance outstanding |
| ZIO-018 | Stdlib contracts + installable library template | Undecided design (review §4.5) |
| ZIO-019 | Debug/trace/perf baseline | Not implemented (partial anchors: `observer.rs`, `fmt.rs`, self-hosted build) |
| ZIO-020 | Version binding & stable release policy | Undecided design |
| ZIO-021 | Trusted Rust engine + `inline` (R00/E00) | Not implemented (design pending) |
| ZIO-022 | Task/message/persistence min slice | Not implemented (P2 queue) |
| ZIO-023 | Compute/model/tool backend adapters | Not implemented (P2 queue; Numa/Loom rows are starting points) |
| ZIO-024 | AI usage + external-app acceptance samples | Not implemented (P2 queue) |
| ZIO-025 | Measurement-driven advanced optimization | Not implemented (P2 queue; JIT/AOT/MOP rows stay Planned) |

## Current-run evidence — 2026-10-16

Linux x64, workspace 0.2.1, revision and dirty-tree qualification as above.
These are CLI observations, separate from the historical contract counts.

| Executed command | Observed result | Limit |
| --- | --- | --- |
| `cargo run -p zio-cli -- examples/toolchain/semantics.zio` | 16 expected output lines, including `15`, `10`, `(+ 3 1)`, `true`, `500500`, `120`, and exact i64 maximum | Source-loading path; not proof of independent VM execution |
| `cargo run -p zio-cli -- examples/toolchain/meta-eval.zio` | Same 16 lines via explicit metacircular example | Not a full three-executor regression suite |
| `bash tools/smoke-self-build.sh` | Stage 1/2/3 each 739890 bytes; exact canonical byte equality; fresh compiled entries; `sum=10 closure=15` | Rust bootstrap and VM remain |
| `cargo run -p zio-cli -- examples/interop/js.zio` | Exact BigInt `9007199254740993`, callback `42`, `no process: true` | Native QuickJS only; J01 parity not checked |
| `bash tools/smoke-toolchain-artifacts.sh` | Save/inspect/run, grants/canary refusal, compilation, standalone native apps and damaged-payload refusal passed | No browser/WASI or clean developer-install check |

Outstanding: real-browser application/Worker acceptance, pinned WASI run,
fresh-install resource closure and long-session reclamation. No current-run
claim is made for those targets.

### Query implementation verification (ZIO-004)

The same HEAD plus the uncommitted query implementation was exercised after the
baseline above; this is not a newly tagged release or a complete M1 claim.

| Executed check | Observed result | Boundary |
| --- | --- | --- |
| `cargo test --workspace --no-fail-fast` | 391 passed, 0 failed, 0 ignored across 38 test suites (including zero-test/doc-test targets); query contract 11 passed | Default feature set; not every optional transport feature |
| `cargo run -p zio-cli -- examples/toolchain/query.zio` and interactive REPL | Help, structured native/source descriptions, examples, search and not-loaded module results; `PASS query metadata`, explicit call 42 | Querying did not execute the target; subsequent explicit call did |
| Native build of `query.zio`, run from `/tmp` with an empty environment and no checkout on PATH | `PASS query metadata` and 42 | Native application smoke, not complete developer-install acceptance |
| `bash tools/build-wasm.sh`; Node smoke of the resulting engine | Query example, plain comments, class kind and capability flags passed | Browser engine rebuilt from current core |
| Chromium playground submit | Structured result `[:native false]`; screenshot captured | Existing browser IoHost writes help to console, not the page result pane; no new output-capture claim |
| `cargo fmt --all --check` | Passed | Rust formatting only |
| `cargo clippy -p zio-core --lib --message-format=json` | No diagnostics in the two new query modules; 114 other warnings remain | **Strict workspace Clippy is not green**; existing Arc/non-Send/Sync and other warnings were not suppressed or broadly refactored |
| CI-parity Clippy (`.github/workflows/ci.yml` flags, `RUST_MIN_STACK=134217728`) | Passed across the workspace and all targets | Uses the repository's existing explicit lint allow-list; does not claim the bare strict invocation is clean |
| `bun run build` in `apps/site` | 93 static pages generated | Existing Shiki/MDX warnings remain; not a deployment |

The query contract and implementation passed specification then code-quality
review by an OMP `baocloud/DeepSeek-V4-flash` child. Runtime harness, MCP,
condition/restart, long-session GC and remote resume remain separate tasks.
