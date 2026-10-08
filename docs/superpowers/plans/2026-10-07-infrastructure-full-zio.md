# Infrastructure and Full Zio Implementation Plan

> **For agentic workers:** Use subagent-driven-development for independent ownership slices. Parent owns integration, shared manifests, CLI/package installation, final smoke/tests/docs and cutover. Every child skips build/lint/tests/formatters mid-flight; integrated verification runs after edits settle.

**Goal:** Complete general language infrastructure, self-host the Zio compiler, and replace Grove and reusable library business implementations with Zio, including the browser controller.

**Architecture:** Follow [approved design](../specs/2026-10-07-infrastructure-full-zio-design.md). Keep minimal native language/runtime and generic platform/tensor mechanisms; no Grove operation hidden behind a native binding. Preserve old stored history and existing consumer-visible contracts; delete old business only after replacement paths execute.

**Tech Stack:** Existing Rust/Zio/Astro/WASM/SQLite/PyTorch dependencies; portable instruction runtime, Zio compiler/libraries/application, Tree-sitter grammar and stdio language service. No speculative registry, JIT, alternative compiler or UI framework.

## Ownership and shared contracts

- Foundation owner: core reader/error/span/syntax/JSON/bootstrap/module/loading/WASM and affected contracts. Do not edit evaluator/value/context without agreed handoff.
- Compiler/runtime owner: `langs/compiler/**`, new core bytecode/runtime modules, evaluator/value/ZOS integration and compiler contracts. Own `value.rs` and `eval.rs`; coordinate module registry/context changes with foundation owner. Public bytecode APIs are fixed by design §1.
- Native storage owner: new `contribs/native/host/src/storage.rs`, supporting storage/codec files and behavior contracts; use design §3 primitive names. No shared manifest/root installer edits.
- Native transport owner: new `contribs/native/host/src/transport.rs`, process/HTTP/framing/isolation helpers and behavior contracts; no Grove records/routes or manifest/root installer edits.
- Tensor owner: generic backend under `contribs/tensor/**`, native host `tensor.rs`, and narrow operation contract. No task/teacher/recipe/objective/reward policy in Python.
- Numerical/CLI/standard library owner: `libs/numa/**`, `libs/rill/**`, `libs/std/**` except `core.zio` shared with compiler only by agreement. Remove stubs/load-time prints and correct observable behavior.
- Loom/learning owner: `libs/loom/**`, `libs/learning/**`; port session/budget/tool/teacher/ACP and real recipe policy, reuse existing symbolic/feedback/population/memory/model functions.
- Grove persistence owner: `apps/grove/{contracts,codec,store,artifacts,events,lineage,feedback,governance,evaluation,approval,retention}.zio` and migration behavior contracts. Publish explicit Zio exports to orchestration owner.
- Grove orchestration owner: `apps/grove/{checkpoint,composition,ensemble,memory,coordinator,runner,inference,demo,main,http,views}.zio` and behavior contracts. Parent handles package wrapper/CLI installed integration. No old native calls permitted.
- Browser owner: `apps/grove/web/**` and Grove browser Zio controller; generic bridge only in JS. Source/controller are served by Zio HTTP entry, not Astro.
- Editor owner: `contribs/tree-sitter-zio/**`, `contribs/language-server/**`, `contribs/editors/**`; source analysis never executes user code. No need for a heavy editor frontend.
- Parent: `Cargo.toml`, package manifests, new host `src/lib.rs`, `langs/core/src/lib.rs` module declarations, `langs/cli/**`, package/install tools, final docs/status/site integration and deletion of obsolete native crates/callers.

Child functions not yet indexed require UNKNOWN confirmation by source, not an all-clear. Existing changes run symbol impact first; warn parent before HIGH/CRITICAL. Index is current at baseline 904bb52, but dynamic callable walks remain incomplete. Language server status is currently unavailable; use graph/source until the new server can serve references.

## Task 1: Fixed language/protocol foundation

- [ ] Add genuine regression contracts for JSON null, trailing/non-JSON input, escapes, finite numeric bounds, unsupported values and colliding normalized keys. Parent baseline already demonstrated failures in real CLI.
- [ ] Implement strict JSON via existing serde_json and deterministic/ordered record encoding needed by consumers; no reader-as-JSON fallback.
- [ ] Attach reader error ranges and expose a pure syntax inspection value without executing parsed input. Fix malformed string/quote/delimiter behavior and validate public SyntaxNode trust boundaries.
- [ ] Make real require loading share cycle/cache state, use IoHost and enforce owned exports including empty exports. Propagate errors/source identity and unwind in-flight loads.
- [ ] Provide `LoadProfile` source-evaluator selection so compiled module loading calls the Zio compiler/runtime while AST bootstrap remains explicit; coordinate callback signature `fn(&EvalContext, &str, &str) -> Result<Value, EvalError>` with compiler owner.
- [ ] Use the same fail-closed stdlib/source-aware bootstrap for native and WASM; browser host capabilities are explicit and parse remains separate.

Verification after integration: `cargo test -p zio-core --test runtime_contract`; actual CLI strict-JSON/malformed-source/import-cycle/visibility/root-escape smokes; actual rebuilt WASM stdlib and error recovery.

## Task 2: Compiler and executable self-hosting

- [ ] Implement portable validated instruction module and closures/captures/tail calls/handlers/collections; no AST-evaluation opcode.
- [ ] Add compiled-function dispatch to higher-order native functions and ZOS without changing current value/effect/error semantics.
- [ ] Implement complete recursive expansion, binding analysis and compiler in `langs/compiler/` Zio. Preserve quote/source distinctions and actual supported language/ZOS semantics.
- [ ] Implement fixed public compile/run/execute/register APIs; Rust compile entry only calls the Zio compiler.
- [ ] Compile required stdlib and compiler, execute compiled compiler for stages 1/2, and compare normalized complete artifacts and hashes. Ordinary compiler functions cannot be interpreted secretly.
- [ ] Keep deterministic language/ZOS/error/effect differential contracts and source-located malformed-bytecode refusal.

Verification after integration: native compiler smoke compiling and executing closures, rest args, tail recursion, let/loop, macros, module exports, errors and ZOS; three-stage self-build with equal normalized artifacts; inspect actual instruction execution evidence. No compile-output text assertions.

## Task 3: Native generic storage, transport and tensor mechanisms

- [ ] Expose exactly the generic primitives from design §3 with opaque resources, scoped authority and explicit errors. Reuse rusqlite/serde_json/sha2/libc/ureq/tokio where required.
- [ ] Prove parameterized transactions rollback actual writes and callbacks do not deadlock. Verify binary atomic durability and historical digest inputs.
- [ ] Implement bounded HTTP and process lifecycle/polling, exact frame identity and real isolated filesystem root; cancellation kills/reaps, no unsafe namespace fallback.
- [ ] Implement generic tensor operation backend, autodiff/Adam/seeded sampling/safe state export/import. No Grove recipe/task policy remains there.
- [ ] Install resources only with explicit CLI/application capability grants; compiler/LSP/browser sessions do not inherit application secrets/database/publisher.

Verification after integration: actual SQLite failure/rollback and competing CAS; actual framed process and malformed/oversized/error/cancel behavior; actual isolated canary/symlink/network refusal; controlled real HTTP exchange; actual CPU tensor gradients and optimizer/RNG roundtrip.

## Task 4: Complete reusable Zio libraries

- [ ] Implement Rill parser/subcommand/help/dispatch and consume it from Grove; language CLI handles only generic launch/toolchain flags.
- [ ] Correct Numa scale/shape/finite contracts and implement tensor-facing numerical algorithms used by learning.
- [ ] Replace exposed standard-library query placeholder with an executable supported query contract; no fake results or demo-as-implementation.
- [ ] Move Loom session/tool sequencing, shared admission/settlement/unknown-cost, cancellation, teacher validation and ACP client/server protocol logic to Zio. Native side is generic transport/secret custody.
- [ ] Implement real learning objectives and data/teacher/action/reward/mask/metric policy in Zio, reusing existing feedback/model/population/memory/symbolic functions.
- [ ] Remove load-time library print side effects and obsolete mirrored native policy after all consumers migrate.

Verification after integration: real library CLI programs; controlled teacher/model and ACP wire exchange with tool correlation/cancel; shared budget refusal and unknown-cost accounting; each current objective family computes real gradients and heldout behavior, not just returns declarations.

## Task 5: Grove records, persistence and human authority in Zio

- [ ] Port exact historical codecs/schema/records and load existing populated stores without rewriting immutable object bytes/history.
- [ ] Port signals/conflict resolution/frozen datasets/licences/recipes/artifacts/events/provenance and durable receipts in Zio transactions.
- [ ] Port frozen evaluation/quality gates, protected source scope, immutable candidates, decline/authentic approvals and versioned publication.
- [ ] Atomically bind authentic publisher, exact evidence, version, decision, pointer and receipt; no boolean approval/native publication shortcut.
- [ ] Port retraction and declared-horizon reachability retention with all audit/provenance/expert/capability roots; damage refuses deletion.
- [ ] Record actually consumed training provenance separately from mere dataset membership.

Verification after integration: old populated DB and exact digests; malformed schema/foreign evidence/stale version/missing repeats/retracted source refusals; actual user approval publishes once and replay stays same version; transaction crash/replay and retention no-default-deletion behavior.

## Task 6: Grove full orchestration, surfaces and browser controller

- [ ] Port queue/state/owner epoch/leases/billing/recovery to one Zio owner loop and explicit generic process concurrency.
- [ ] Port real checkpoint/pause/resume/fork, optimizer/RNG continuity, semantic-space composition, frozen/shared parameters, ensembles and memory/abstraction.
- [ ] Run inference/training with generic tensor backend; persist output snapshot and source graph, real progress and checkpoints, and observed spend.
- [ ] Port all existing CLI commands and exact HTTP routes/errors/receipts to Zio. No route handler may invoke a legacy Grove Rust operation.
- [ ] Port task synthesis, event state, forms, comparisons/review/approval to a Zio/WASM browser controller; JS only delivers generic events/fetch/DOM/drawing.
- [ ] Preserve accessible labels/error states and explicit human approval. Render real traces, loss and immutable versions; static docs are not product verification.

Verification after integration: actual service CLI/HTTP/Web; two concurrent workers and shared budgets; cancel/reap, owner restart, stale completion refusal; real CPU baseline/candidate plus save/resume/fork and frozen modules; actual browser feedback and publisher flow; receipts/status/cursors match consumers.

## Task 7: Editor infrastructure and installed distribution

- [ ] Generate real Tree-sitter parser with actual syntax corpus and highlight/locals queries; incomplete input remains inspectable.
- [ ] Implement real stdio LSP lifecycle/versioned documents/UTF-16 diagnostics/symbols/definition/references/hover/completion/semantic highlighting without user-source execution.
- [ ] Add editor wiring that launches the server and uses Zio language/highlighting; exercise actual protocol, not only config loading.
- [ ] Implement local manifest/lock and distribution assembly including compiled compiler, libraries, app, backend/resources/browser assets.
- [ ] Run installed language tools and Grove from a fresh directory, with checkout inaccessible; module/worker/web paths must not depend on compile CWD or Cargo manifest ancestry.

Verification after integration: Tree-sitter generate/parse corpus; real initialize/didOpen/didChange/definition/references/shutdown/exit exchange with emoji/CJK/CRLF and side-effect source remaining unexecuted; installed CLI/app lifecycle outside checkout.

## Task 8: Clean cutover and final acceptance

- [ ] Migrate affected callers/contracts to real new paths; remove wiring/source-text/incidental wording tests rather than repin them.
- [ ] Delete old `apps/grove/native/{app,learning}` business crates and Python task/recipe/training policy only after replacements execute; retain only genuinely generic extracted mechanisms.
- [ ] Update workspace/dependencies, current architecture/features/book/site/README/install instructions; replace historical current-status claims with exact exercised evidence.
- [ ] Run the integrated workspace behavior suite once after parallel edits settle, then reproduce and fix actual failures at their shared cause.
- [ ] Rebuild shipped WASM and Astro, exercise actual desktop/mobile language and product surfaces, inspect screenshots and source-aware error recovery.
- [ ] Run graph detect-changes on final affected scope before local commits; report unresolved dynamic edges and observed risk honestly. No push/deploy.

The completion report must include compiler stage hashes, editor protocol evidence, independent installation, real CPU/library/app/browser behavior and remaining verification limits. None of the eight user-tracked scope tasks are complete merely because a source file or plan exists.
