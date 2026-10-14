# Language-first site implementation plan

> **For agentic workers:** Execute the independent content slices in parallel; integration and verification have one owner.

**Goal:** Present Zio as a general-purpose Lisp, with usable library documentation and transparent build provenance.

**Architecture:** Reuse Astro MDX pages and the existing source-comment API collection. Keep contributor material accessible outside the language learning path. Compute version metadata once during the build, not in the browser.

**Tech Stack:** Astro, MDX, Node standard library, existing Zio WASM runtime.

## Approved design

Main navigation: language introduction, documentation, libraries, Playground. Home introduces expressions, functions, data, macros and use cases without implementation phases. Each of std, Numa, Rill, Loom and Learning gets its own introduction and links to generated module references. Contribution has its own page linking implementation manuals, ADRs and development plans. Header shows the workspace version and release/development status; footer shows branch and commit for the current documentation build. Release status requires an exact matching version tag. Missing Git metadata is explicit; CI can supply it. Historical version switching is out of scope.

## Execution

- [x] Replaced `apps/site/src/pages/index.astro`, retired the unused `src/data/content.ts` catalogue and static CodeTabs surface, and kept the interactive REPL.
- [x] Rewrote language introduction and curated the learning route, retaining syntax details and contributor archives. Added the independent contribution page.
- [x] Added a library catalogue and five independent introductions, linked all 22 current modules, and grouped the generated reference index.
- [x] Added build provenance computed in Astro configuration before bundling, using the workspace version and verified release/development classification. Updated navigation, source links and Docker metadata inputs.
- [x] Updated narrow-screen navigation, keyboard focus and build provenance. Dependencies unchanged.

## Verification

Before symbol changes, refresh GitNexus and run upstream impact; unresolved empty graph results require reference search. Run `bun run build` once after integration. Serve built output on a fresh port. In a real browser inspect home, introduction, documentation, all library introductions, a generated API page and contribution; inspect desktop and narrow-screen layout. Exercise persistent Playground definitions and evaluation. Check all internal links against emitted HTML paths and verify metadata against actual Git/Cargo inputs. Cover release-tag mismatch, no-Git metadata and CI overrides with deterministic Node checks. Update `docs/release.md` with build metadata variables after runtime proof. Do not commit or deploy unless requested.

## Exercised verification

- Static build emitted 85 pages; Node provenance checks passed (matching clean tag, mismatch, modified tree, no Git and CI-supplied metadata).
- Actual Chromium checked the learning pages, five library pages, reference, contribution and archives. All 71 distinct internal links collected from these pages returned HTTP 200; no page errors were observed.
- Desktop screenshot and 390px mobile checks covered home, docs, library catalogue, Numa, a generated reference, contribution and Playground. A 320px Numa check also had no page-level horizontal overflow.
- Real WASM Playground preserved `(def t 40)` across submissions and returned `42` for `(+ t 2)`.
- Executed all nine introductory/library code blocks with the native CLI. Intermediate output smoke corrected the standard-library sequence result and numeric display examples.
- Build emitted Astro MDX directive and chunk-size warnings; they were not suppressed. No commit, deployment, full Rust suite or Docker image build was performed.
