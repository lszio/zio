# Phase 0 Truth Baseline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the repository's documentation, examples, warnings, status report, and AST performance baseline truthful and reproducible before changing the execution engine.

**Architecture:** This phase does not introduce VM, capabilities, numeric APIs, macros, or the landing site. It establishes executable example contracts, fixes the existing stdlib tail-recursion defect at its source, generates a checked-in status snapshot from repository facts, and records an AST-only benchmark artifact. Documentation then uses the same stable/experimental/planned vocabulary everywhere.

**Tech Stack:** Rust 2021 workspace, Cargo tests, Bash/POSIX tools, existing Zio CLI, Markdown.

## Global Constraints

- Scope is Phase 0 only: documentation truth, examples, compiler warnings, status automation, and AST baseline.
- Preserve the public language behavior except for fixing `nth`, `last`, and the existing `def-logger` macro-expansion defect in `examples/macros.zio`.
- Do not add runtime dependencies or any process, numeric, DSL, VM, JIT, or learning capability.
- Every runnable example must be executed by an integration test through `zio-cli`.
- `RUSTFLAGS="-Dwarnings" cargo test --workspace` must pass without Rust compiler warnings.
- Public performance claims must name the command, engine, machine metadata, revision, and metric; this phase records measurements but claims no target speedup.
- Generated status must be deterministic and must not embed `HEAD`, timestamps, or other values that become stale after a documentation commit.
- Keep `docs/superpowers/` as local planning material; ignore `.superpowers/` working artifacts.

---

## File Structure

- Modify: `.gitignore` — ignore generated brainstorming session state.
- Create: `examples/manifest.tsv` — declares each example's runnable status and required output marker.
- Create: `cli/tests/example_contract.rs` — runs each manifest-listed runnable example through the built CLI.
- Modify: `core/stdlib/zio/core.zio` — replace invalid function-level `recur` in `last` and `nth`.
- Modify: `core/src/eval.rs` — regression coverage for the repaired stdlib functions.
- Modify: `examples/basics.zio`, `examples/zos-concept.zio`, `examples/datalog-concept.zio` — convert stale or unparsable demonstrations into honest runnable examples.
- Modify: Rust files reported by `RUSTFLAGS="-Dwarnings" cargo test --workspace` — remove warnings without blanket `allow` attributes.
- Create: `tools/project-status.sh` and `docs/status.md` — deterministic repository fact report and checked-in snapshot.
- Create: `tools/record-ast-baseline.sh`, `benchmarks/ast-baseline.json`, and `benchmarks/README.md` — reproducible AST baseline measurement.
- Create: `docs/feature-matrix.md` — source of truth for stable, experimental, and planned features.
- Modify: `README.md`, `docs/zio-architecture.md`, `docs/eval-pipeline.md`, `docs/roadmap.md`, `docs/adrs.md`, `docs/zos-spec.md`, `docs/zio-philosophy.md`, and `docs/glossary.md` — align claims and links with the generated truth and feature matrix. These are the current repository paths corresponding to obsolete plan paths `docs/architecture.md`, `docs/adr/001-runtime-architecture.md`, and absent `docs/book/` pages.

### Task 1: Add the executable-example contract

**Files:**
- Modify: `.gitignore`
- Create: `examples/manifest.tsv`
- Create: `cli/tests/example_contract.rs`

**Interfaces:**
- Consumes: the compiled binary exposed by Cargo as `env!("CARGO_BIN_EXE_zio-cli")`.
- Produces: `examples/manifest.tsv` rows in `file|status|expected_stdout_marker` format. `status` is either `runnable` or `documentary`; the integration test executes only `runnable` rows.

- [ ] **Step 1: Write the manifest and failing integration test**

Create `examples/manifest.tsv`:

```text
# file|status|expected_stdout_marker
basics.zio|runnable|=== Higher-order ===
macros.zio|runnable|[ERROR] disk full
zos-concept.zio|runnable|ZOS capability probe completed.
datalog-concept.zio|runnable|zio-datalog library: planned capability.
```

Create `cli/tests/example_contract.rs`:

```rust
use std::path::PathBuf;
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate lives below workspace root")
        .to_path_buf()
}

fn run_example(file: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .arg(workspace_root().join("examples").join(file))
        .output()
        .expect("run zio-cli example")
}

#[test]
fn runnable_examples_exit_successfully_and_print_their_marker() {
    for line in include_str!("../../examples/manifest.tsv").lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('|').collect();
        assert_eq!(fields.len(), 3, "invalid manifest row: {line}");
        if fields[1] != "runnable" {
            continue;
        }

        let output = run_example(fields[0]);
        assert!(
            output.status.success(),
            "{} failed: {}",
            fields[0],
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(fields[2]),
            "{} did not print marker {:?}; stdout was {stdout:?}",
            fields[0],
            fields[2]
        );
    }
}
```

Append `/.superpowers/` to `.gitignore` on its own line. Retain the existing `superpowers` pattern.

- [ ] **Step 2: Run the contract to prove the current examples are not shippable**

Run: `cargo test -p zio-cli --test example_contract`

Expected: FAIL. The output identifies the existing basic, macro, or concept examples as non-zero exits or missing required markers.

- [ ] **Step 3: Commit the test harness**

Run:

```bash
git add .gitignore examples/manifest.tsv cli/tests/example_contract.rs
git commit -m "test: add executable example contract"
```

### Task 2: Repair stdlib function tail recursion

**Files:**
- Modify: `core/src/eval.rs` in the existing evaluator test module
- Modify: `core/stdlib/zio/core.zio` in `last` and `nth`
- Modify: `examples/macros.zio` in the `def-logger` macro

**Interfaces:**
- Consumes: `eval_str(&Context, &str) -> Value` and `load_stdlib(&Context)` already defined by `core/src/eval.rs`.
- Produces: `(last coll)` returning the final list value or `nil`, `(nth coll n)` returning index `n` or `nil` without emitting `TailResult::Recur` outside `loop`, and a `def-logger` expansion that looks up variadic `msg` only at generated-function runtime.

- [ ] **Step 1: Add the regression test**

Add this test alongside the current stdlib evaluator tests:

```rust
#[test]
fn test_stdlib_nth_and_last_use_function_tail_calls() {
    let ctx = make_ctx();
    load_stdlib(&ctx);

    assert_eq!(
        eval_str(&ctx, "(nth (list 10 20 30) 1)").unwrap(),
        Value::Integer(20)
    );
    assert_eq!(
        eval_str(&ctx, "(last (list 10 20 30))").unwrap(),
        Value::Integer(30)
    );
    assert_eq!(
        eval_str(&ctx, "(nth (list 10 20 30) 99)").unwrap(),
        Value::Nil
    );
}
```

Use the existing test module imports and helpers rather than duplicating them.

- [ ] **Step 2: Run the narrow regression test**

Run: `cargo test -p zio-core eval::tests::test_stdlib_nth_and_last_use_function_tail_calls -- --exact`

Expected: FAIL with `unexpected recur`.

- [ ] **Step 3: Replace the two invalid uses of recur**

In `core/stdlib/zio/core.zio`, change only these tail positions:

```clojure
(last (cdr coll))
(nth (rest coll) (dec n))
```

Do not change `core/src/special/letloop.rs`: its `recur` handling remains restricted to `loop`.

- [ ] **Step 4: Add the macro-example regression and repair def-logger expansion**

Add `test_macro_example_generates_variadic_logger_functions` beside the list-helper regression. It must load `core/stdlib/zio/core.zio`, evaluate the actual `examples/macros.zio` file through the evaluator, and assert it returns `Value::Nil`.

Run the test before the correction:

```bash
cargo test -p zio-core eval::tests::test_macro_example_generates_variadic_logger_functions -- --exact
```

Expected: FAIL with `SymbolNotFound("msg", ...)`, proving the macro eagerly evaluates the generated function's parameter.

In `examples/macros.zio`, change only the logger body construction from eager `str` evaluation to the following data expression:

```clojure
(list 'str "[" level "] " (list 'apply 'str 'msg))
```

This preserves the generated `println` output and defers the `msg` lookup to the variadic runtime function.

- [ ] **Step 5: Run the regressions and macro example**

Run:

```bash
cargo test -p zio-core eval::tests::test_stdlib_nth_and_last_use_function_tail_calls -- --exact
cargo test -p zio-core eval::tests::test_macro_example_generates_variadic_logger_functions -- --exact
cargo run -p zio-cli -- examples/macros.zio
```

Expected: both regressions pass, and the macro example exits zero and prints `[ERROR] disk full`.

- [ ] **Step 6: Commit the fix**

Run:

```bash
git add core/src/eval.rs core/stdlib/zio/core.zio examples/macros.zio
git commit -m "fix: repair stdlib recursion and macro example"
```

### Task 3: Make every declared example honest and runnable

**Files:**
- Modify: `examples/basics.zio`
- Modify: `examples/zos-concept.zio`
- Modify: `examples/datalog-concept.zio`
- Test: `cli/tests/example_contract.rs`

**Interfaces:**
- Consumes: manifest contract from Task 1 and fixed stdlib functions from Task 2.
- Produces: four zero-exit examples whose standard output contains their manifest marker; unsupported sets and Datalog evaluation are not presented as working features.

- [ ] **Step 1: Re-run the contract after Task 2**

Run: `cargo test -p zio-cli --test example_contract`

Expected: FAIL because `basics.zio`, `zos-concept.zio`, and `datalog-concept.zio` are still not valid runnable demonstrations.

- [ ] **Step 2: Remove the unsupported set literal from basics**

In `examples/basics.zio`, replace the line that evaluates `(println #{a b c})` with this explanatory comment:

```clojure
;; Set literals are documented as planned in docs/feature-matrix.md.
```

Keep the existing higher-order output section so `=== Higher-order ===` remains printed.

- [ ] **Step 3: Replace the ZOS sketch with the tested object-system syntax**

Replace the contents of `examples/zos-concept.zio` with:

```clojure
(println "=== ZOS Capability Probe ===")

(defclass point nil
  ((x :initarg :x)
   (y :initarg :y)))

(def p (make-instance point :x 10 :y 20))
(println "point-x:" (slot-value p :x))
(println "point-y:" (slot-value p :y))

(defgeneric describe (shape))
(defmethod describe ((shape point))
  (str "Point(" (slot-value shape :x) ", " (slot-value shape :y) ")"))

(println (describe p))
(println "ZOS capability probe completed.")
```

Use the parenthesized `defgeneric` parameter list shown in `core/src/eval.rs::test_zos_generic_function`; preserve the same output marker and only demonstrate supported object-system operations.

- [ ] **Step 4: Replace the Datalog sketch with data-only syntax**

Replace the contents of `examples/datalog-concept.zio` with:

```clojure
(println "=== Datalog as Data ===")

(def person-query
  '[:find ?name
    :where
    [?entity :person/name ?name]])

(println "query-form:" person-query)
(println "zio-datalog library: planned capability.")
```

Do not use `#|` block-comment syntax or claim a query evaluator exists.

- [ ] **Step 5: Run examples individually and through the contract**

Run:

```bash
cargo run -p zio-cli -- examples/basics.zio
cargo run -p zio-cli -- examples/zos-concept.zio
cargo run -p zio-cli -- examples/datalog-concept.zio
cargo test -p zio-cli --test example_contract
```

Expected: all commands exit zero; the contract passes.

- [ ] **Step 6: Commit the runnable examples**

Run:

```bash
git add examples/basics.zio examples/zos-concept.zio examples/datalog-concept.zio
git commit -m "docs: make language examples executable"
```

### Task 4: Eliminate existing Rust compiler warnings

**Files:**
- Modify: only Rust source files named by `RUSTFLAGS="-Dwarnings" cargo test --workspace`.
- Test: the workspace test suite.

**Interfaces:**
- Consumes: the existing test suite and compiler diagnostics.
- Produces: a warning-free Rust build under `-Dwarnings`; no `#[allow(...)]` is added solely to suppress a warning.

- [ ] **Step 1: Capture the exact warning list**

Run: `RUSTFLAGS="-Dwarnings" cargo test --workspace`

Expected: FAIL during compilation, listing unused imports, unused variables or mutability, and dead code that are currently warnings in the normal build.

- [ ] **Step 2: Add coverage before retaining reader span helpers**

If diagnostics identify `make_span` or `with_span` in `core/src/reader/reader.rs` as dead code, add a reader test that calls the public source-reading path and asserts the parsed root has a source span. The test must use the existing source-reading API and assert both its start and end positions, not merely `is_some()`.

Run: `cargo test -p zio-core reader -- --nocapture`

Expected before the implementation adjustment: the new assertion exposes that the helper path is not used or the helper is dead.

- [ ] **Step 3: Apply minimal, behavior-preserving warning fixes**

For every diagnostic from Step 1:

- remove an import only when no code path needs it;
- rename an intentionally unused binding to begin with `_`;
- remove `mut` only when the binding is never assigned;
- delete a private helper only after the new reader span test proves its behavior is covered elsewhere;
- remove stale documentation comments that describe deleted helpers;
- preserve ZOS imports and parameters only where a compiled feature path uses them.

Do not add broad lint suppression attributes and do not redesign `Value`, the evaluator, object system, or thread-safety model in this task.

- [ ] **Step 4: Verify with warnings made fatal**

Run:

```bash
RUSTFLAGS="-Dwarnings" cargo test --workspace
cargo test -p zio-cli --test example_contract
```

Expected: both commands pass with no Rust compiler warning output.

- [ ] **Step 5: Commit warning cleanup**

Run:

```bash
git add core cli
git commit -m "chore: remove compiler warnings"
```

### Task 5: Generate and check a deterministic project status report

**Files:**
- Create: `tools/project-status.sh`
- Create: `docs/status.md`

**Interfaces:**
- Consumes: workspace `Cargo.toml`, `examples/manifest.tsv`, Rust source files, and Cargo test listing.
- Produces: `tools/project-status.sh [--check]`. With no option it writes Markdown to stdout. With `--check` it exits 0 exactly when its stdout equals `docs/status.md`, otherwise exits 1 and prints the regeneration command.

- [ ] **Step 1: Write the status-generator contract**

Create `tools/project-status.sh` with this observable output shape:

```markdown
# Zio Project Status

> Generated with `tools/project-status.sh`. Verify with `tools/project-status.sh --check`.

| Fact | Value |
| --- | --- |
| Workspace crates | 2 |
| Rust tests | an integer computed at generation time |
| Special forms | an integer computed at generation time |
| Native bindings | an integer computed at generation time |
| Runnable examples | an integer computed at generation time |
```

Use `cargo test --workspace -- --list` to count test lines ending in `: test`; use `rg` against the existing registration sites to count special forms and native bindings; use `examples/manifest.tsv` to count `runnable` rows. Use a shell temporary file for `--check` and a `trap` to remove it. Do not include a commit hash, timestamp, hostname, or dirty state in the generated Markdown.

Use this complete script body:

```bash
#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

emit_status() {
  local tests special_forms native_bindings runnable_examples
  tests="$(cargo test --workspace -- --list | awk '/: test$/ { count += 1 } END { print count + 0 }')"
  special_forms="$(rg -n '=> Some\(' core/src/special/mod.rs | wc -l | tr -d ' ')"
  native_bindings="$(rg -n 'env\.set\(' core/src -g '*.rs' | wc -l | tr -d ' ')"
  runnable_examples="$(awk -F '|' '$1 !~ /^#/ && $2 == "runnable" { count += 1 } END { print count + 0 }' examples/manifest.tsv)"

  printf '# Zio Project Status\n\n'
  printf '> Generated with `tools/project-status.sh`. Verify with `tools/project-status.sh --check`.\n\n'
  printf '| Fact | Value |\n| --- | --- |\n'
  printf '| Workspace crates | 2 |\n'
  printf '| Rust tests | %s |\n' "$tests"
  printf '| Special forms | %s |\n' "$special_forms"
  printf '| Native bindings | %s |\n' "$native_bindings"
  printf '| Runnable examples | %s |\n' "$runnable_examples"
}

case "${1:-}" in
  "")
    emit_status
    ;;
  --check)
    temp_file="$(mktemp)"
    trap 'rm -f "$temp_file"' EXIT
    emit_status > "$temp_file"
    if cmp -s "$temp_file" docs/status.md; then
      exit 0
    fi
    printf 'docs/status.md is stale; run tools/project-status.sh > docs/status.md\n' >&2
    exit 1
    ;;
  *)
    printf 'usage: %s [--check]\n' "$0" >&2
    exit 2
    ;;
esac
```

- [ ] **Step 2: Verify status check initially fails**

Run: `tools/project-status.sh --check`

Expected: FAIL because `docs/status.md` does not yet exist.

- [ ] **Step 3: Generate the checked-in snapshot**

Run: `tools/project-status.sh > docs/status.md`

Then run: `tools/project-status.sh --check`

Expected: PASS.

- [ ] **Step 4: Add a negative check for a stale report**

Run:

```bash
cp docs/status.md /tmp/zio-status.md
printf '\n' >> docs/status.md
! tools/project-status.sh --check
mv /tmp/zio-status.md docs/status.md
tools/project-status.sh --check
```

Expected: the negated check detects the altered snapshot, then the restored snapshot passes.

- [ ] **Step 5: Commit status automation**

Run:

```bash
git add tools/project-status.sh docs/status.md
git commit -m "docs: add generated project status"
```

### Task 6: Record a reproducible AST evaluator baseline

**Files:**
- Create: `tools/record-ast-baseline.sh`
- Create: `benchmarks/ast-baseline.json`
- Create: `benchmarks/README.md`

**Interfaces:**
- Consumes: `target/release/zio-cli`, three temporary Zio source files, `git rev-parse HEAD`, and `uname -srm`.
- Produces: `tools/record-ast-baseline.sh --output PATH`, a JSON object with keys `engine`, `command`, `revision`, `machine`, `benchmarks`, and `units`. `engine` is exactly `"ast"` and `units` is exactly `"seconds"`.

- [ ] **Step 1: Write the recorder test fixture programs**

The script must create exactly these workloads in a `mktemp -d` directory and delete the directory through `trap`:

```clojure
;; sum-loop.zio
(loop [i 0 total 0]
  (if (= i 10000) total (recur (inc i) (+ total i))))

;; function-call.zio
(defn identity-loop [i]
  (if (= i 10000) i (identity-loop (inc i))))
(identity-loop 0)

;; map-loop.zio
(loop [i 0 xs (list 1 2 3 4 5 6 7 8 9 10)]
  (if (= i 1000) xs (recur (inc i) (map inc xs))))
```

- [ ] **Step 2: Implement the recording command**

Implement `tools/record-ast-baseline.sh` so it:

1. requires `--output` followed by a path and rejects any other argument sequence;
2. runs `cargo build --release --bin zio-cli`;
3. measures each workload with the shell `time` builtin or `/usr/bin/time -f %e`, storing one elapsed-seconds number per benchmark;
4. writes valid JSON to the requested output path;
5. never replaces `benchmarks/ast-baseline.json` unless that exact path was passed by the caller.

Use direct process invocation of `target/release/zio-cli`; do not use a shell string to execute Zio programs.

Use this complete script body, retaining the three fixture contents from Step 1:

```bash
#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 || "$1" != "--output" || -z "$2" ]]; then
  printf 'usage: %s --output PATH\n' "$0" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="$2"
work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT
cd "$root"

printf '%s\n' \
  '(loop [i 0 total 0]' \
  '  (if (= i 10000) total (recur (inc i) (+ total i))))' \
  > "$work_dir/sum-loop.zio"
printf '%s\n' \
  '(defn identity-loop [i]' \
  '  (if (= i 10000) i (identity-loop (inc i))))' \
  '(identity-loop 0)' \
  > "$work_dir/function-call.zio"
printf '%s\n' \
  '(loop [i 0 xs (list 1 2 3 4 5 6 7 8 9 10)]' \
  '  (if (= i 1000) xs (recur (inc i) (map inc xs))))' \
  > "$work_dir/map-loop.zio"

cargo build --release --bin zio-cli
binary="$root/target/release/zio-cli"
measure() {
  local name="$1" program="$2" elapsed
  /usr/bin/time -f '%e' -o "$work_dir/$name.time" "$binary" "$program" >/dev/null
  elapsed="$(tr -d '\n' < "$work_dir/$name.time")"
  case "$elapsed" in
    *[!0-9.]*|'') printf 'invalid elapsed value for %s: %s\n' "$name" "$elapsed" >&2; exit 1 ;;
  esac
  printf '%s' "$elapsed"
}

sum_loop="$(measure sum-loop "$work_dir/sum-loop.zio")"
function_call="$(measure function-call "$work_dir/function-call.zio")"
map_loop="$(measure map-loop "$work_dir/map-loop.zio")"
mkdir -p "$(dirname "$output")"
printf '{\n'
printf '  "engine": "ast",\n'
printf '  "command": "target/release/zio-cli PROGRAM",\n'
printf '  "revision": "%s",\n' "$(git rev-parse HEAD)"
printf '  "machine": "%s",\n' "$(uname -srm)"
printf '  "units": "seconds",\n'
printf '  "benchmarks": {\n'
printf '    "sum-loop": %s,\n' "$sum_loop"
printf '    "function-call": %s,\n' "$function_call"
printf '    "map-loop": %s\n' "$map_loop"
printf '  }\n'
printf '}\n'
```

- [ ] **Step 3: Record a temporary baseline and validate its schema**

Run:

```bash
tools/record-ast-baseline.sh --output /tmp/zio-ast-baseline.json
rg '"engine": "ast"' /tmp/zio-ast-baseline.json
rg '"units": "seconds"' /tmp/zio-ast-baseline.json
rg '"sum-loop"|"function-call"|"map-loop"' /tmp/zio-ast-baseline.json
```

Expected: the command exits zero and all four searches match.

- [ ] **Step 4: Write the artifact and benchmark contract**

Run: `tools/record-ast-baseline.sh --output benchmarks/ast-baseline.json`

Create `benchmarks/README.md` explaining the exact recorder command, that the artifact is an AST baseline rather than a speed claim, and that comparisons require the same workload, engine field, release build, and machine metadata.

- [ ] **Step 5: Commit the baseline**

Run:

```bash
git add tools/record-ast-baseline.sh benchmarks/ast-baseline.json benchmarks/README.md
git commit -m "perf: record AST evaluator baseline"
```

### Task 7: Publish the single source of truth and synchronize documentation

**Files:**
- Create: `docs/feature-matrix.md`
- Modify: `README.md`
- Modify: `docs/zio-architecture.md`
- Modify: `docs/eval-pipeline.md`
- Modify: `docs/roadmap.md`
- Modify: `docs/adrs.md`
- Modify: `docs/zos-spec.md`
- Modify: `docs/zio-philosophy.md`
- Modify: `docs/glossary.md`

**Interfaces:**
- Consumes: generated facts from `docs/status.md`, example contract from Task 1, AST artifact from Task 6, and the approved design specification at `docs/superpowers/specs/2026-08-10-zio-vm-applications-site-design.md`.
- Produces: `docs/feature-matrix.md` as the authoritative stable/experimental/planned table, with all referenced documentation linking to it and making no conflicting count or completion claims.

- [ ] **Step 1: Create the feature matrix**

Create `docs/feature-matrix.md` with these rows and statuses:

| Area | Status | Evidence |
| --- | --- | --- |
| Reader and syntax expansion | Stable | reader unit tests |
| AST evaluator and closures | Stable | core evaluator tests |
| Core macros and stdlib | Stable | `examples/macros.zio` contract |
| ZOS classes and generic dispatch subset | Experimental | `examples/zos-concept.zio` contract |
| Persistent collection library | Planned | `lib/zio/persistent.zio` |
| Datalog evaluator | Planned | `examples/datalog-concept.zio` |
| ZIR, bytecode VM, and JIT | Planned | approved VM design |
| Process capability and pacman updater | Planned | approved applications design |
| Numeric kernel and scientific API | Planned | approved applications design |
| Pipeline DSL | Planned | approved applications design |
| Homoiconic learner | Planned | approved applications design |
| Landing site | Planned | approved applications design |

Add links to `docs/status.md` and `benchmarks/README.md` above the table.

- [ ] **Step 2: Replace stale quantitative claims**

Update `README.md` to state the workspace has two crates and point readers to `docs/status.md` for generated counts. Remove claims of exactly 103 tests, zero warnings, three crates, twelve special forms, or thirty builtins.

In the current architecture, evaluator, roadmap, ADR, ZOS, philosophy, and glossary pages, replace claims that imply an implemented VM, persistent collections, Datalog, process capability, numeric computing, DSL, learner, or site with links to the feature matrix and their planned status. Retain historical rationale only when it is clearly marked historical.

- [ ] **Step 3: Mark example limitations at their public entry points**

Ensure the README example section states that all files in `examples/manifest.tsv` are runnable, while the Datalog example demonstrates query data rather than evaluation. Link the unsupported set literal note to `docs/feature-matrix.md`.

- [ ] **Step 4: Run documentation consistency checks**

Run:

```bash
tools/project-status.sh --check
rg -n '103 tests|0 warnings|three crates|12 special forms|30 builtins' README.md docs
rg -n '#\|' examples
cargo test -p zio-cli --test example_contract
```

Expected: the status and example contract pass; the two `rg` searches return exit code 1 because no stale numeric claims or unsupported block-comment syntax remain.

- [ ] **Step 5: Commit documentation synchronization**

Run:

```bash
git add README.md docs examples/manifest.tsv
git commit -m "docs: synchronize feature and architecture status"
```

### Task 8: Execute the Phase 0 release gate

**Files:**
- Verify: all Phase 0 files and the existing workspace.

**Interfaces:**
- Consumes: the completed contracts, generated snapshot, benchmark artifact, and documentation matrix.
- Produces: reproducible evidence that Phase 0 is complete; no source changes are introduced by this task.

- [ ] **Step 1: Run the complete warning-free test gate**

Run:

```bash
RUSTFLAGS="-Dwarnings" cargo test --workspace
cargo test -p zio-cli --test example_contract
tools/project-status.sh --check
```

Expected: every command exits zero.

- [ ] **Step 2: Validate the performance artifact without rewriting it**

Run:

```bash
rg '"engine": "ast"' benchmarks/ast-baseline.json
rg '"units": "seconds"' benchmarks/ast-baseline.json
rg '"sum-loop"|"function-call"|"map-loop"' benchmarks/ast-baseline.json
```

Expected: each command exits zero. Do not record a new baseline in this gate; the artifact records the measurement revision from Task 6.

- [ ] **Step 3: Run non-fatal lint and whitespace checks**

Run:

```bash
cargo clippy --workspace --all-targets
git diff --check
git status --short
```

Expected: Clippy exits zero; any advisory outside the fatal compiler-warning gate is recorded in the review notes, not suppressed in this phase. `git diff --check` exits zero. The status output contains only intended Phase 0 changes and local ignored `.superpowers/` artifacts.

- [ ] **Step 4: Inspect final public claims**

Run:

```bash
rg -n 'ZIR|bytecode|JIT|Datalog|persistent|pacman|scientific|pipeline|homoiconic|landing' README.md docs/feature-matrix.md docs/status.md
git log --oneline -8
```

Expected: every implementation-status claim agrees with the matrix, and the log contains the seven Phase 0 commits.

- [ ] **Step 5: Create the final Phase 0 commit only if verification required a source correction**

If a correction was necessary in Steps 1–4, commit it with the most specific conventional message matching the corrected area. If no correction was necessary, do not create an empty commit.
