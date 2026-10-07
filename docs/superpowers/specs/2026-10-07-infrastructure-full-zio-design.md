# Infrastructure, full Zio implementation and toolchain self-hosting

## Approved scope

The user requested infrastructure completion and full Zio implementation, and explicitly selected both toolchain self-hosting and Zio implementation of Grove browser business. The previously approved flat layout and ZOS-in-core decision remain binding.

- `langs/core`: minimal native bootstrap/runtime, reader, ZOS and execution mechanisms; no Grove policy.
- `langs/compiler`: Zio expansion, analysis and compiler. A real portable instruction backend executes compiler-produced code. Compiler compiling itself is required.
- `libs`: Zio standard, computation (Numa), CLI composition (Rill), session/teacher/harness (Loom) and learning policy.
- `contribs`: editor/LSP/Tree-sitter integration and narrow native platform or tensor mechanisms. Infrastructure implementation languages remain unrestricted.
- `apps/grove`: Zio persistence schema and business, coordination, evaluation, approvals, training orchestration, HTTP/CLI, reports and browser controller.
- `apps/site`: existing Astro language introduction, accurate syntax documentation and actual WASM playground. It is not Grove.

A native function that runs an entire legacy Grove operation is not a migration. Moving unchanged Python recipes into `contribs` is not a migration. Source loading, macro expansion demos and a compiler emitting source for the old AST evaluator do not establish self-hosting.

## Current evidence and prerequisites

The parent executed the ordinary CLI basics program successfully. Actual REPL probes showed `(nil? (json-parse "null"))` returning false, `json-parse` accepting trailing `42 43` and Lisp `(+ 1 2)`, and identical Numa vectors returning 1000 despite a declared 10000 basis-point scale. These require correction before protocol or numerical migration.

Source inventories identify missing structured reader diagnostics, pure syntax inspection bindings, module cache/cycle/IoHost integration, strict JSON, native/WASM bootstrap parity, application argv, package distribution, binary/atomic filesystem operations, cryptographic identity, SQLite transactions, bounded process lifecycle, general HTTP and tensor operations. LSP and Tree-sitter are not currently implemented. Existing native and Python contracts are the migration behavior oracle, not a license to copy their latent failure paths.

## 1. Language and executable format

### Front end

The native reader remains the token/parse primitive. Expose a pure source-inspection operation returning tagged syntax and source ranges without evaluating source. Reader errors carry exact byte ranges; editor positions convert UTF-16 correctly. Malformed strings, quote and delimiters are errors, not silently accepted symbols or nil.

Zio owns recursive expansion, binding analysis and code generation. Expansion traverses nested forms rather than only the outermost macro. Existing core macro mechanisms may serve as one-step primitives; ordinary compiler algorithms and generated program execution must not be native AST-evaluator wrappers. Quotes remain data. Compiler analysis records lexical locals/captures, global/module resolution, tail positions, loops, handlers and source locations. Dynamic or unsupported constructs must report an explicit source-located error, never silently switch execution engines.

### Instruction runtime

Use a versioned portable stack instruction module with typed constants, functions, explicit locals/captures, jumps, calls/tail calls, collection construction, handlers and return. Validate format/version, operands, indexes, control-flow targets and stack bounds before execution. No `EvalAst` instruction or generic special-form AST fallback. Native ZOS operations remain runtime primitives; method bodies and initializers that are functions execute compiled closures.

Cover existing consumer-used language semantics: literals and quote; closures/rest arguments; `def`, `defn`, `set!`; `if`, `do`, `and`, `or`, `cond`; simultaneous/sequential let; loop/recur and ordinary tail calls; macros; modules/exports/requires; try/conditions; existing classes, generic dispatch, methods and next-method subset. Do not claim completion of previously unimplemented MOP or JIT.

Runtime interfaces under `zio_core::bytecode` must provide `register(&EvalContext)`, `compile_source(&EvalContext, name, source) -> Result<Value, EvalError>`, `execute_module(&EvalContext, &Value) -> Result<Value, EvalError>`, and `run_source(&EvalContext, name, source) -> Result<Value, EvalError>`. `compile_source` invokes the Zio compiler; it does not implement lowering in Rust. Bootstrap stage 0 may interpret the compiler source explicitly. Stage 1 and stage 2 execute compiled compiler and standard-library functions.

### Self-hosting acceptance

1. Rust bootstrap runs the Zio compiler and compiles its complete source and required Zio stdlib.
2. The compiled compiler, executed by the instruction runtime, compiles the same compiler source again.
3. That next compiled compiler repeats the build.
4. Normalize only identified nonsemantic source/debug metadata; compare complete normalized modules and hash them.
5. Run representative language, macros, module visibility, closures, tail loops, errors and ZOS programs through AST and instruction engines; compare values, effects and error classes.
6. Compiler execution cannot silently delegate ordinary compiler functions or emitted program forms to the AST interpreter.

## 2. Modules, protocols and deployment

`require` uses granted roots and the caller's IoHost; nested loads share a cache and in-flight dependency stack. Cycle detection is active on real loads. Exports refer to owned bindings and remain enforced even when the export set is empty. Module failures must unwind loading state and retain their source identity.

Strict JSON is a real JSON codec: JSON null is Zio nil; trailing input, non-JSON syntax, nonfinite numbers and unsupported values are refused. Escaping is JSON-valid. Object key normalization cannot silently collapse distinct keys. Provide deterministic serialization for newly written identities; retain exact existing artifact bytes and stored JSON bodies when validating historical digests.

The language CLI forwards script arguments and exposes explicit native capability assembly without linking Grove. A local package manifest declares entry, source roots, resources and required native capabilities. Packaged application dependencies and resources must resolve relative to the installed distribution, not CWD or compile-machine checkout. Use local/offline resolution and locked versions/content identities; a network registry is not needed for this deliverable.

Native and browser sessions share fail-closed stdlib and source-aware bootstrap. Browser filesystem/process/database capabilities are explicitly unavailable unless provided by a browser host; no attempted native filesystem fallback. Export parse/inspection separately from evaluation.

## 3. Generic native platform contract

Create `contribs/native/host` as crate `zio-host`. Parent owns its Cargo manifest and root installer, and workspace/CLI integration. Modules install NativeFn primitives into an explicitly granted context. Resource handles are opaque native callable values with private captured state; they cannot be serialized, forged by source or recovered through integer/string IDs. They do not require a second object system or global application singleton.

Use `host/` names. Host-returned envelope/row maps use keyword keys; JSON input object keys remain strings and Zio codecs deliberately normalize them. Host errors carry stable actionable classes without swallowing failures. Installation does not create resources or grant application policy.

`HostPolicy` is parent-owned native configuration: canonical read/write roots, allowed environment names, argv and explicit network/process/tensor/stdio permissions. Installers capture a private clone; source cannot mutate it. Native filesystem operations enforce the configured canonical roots, including creation under an authorized parent and refusal of symlink escapes. Application manifests request capabilities; they do not grant themselves authority.

### Storage/identity module

`storage::install(&EvalContext, &HostPolicy)` installs:

- `host/db-open(path)` -> opaque database; `host/db-query(db, sql, params)` -> vector of keyword-column rows; `host/db-execute(db, sql, params)` -> affected row count; `host/db-transaction(db, fn)` -> callback result, commit on success and rollback on error; `host/db-close(db)`.
- Parameter binding is mandatory. Transactions use the same connection through callback operations, without holding a nonreentrant mutex across language callbacks; reject unsafe competing transaction ownership.
- `host/read-bytes(path)`, `host/write-atomic(path, bytes)`, `host/mkdir(path)`, `host/list-dir(path)`, `host/remove(path)`, `host/rename(from,to)`, `host/canonical-path(path)`; binary buffer inputs and outputs, actual flush/fsync/rename durability and canonical-root enforcement where granted.
- `host/sha256(bytes)` -> lowercase hex; `host/random-bytes(count)` -> buffer; `host/clock-ms()`, `host/sleep-ms(ms)`; `host/argv()` and explicitly allowed `host/getenv(name)`.
- Generic ordered/deterministic JSON and binary numeric decoding needed by exact historical record/state formats, not Grove record constructors.

### Transport/process module

`transport::install(&EvalContext, &HostPolicy)` installs:

- `host/http-request(request)` -> bounded response; `host/http-listen(config)` -> server handle; `host/http-next(server, timeout-ms)` -> request or nil; `host/http-reply(server, request-id, response)`; `host/http-close(server)`.
- Request map: `:id`, `:method`, `:path`, `:query`, `:headers`, `:body`; response map: `:status`, `:headers`, `:body`. Headers have string keys. No role mapping or Grove route in Rust.
- `host/process-start(config)` -> process handle; bounded `host/process-read(process, timeout-ms)`, `host/process-write(process, frame)`, `host/process-wait(process, timeout-ms)`, `host/process-kill(process)`. Framed NDJSON, bounded output/stderr, exact process identity, kill and reap.
- Config has executable/argv/cwd/environment, timeout/frame/resource limits and explicit read-only mounts plus writable scratch. Native user/network/PID/mount namespaces and pivot-root implement isolation; no unsafe fallback or enclosing-checkout mount.
- Generic polling/timers permit a single Zio owner to serve HTTP while multiple native processes execute. Do not pretend synchronous `future-call` is a concurrent scheduler.
- Generic Content-Length JSON-RPC frame read/write for editor stdio, with byte caps and EOF/error distinction.

### Tensor module

Numa/learning policies drive a generic CPU tensor backend in `contribs`. Backend operations include construction, shape/dtype validation, arithmetic/matmul/reduction/activation, differentiation, optimizer parameter updates, seeded sampling, finite-number checking and non-executing optimizer/RNG/parameter state import/export. Python may host PyTorch kernels, but may not choose Grove datasets, objectives, teachers, rewards, actions, metrics, recipes or approval outcomes.

`tensor::install(&EvalContext, &HostPolicy)` exposes `host/tensor-start(config)`, `host/tensor-call(backend, operation)` and `host/tensor-close(backend)`, over the bounded process mechanism. Operation payloads describe generic tensor operations and handles, not `grove train` or a Python recipe-family dispatch.

## 4. Zio libraries

- Numa owns numerical/vector/matrix/tensor-facing algorithms and semantic similarity. Preserve encoder/space-version boundaries, correct basis-point scale, dimensional and finite-number validation.
- Rill owns argv/subcommands/help/option validation and command dispatch; process exit and terminal I/O are host operations.
- Loom owns conversation/tool-call sequencing, response correlation, history, shared budget admission/settlement/unknown-cost accounting, cancellation, teacher modality/vocabulary/licence validation and ACP client/server orchestration. Transport and secret custody alone may remain native.
- Learning owns symbolic search, feedback precedence/conflicts, graph/model rewrite, populations, memory/abstraction and all current real training objective families. Supervised/hard distillation, soft/KL, preference, demonstration, masked representation, delayed environment feedback and policy-gradient behavior remain real operations, not declarations.
- Standard library capabilities are executable; remove load-time prints from reusable modules and remove exposed placeholders rather than advertise them as implementations.

## 5. Grove persistence and business

Zio owns all existing `native/learning` domain responsibilities: records and version checks; schema/SQL; observation/prediction/signals and frozen datasets; recipes/runs/branches/populations; durable artifacts and provenance; checkpoints/continuation/fork; composition/frozen parameters/shared groups/semantic spaces; ensemble routing/combination/costs; memory/licences/abstraction/reuse; retraction and reachability-based retention; append-only trace events; frozen evaluation and gates; governance/candidates/decline/authentic approvals/publication; owner epoch/leases/billing/queue/recovery.

Preserve existing store history. Artifact identity remains SHA256 of `grove-artifact-v1\0` followed by exact bytes; objects retain existing `artifacts/objects/<aa>/<hex>` layout. `ArtifactRef` historical digest arrays and wire hex forms remain readable. Read historical bodies as their stored bytes when proving digest-bound approvals. Unknown schemas and incompatible state refuse explicitly. Schema migration must be transactional and never silently rewrite immutable history.

The user did not request a new role hierarchy or identity service: preserve existing hierarchical reader/annotator/operator/publisher behavior and explicit human/local publisher action, while never accepting `approved=true` as authority. No model or worker receives database, secret, evaluator-policy or publication handles. Protected paths move with the policies into Zio and include compiler/grant/approval boundaries.

Use one Zio owner event loop and explicit native process concurrency. Atomic transactions combine run/work/receipt creation, lease/head/epoch checks, billing and result/state commits. Read-only openings do not reclaim owner leases. Cancellation actually kills/reaps work, terminal results cannot overwrite cancellation, and restart reconciles outstanding attempts without pretending unknown effects never occurred. Persist progress/checkpoints/trained parameters as they are produced; training cannot report a durable model after deleting its output.

Approval commits bind an authenticated publisher to exact subject/source/evaluation and the observed publication version. Gate/retraction/evidence/version checks, decision and pointer/receipt update must commit atomically; stale losers do not fabricate audit decisions. Retention deletes nothing without a declared horizon and must retain all referenced approval/provenance/ensemble/capability evidence; corrupt roots fail closed.

## 6. CLI, HTTP, product UI and independent application

Grove Zio entry dispatches existing demo/inspect/checkpoint/fork/resume/compare/select/approve/decline/run/serve commands with existing error classes and explicit capability failures. All current API routes remain implemented or explicitly versioned; every mutating operation has principal/route/request-bound durable receipts, not check-effect-record races. HTTP transport does not perform business operations.

Keep actual prediction, feedback, signal status/frozen membership, runs/progress/recovery, modules/lineage/experts, comparison and explicit publisher approval. Zio also renders reports and owns task synthesis, view state, action validation and product flow. A thin generic JavaScript browser bridge performs DOM, fetch, events and drawing, feeding serialized events/results to a Zio/WASM controller; it contains no Grove task generator or approval/training decisions. HTML/CSS and generic browser bridging are presentation mechanisms, not claimed as Zio source.

Product projections distinguish filed, frozen, actually consumed, trained, independently evaluated, qualified, approved and active. Do not label dataset membership as proven learning.

An installed distribution includes language/compiled compiler, Zio libraries and application, CPU backend/environment contract, task/resources/web assets and manifest. Run it from a fresh directory with no source-checkout dependency and exercise a real complete lifecycle.

## 7. Editor infrastructure

Tree-sitter grammar and corpus follow actual Zio reader syntax, including quote, dispatch, characters, comments, strings, numbers and incomplete input; no copied Clojure anonymous-function semantics. Provide real generated parser and highlight/locals queries with editor wiring.

Language service supports stdio initialize/shutdown/exit; versioned open/change/close; source-located diagnostics with UTF-16 ranges; document symbols; definition/references/hover/completion with lexical shadowing and static module exports; semantic highlighting. It reads and parses source but never evaluates edited source or its macros. No source writes/HTTP/database/model effects from opening a document. Editor integration must actually launch the language service and use the grammar, not only ship a configuration file.

## 8. Verification and cutover

Each owner first identifies affected symbols with GitNexus impact and reports HIGH/CRITICAL before edits; exported changes check LSP references when a server exists. Shared-file edits have one integration owner. No builds, lint, tests or formatters during parallel edits; parent performs integrated checks and actual runtime smokes afterwards.

Required evidence: strict codec boundary and module failure/cycle/visibility/sandbox behavior; interpreter/compiler differential semantics and three-stage self-compilation; real Tree-sitter parsing and LSP frame/document/Unicode/no-execution scenarios; native/WASM parity and actual language-site/browser surface; existing populated-store compatibility and transactional race/recovery/approval refusal; actual CPU gradients, all objective families, frozen-parameter checks and optimizer/RNG continuation; concurrent workers/shared budgets/cancel/restart; real HTTP/CLI/UI and detached-install lifecycle.

After the new path is exercised, migrate all callers/contracts/docs, delete obsolete Rust Grove business crates and Python recipe/task policy, and remove temporary scaffolds. No compatibility aliases, source-text tests or old-business escape-hatch functions. No push or deployment is authorized by this request.
