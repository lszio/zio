//! W08 end-to-end: the `grove` binary's commands against a real store,
//! real worker processes and the frozen W00 task.
//!
//! The demo's report is asserted on its substance — accuracies, the lift,
//! the ledger continuity — never on "something came back".

use std::path::PathBuf;
use std::process::Command;

use grove_app::{demo, inspect, open_store, operator, Paths};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn paths() -> Paths {
    Paths::from_repo_root()
}

fn venv_ready() -> bool {
    paths().python.exists()
}

fn isolation_available() -> bool {
    grove::worker::Isolation::probe("unshare")
        .map(|iso| iso.enforce().is_ok())
        .unwrap_or(false)
}

fn bin_path() -> PathBuf {
    // the integration test target sits next to the binary in target/
    repo_root().join("target/debug/grove")
}

fn grove(args: &[&str]) -> (String, String, bool) {
    let out = Command::new(bin_path())
        .args(args)
        .current_dir(&repo_root())
        .output()
        .expect("the grove binary is built by cargo test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

#[test]
fn the_dual_demo_clears_the_frozen_gates_end_to_end() {
    if !venv_ready() || !isolation_available() {
        eprintln!("skipping: no torch venv or no namespace isolation");
        return;
    }
    let root = std::env::temp_dir().join(format!("grove-w08-{}", std::process::id()));
    let report = demo::run_dual(&root, &paths(), "cpu").expect("the demo completes");

    // the substance: real numbers on both populations, the lift, the pause
    assert!(report.contains("baseline (linear fusion)"), "{report}");
    assert!(report.contains("candidate (nonlinear, paused+resumed)"), "{report}");
    assert!(report.contains("paused at step 150"), "{report}");
    // The demo produced a candidate and stopped. It did not publish:
    // reaching the publisher role by calling a function is not a human
    // decision, and G04 removed exactly that shortcut.
    assert!(
        report.contains("publication: none"),
        "the demo must not promote its own candidate: {report}"
    );
    assert!(
        report.contains("awaits human approval"),
        "and it must say the candidate is waiting for one: {report}"
    );

    // and the store agrees: the lineage ledger is continuous (150 + 150),
    // the paused run stayed paused, and NOTHING is deployed
    let store = open_store(&root).unwrap();
    let runs = store.runs();
    let baseline = runs.iter().find(|r| r.id == "run-baseline").unwrap();
    assert_eq!(baseline.steps_consumed, 300);
    let joint = runs.iter().find(|r| r.id == "run-joint").unwrap();
    assert_eq!(
        joint.state,
        grove::contracts::RunState::Paused,
        "the paused run keeps its record"
    );
    let resumed = runs.iter().find(|r| r.id == "run-joint-resumed").unwrap();
    // a resumed record carries the LINEAGE total: 150 inherited at the
    // boundary plus 150 for its own leg. Summing records would double-count
    // the inherited part — the latest record IS the ledger.
    assert_eq!(resumed.steps_consumed, 300, "150 inherited + 150 leg two");
    assert_eq!(joint.steps_consumed, 150, "the paused run keeps its own ledger");
    assert!(
        store.active_publication().unwrap().is_none(),
        "the demo left the deployment alone"
    );

    // The candidate the demo produced is a real trained model, not the
    // run's placeholder contract: `run-joint`'s `base_snapshot` only
    // ever held a seed, and approving that would point the deployment at
    // bytes nobody trained.
    let joint_run = runs.iter().find(|r| r.id == "run-joint").unwrap();
    let hex = report
        .lines()
        .filter(|l| l.contains("--snapshot"))
        .last()
        .and_then(|l| l.rsplit(' ').next())
        .expect("the demo names the candidate it produced");
    let candidate = grove::contracts::ArtifactRef::parse_hex(hex).unwrap();
    assert_ne!(
        candidate,
        joint_run.base_snapshot,
        "the candidate must be the trained weights, not the run's placeholder"
    );
    assert!(
        !store.evaluations_for(&candidate).unwrap().is_empty(),
        "the demo must score the weights it actually trained"
    );

    // NOW a human approves it, through the CLI, with the real weights.
    let (out, err, ok) = grove(&[
        "approve",
        "--root",
        root.to_str().unwrap(),
        "--protocol",
        "accept-v1",
        "--snapshot",
        hex,
    ]);
    assert!(ok, "approve failed: {out}{err}");
    assert!(out.contains("approved by"), "{out}");

    let active = store.active_publication().unwrap().unwrap();
    assert_eq!(active.0, 1);
    assert_eq!(active.1, candidate, "the pointer names the trained weights");
    // the published snapshot carries real parameters, and its graph
    let published = store.load_snapshot(&active.1).unwrap();
    assert!(
        !published.params.is_empty(),
        "the published snapshot has no parameters; it is not a model"
    );
    assert!(
        published.graph.is_some(),
        "the published snapshot declares no operator graph, so it cannot serve"
    );
    for param in &published.params {
        let bytes = store.artifacts().get(&param.artifact).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let entry = &parsed["params"][&param.module];
        assert!(
            entry.get("w").is_some() && entry.get("b").is_some(),
            "layer {} must carry both a weight and a bias; a missing bias \
             describes a different model than the one trained",
            param.module
        );
    }
    // and it is the model the evaluation belongs to: the accuracy record
    // under this protocol is bound to THIS digest
    let records = store.evaluations_for(&active.1).unwrap();
    assert!(
        records.iter().any(|r| r.protocol_id == "accept-v1"
            && r.metrics.iter().any(|(n, v)| n == "accuracy" && *v >= 0.90)),
        "the published snapshot has no passing evaluation under the frozen \
         protocol: {records:?}"
    );
    // and the decision is on the record, naming who made it
    let approvals = store.approval_log().unwrap();
    assert_eq!(approvals.len(), 1, "one approval, one publication: {approvals:?}");
    assert_eq!(approvals[0]["authenticated_actor"], "grove-cli");
}

#[test]
fn an_underfit_model_cannot_be_approved() {
    if !venv_ready() || !isolation_available() {
        eprintln!("skipping: no torch venv or no namespace isolation");
        return;
    }
    let root = std::env::temp_dir().join(format!("grove-w08-underfit-{}", std::process::id()));
    let report = demo::run_dual(&root, &paths(), "cpu").unwrap();
    assert!(
        report.contains("publication: none"),
        "the demo publishes nothing: {report}"
    );

    // The BASELINE (0.53 on the XOR) has an evaluation record but fails
    // the 0.90 gate: approving it must be refused, loudly.
    let store = open_store(&root).unwrap();
    let runs = store.runs();
    let baseline = runs.iter().find(|r| r.id == "run-baseline").unwrap();
    let hex = baseline.base_snapshot.to_hex();
    let (out, err, ok) = grove(&[
        "approve",
        "--root",
        root.to_str().unwrap(),
        "--protocol",
        "accept-v1",
        "--snapshot",
        &hex,
    ]);
    assert!(!ok, "an underfit model must not be approved: {out}");
    assert!(err.contains("hard gates"), "{err}");
    assert!(
        store.active_publication().unwrap().is_none(),
        "a refused approval leaves nothing deployed"
    );
}

#[test]
fn inspect_reports_the_history_the_store_holds() {
    if !venv_ready() || !isolation_available() {
        eprintln!("skipping: no torch venv or no namespace isolation");
        return;
    }
    let root = std::env::temp_dir().join(format!("grove-w08-inspect-{}", std::process::id()));
    let demo_report = demo::run_dual(&root, &paths(), "cpu").unwrap();

    let store = open_store(&root).unwrap();
    let report = inspect(&store).unwrap();
    assert!(report.contains("runs (3)"), "all three run records: {report}");
    assert!(report.contains("run-baseline"), "{report}");
    assert!(report.contains("run-joint-resumed"), "{report}");
    assert!(report.contains("checkpoints (1)"), "the pause checkpoint: {report}");
    // the demo alone published nothing, and `inspect` says so
    assert!(
        report.contains("publication: none"),
        "nothing is deployed before a human approves: {report}"
    );

    // approve the candidate the demo named, then the history records the
    // decision. The candidate is the *trained* snapshot the demo
    // committed, not the run's `base_snapshot` — that one only ever
    // held a seed.
    let hex = demo_report
        .lines()
        .filter(|l| l.contains("--snapshot"))
        .last()
        .and_then(|l| l.rsplit(' ').next())
        .expect("the demo names the candidate it produced");
    let (approve_out, err, ok) = grove(&[
        "approve",
        "--root",
        root.to_str().unwrap(),
        "--protocol",
        "accept-v1",
        "--snapshot",
        hex,
    ]);
    assert!(ok, "{approve_out}{err}");

    let after = inspect(&store).unwrap();
    assert!(after.contains("publication: v1"), "{after}");
    assert!(after.contains("approvals (1)"), "the decision is on the record: {after}");

    // and through the binary, same story
    let (out, _, ok) = grove(&["inspect", "--root", root.to_str().unwrap()]);
    assert!(ok);
    assert!(out.contains("publication: v1"), "{out}");
}

#[test]
fn select_compares_under_one_protocol_only() {
    if !venv_ready() || !isolation_available() {
        eprintln!("skipping: no torch venv or no namespace isolation");
        return;
    }
    let root = std::env::temp_dir().join(format!("grove-w08-select-{}", std::process::id()));
    demo::run_dual(&root, &paths(), "cpu").unwrap();
    let store = open_store(&root).unwrap();
    let runs = store.runs();
    let baseline = runs.iter().find(|r| r.id == "run-baseline").unwrap();
    let joint = runs.iter().find(|r| r.id == "run-joint").unwrap();
    let hexes = format!(
        "{},{}",
        baseline.base_snapshot.to_hex(),
        joint.base_snapshot.to_hex()
    );

    let (out, _, ok) = grove(&[
        "select",
        "--root",
        root.to_str().unwrap(),
        "--protocol",
        "accept-v1",
        "--snapshots",
        &hexes,
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("non-dominated candidates"), "{out}");
    // the underfit baseline is named as a gate failure, not silently ranked
    assert!(out.contains("GATE FAILED"), "{out}");

    // a protocol with no records for these snapshots is refused, not
    // answered with an empty table
    let (_, err, ok) = grove(&[
        "select",
        "--root",
        root.to_str().unwrap(),
        "--protocol",
        "never-ran",
        "--snapshots",
        &hexes,
    ]);
    assert!(!ok, "{err}");
    assert!(err.contains("not provisioned"), "{err}");
    // a provisioned protocol with no records still refuses with the
    // comparison-level message
    let (_, err2, ok2) = grove(&[
        "select",
        "--root",
        root.to_str().unwrap(),
        "--protocol",
        "accept-v1",
        "--snapshots",
        "0000000000000000000000000000000000000000000000000000000000000000",
    ]);
    assert!(!ok2, "{err2}");
    assert!(err2.contains("no evaluation records"), "{err2}");
}

#[test]
fn unknown_commands_and_flags_are_rejected_with_usage() {
    let (_, err, ok) = grove(&["launch-missiles"]);
    assert!(!ok);
    assert!(err.contains("unknown command"), "{err}");

    let (_, err2, ok2) = grove(&["inspect", "--root", "/tmp/x", "--bogus", "1"]);
    assert!(!ok2);
    assert!(err2.contains("unknown flag"), "{err2}");

    let (_, err3, ok3) = grove(&[]);
    assert!(!ok3);
    assert!(err3.contains("usage:"), "{err3}");
}

#[test]
fn the_operator_role_cannot_publish_through_the_cli() {
    // the CLI's publisher escalation is role-based; the point of this
    // contract is that inspect (a read) needs no publisher, while publish
    // requires one — enforced in the library, not the shell
    let root = std::env::temp_dir().join(format!("grove-w08-role-{}", std::process::id()));
    let store = open_store(&root).unwrap();
    let op = operator();
    assert!(op.may(grove::contracts::ActorRole::Operator));
    assert!(!op.may(grove::contracts::ActorRole::Publisher));
    let _ = store;
}
