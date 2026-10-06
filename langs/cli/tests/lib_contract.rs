use std::path::PathBuf;
use std::process::Command;

/// Contract tests for the extension libraries in libs. Each contract
/// file loads its library, exercises every public function, and prints
/// PASS/FAIL markers. This is the guardrail that would have caught the
/// libraries shipping with broken functions (undefined symbols, parallel
/// `let` misuse) — see the 2026-09 architecture review.

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("cli crate lives at langs/cli below workspace root")
        .to_path_buf()
}

fn run_contract(file: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .arg(workspace_root().join("langs/cli/tests/libs").join(file))
        .current_dir(&workspace_root())
        .output()
        .unwrap_or_else(|e| panic!("run lib contract {file}: {e}"));
    assert!(
        output.status.success(),
        "contract {file} exited with failure:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        !stdout.contains("FAIL"),
        "contract {file} reported failures:\n{stdout}"
    );
    stdout
}

fn assert_markers(stdout: &str, file: &str, markers: &[&str]) {
    for marker in markers {
        // zio prints strings with quotes: "PASS" "marker"
        let line = format!("\"PASS\" \"{marker}\"");
        assert!(
            stdout.contains(&line),
            "contract {file} missing PASS marker {marker:?}:\n{stdout}"
        );
    }
}

#[test]
fn persistent_library_contract() {
    let out = run_contract("persistent.zio");
    assert_markers(
        &out,
        "persistent.zio",
        &[
            "conj-vector",
            "conj-list",
            "conj-map",
            "pop-vector",
            "pop-list",
            "peek-vector",
            "peek-list",
            "merge",
            "merge-override",
            "set-count",
            "contains-yes",
            "contains-no",
            "disj-removes",
            "disj-keeps",
        ],
    );
}

#[test]
fn datalog_library_contract() {
    let out = run_contract("datalog.zio");
    assert_markers(
        &out,
        "datalog.zio",
        &["transact-count", "transact-datom", "q-stub-values"],
    );
}

#[test]
fn agent_library_contract() {
    let out = run_contract("agent.zio");
    assert_markers(
        &out,
        "agent.zio",
        &[
            // A failing first attempt is retried, and the second is the
            // candidate. `retry-calls` is the evidence that the logic
            // asked again rather than the harness doing it.
            "retry-status",
            "retry-turns",
            "retry-calls",
            "retry-ran-twice",
            // The ceiling the grant set ends the loop and the failure
            // is reported, not swallowed into an exhausted run.
            "ceiling-status",
            "ceiling-turns",
            "ceiling-calls",
            "ceiling-reports-failure",
            // A reply with no source is not a program, and a failure
            // keeps its error for the next turn.
            "source-of-empty",
            "source-of-present",
            "feedback-keeps-error",
            "feedback-defaults",
        ],
    );
}

#[test]
fn protocol_library_contract() {
    let out = run_contract("protocol.zio");
    assert_markers(
        &out,
        "protocol.zio",
        &["dispatch-dog", "dispatch-cat", "multi-arg", "generic-function-name"],
    );
}

#[test]
fn entity_library_contract() {
    let out = run_contract("entity.zio");
    assert_markers(
        &out,
        "entity.zio",
        &[
            "entity-type",
            "entity-slot",
            "entity-slot-age",
            "entity-id-lookup",
            "entity-id-non-nil",
            "entity-id-names-class",
        ],
    );
}

#[test]
fn learn_library_contract() {
    let out = run_contract("learn.zio");
    assert_markers(
        &out,
        "learn.zio",
        &[
            "zero-loss-at-1",
            "zero-loss-at-2",
            "zero-loss-at-3",
            "extrapolates",
            "deterministic",
            "squares",
        ],
    );
}

#[test]
fn pipeline_library_contract() {
    let out = run_contract("pipeline.zio");
    assert_markers(
        &out,
        "pipeline.zio",
        &["filter-map", "aggregate-count", "expansion-shape", "unknown-stage-detected"],
    );
}

#[test]
fn proposer_library_contract() {
    let out = run_contract("proposer.zio");
    assert_markers(
        &out,
        "proposer.zio",
        &[
            "fill-holes",
            "fill-leaves-unknown-data",
            "parse-drops-noise",
            "parse-nil-is-empty",
            "extract-caps-and-dedups",
            "prompt-has-task",
            "prompt-has-ops",
            "prompt-has-history",
            "prompt-empty-history-ok",
            "correction-mentions-format",
            "correction-on-nil-answer",
            "constructor-is-fn",
        ],
    );
}

/// W14: three indexes over one fact base, and the refusals that keep
/// them honest — a different AST is not merged, different probe sets are
/// not compared, and a licence violation is an error not a missing row.
#[test]
fn memory_library_contract() {
    let out = run_contract("memory.zio");
    assert_markers(
        &out,
        "memory.zio",
        &[
            // structural index
            "shape-same",
            "shape-differs",
            "structural-index-count",
            "structural-neighbours",
            "structural-neighbours-other",
            "merge-refuses-different-ast",
            "merge-same-ast-id",
            // behavioural index, under a named probe set
            "probe-same-match",
            "probe-differs-nil",
            "probe-compatible-differs",
            "behavioural-bucket-v1",
            "behavioural-bucket-v2",
            // licence
            "licence-allowed",
            "licence-own-refused-is-string",
            "licence-source-refused-is-string",
            "licence-refusal-carries-id",
            // abstraction extraction
            "abstraction-spine",
            "abstraction-truncates-empty",
            // total description length, definition charged
            "dl-before-is-one-program",
            "dl-counts-definition",
            "dl-two-calls-one-definition",
            "dl-not-per-call",
            "dl-zero-definition-is-call-only",
            "dl-one-call-loses",
            // macro is last resort and re-checked after expansion
            "macro-not-for-short-spine",
            "macro-for-deep-spine",
            "macro-candidate-carries-name",
            "macro-recheck-after-expansion",
            "macro-expansion-not-the-macro-call",
        ],
    );
}

/// W14: the optional semantic index, bound to an encoder and a space
/// version. A space change invalidates the index unless an explicit
/// migration re-encodes it.
#[test]
fn vector_library_contract() {
    let out = run_contract("vector.zio");
    assert_markers(
        &out,
        "vector.zio",
        &[
            "space-ok",
            "space-mismatch-refused",
            "space-mismatch-mentions-version",
            "space-empty-index-ok",
            "encoder-ok",
            "encoder-mismatch-refused",
            "cosine-identical",
            "cosine-orthogonal",
            "cosine-zero-vector",
            "threshold-passes",
            "threshold-fails",
            "nearest-returns-pair",
            "nearest-picks-collinear",
            "nearest-empty-index-nil",
            "query-space-refused",
            "query-space-refusal-not-a-record",
            "query-encoder-refused",
            "migration-sets-space",
            "migration-sets-encoder",
            "migration-records-origin",
            "migration-rewrites-vector",
            "migration-leaves-original",
            "invalidate-empties",
        ],
    );
}
