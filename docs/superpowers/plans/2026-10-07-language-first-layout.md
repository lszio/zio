# Language-first layout implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use executing-plans to implement this plan task-by-task. Parent owns physical moves and integration; skip build/lint/tests/formatters until all edits are integrated.

> Historical plan (2026-10-07). The application half of this cutover (its source
> tree, CLI/HTTP paths and torch worker) moved to the independent project
> [lszio/groiz](https://github.com/lszio/groiz) and is no longer part of this
> repository; those historical paths are described below without being current layout.

**Goal:** Begin the approved language-first refactor with a working directory cutover, a domain-independent language CLI, and a language site whose playground is separate from the application.

**Architecture:** `langs/` is flat. Zio libraries live in `libs/`; applications live in `apps/`; implementation-language-independent integrations live in `contribs/`. Existing native application business stays explicitly under the application tree until replaced, not disguised as infrastructure. This cutover does not claim a completed application product or a self-hosted compiler.

**Tech Stack:** Existing Rust workspace, Zio libraries, Astro, WASM, Python torch worker. No new dependency.

---

## Task 1: Physical cutover and paths

- [x] Move `core/` → `langs/core/`, `cli/` → `langs/cli/`, `site/` → `apps/site/`.
- [x] Move `app/` → `native/app/`, `learning/` → `native/learning/`, `workers/torch/` → `workers/torch/` under the application tree (that tree later moved to the Groiz repository).
- [x] Move the generic Rust model transport `loom/` → `contribs/native/loom/`; this is an adapter, not the Zio Loom library.
- [x] Move `core/stdlib/zio/core.zio` → `libs/std/core.zio`; update compile-time embedding to `../../../libs/std/core.zio` from `langs/core/src/`.
- [x] Move persistent/entity/protocol/pipeline/datalog Zio files to `libs/std/`; vector to `libs/numa/`; agent/proposer to `libs/loom/`; learn/memory and learn submodules to `libs/learning/`.
- [x] Update workspace members and all Cargo path dependencies, `include_str!`, repository-root calculations, Zio `load`, isolation mounts, governance paths, worker paths, build/deploy scripts and active documentation. Remove empty old source trees; no aliases or symlink compatibility paths.

Workspace member contract:

```toml
members = ["langs/core", "langs/cli", "contribs/native/loom"]  # + the two application crates, later moved to the Groiz repository
```

## Task 2: Language CLI boundary

- [x] Remove the `loom` dependency and `--llm-replay` option from `langs/cli/`.
- [x] Script execution uses `script_ctx`, reads the source, and calls `eval_source`; neither CLI scripts nor REPL install model bindings.
- [x] Keep `--lib-dir`, `ZIO_PATH`, source diagnostics, module exports and fail-closed bootstrap unchanged. Move existing hosted LLM use to existing model-adapter contracts rather than extending language options.
- [x] Update integration contracts and library/example paths. Delete source-text/wiring assertions encountered in affected tests rather than re-pin them.

## Task 3: Language site and separate application

- [x] Reuse the Astro application in `apps/site/`, including its WASM playground.
- [x] Main navigation and homepage prioritize language introduction, syntax documentation, and playground. Provide a dedicated language playground route; the application kept its own page, not an execution mode or preset.
- [x] Correct source-root resolution for docs/book/blog after moving the site; keep WASM imports functional.
- [x] Update architecture/status text to distinguish supported language behavior, proposed Zio libraries, and existing native application business.

## Task 4: Runtime evidence and documentation

- [x] Run `cargo test --workspace --all-features` after integration; correct migration defects and rerun serially.
- [x] Run `cargo run -q -p zio-cli -- examples/basics.zio`, `examples/zos-concept.zio`, and `examples/learn-demo.zio`; verify actual output.
- [x] Build WASM with `bash tools/build-wasm.sh`, then `bun run build` in `apps/site/`.
- [x] Serve the built site, open the real browser, run a playground expression, inspect syntax documentation and the application separation, and capture desktop/mobile screenshots.
- [x] Exercise the existing application CLI/HTTP paths and governance refusal after the path migration; do not claim CPU training without the actual torch runtime and successful run.
- [x] Update `README.md`, `docs/zio-architecture.md`, feature/roadmap documents and deployment instructions to report this cutover and remaining Rust business honestly. Historical specs/plans retain historical claims but receive a supersession note where necessary.

## Acceptance

No old active physical roots or compatibility aliases. Language CLI no longer links Loom. Every library/application loader reaches the new paths. Playground runs the language runtime and is not the application. Existing permission/isolation/publication protections remain exercised. Compiler, Rill, Tree-sitter and LSP are not manufactured as empty directories.


## Verified integration results (2026-10-07)

- `cargo test --workspace --all-features`: **489 passed**, including real worker/isolation and publication contracts. Cargo builds with different feature sets were serialized for the final run; the complete run and a standalone doctest run passed.
- The application's torch worker test suite (relocated to the Groiz repository): **77 passed**. The mount-isolated worker has no unused repository-relative `--task` default; protocol frames supply its data.
- Actual CLI runs of `examples/basics.zio`, `examples/zos-concept.zio`, and `examples/learn-demo.zio` passed. Learning output included `f(10) = 21`, deterministic replay, and square `f(6) = 36`.
- `cargo tree -p zio-cli --depth 1` showed `im` and `zio-core`, no Loom dependency. `tools/project-status.sh --check` passed; its default-feature inventory remains 474 tests, distinct from the all-feature executed count.
- `bash tools/build-wasm.sh`, Astro build and an actual Docker image build passed. The site emitted **46 pages**; all internal page links resolved to generated routes, including Unicode book slugs.
- Real Chromium against the built nginx container exercised all six presets, editable multiline execution, persistent definitions, Ctrl+Enter, reader errors and subsequent recovery. Introduction and syntax navigation worked; the application had no language REPL. Desktop/mobile screenshots were captured; 390, 768 and 1024 pixel widths had no observed horizontal overflow.
- The existing approval smoke ran real dual CPU training and HTTP requests, with only its scratch root changed to a newly owned temporary directory. Training left a candidate unpublished; protected-module and Operator approval attempts were refused; Publisher approval moved the pointer once; replay and stale-version checks held. The previous application smoke scratch root was not modified.
- Observed warnings: Astro/Vite reported MDX `use astro:head-inject` bundling warnings; torch reported unavailable NumPy; Python tests emitted resource warnings. These were not suppressed. The exercised site, training and contracts passed; this does not claim warning-free builds or verification on other platforms.

This stage delivers a working structural cutover, not a complete application rewrite or self-hosted compiler. No deployment, commit or push was performed.

### Graph review limit

The index was rebuilt for the new paths. `detect-changes --scope all --repo zio` reported 236 files, 4032 changed symbols and 436 affected indexed processes, with **CRITICAL** risk for the broad source-tree cutover. The symbol listing remained capped when retried with limits of 10000 and 50000; process discovery also reported omitted traces. This is not an exhaustive graph all-clear. The complete workspace contracts and real language, worker, site and application paths above are the exercised evidence; no commit was made.
