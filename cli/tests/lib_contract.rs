use std::path::PathBuf;
use std::process::Command;

/// Contract tests for the extension libraries in lib/zio. Each contract
/// file loads its library, exercises every public function, and prints
/// PASS/FAIL markers. This is the guardrail that would have caught the
/// libraries shipping with broken functions (undefined symbols, parallel
/// `let` misuse) — see the 2026-09 architecture review.

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("cli crate lives below workspace root")
        .to_path_buf()
}

fn run_contract(file: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_zio-cli"))
        .arg(workspace_root().join("cli").join("tests").join("libs").join(file))
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
            "make-agent-name",
            "make-agent-status",
            "make-agent-empty-history",
            "register-tool-count",
            "step-status",
            "step-history-count",
            "step-history-role",
            "run",
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
