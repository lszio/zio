//! W07 contract: comparison is protocol-scoped, gates precede
//! publication, and the shared acceptance budget cannot be forked away.

use std::path::PathBuf;

use grove::contracts::{
    Actor, ActorRole, ArtifactRef, ErrorKind, EvaluationRecord, ModelSnapshot, ParamRef,
    SCHEMA_VERSION,
};
use grove::evaluation::{self, EvaluationProtocol, RepeatOutcome};
use grove::store::Store;

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

fn publisher() -> Actor {
    Actor::new("release", ActorRole::Publisher)
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-w07-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A store with one committed model snapshot per name, owned by `trainer`.
fn store_with_snapshots(name: &str, names: &[&str]) -> (Store, Vec<(String, ArtifactRef)>) {
    let root = temp_root(name);
    let store = Store::open(&root).unwrap();
    let mut out = Vec::new();
    for n in names {
        let weights = store.artifacts().put(n.as_bytes()).unwrap();
        let snapshot = ModelSnapshot {
            schema: SCHEMA_VERSION,
            owner: "trainer".to_string(),
            entrypoint: "predict".to_string(),
            params: vec![ParamRef {
                module: "fusion".into(),
                shape: vec![1],
                dtype: "float32".into(),
                artifact: weights,
            }],
            libraries: vec![],
            preprocessing_version: "geometry-sensor-xor@1.0.0".into(),
            modules: vec![],
            ensemble: None,
            graph: None,
        };
        let digest = store
            .commit_manifest("ModelSnapshot", "trainer", &snapshot)
            .unwrap();
        out.push((n.to_string(), digest));
    }
    (store, out)
}

fn protocol(id: &str) -> EvaluationProtocol {
    let mut p = EvaluationProtocol::new(id, "geometry-sensor-xor@1.0.0", "ds-1");
    p.seeds = vec![1, 2, 3];
    p.gates = vec![("accuracy".to_string(), 0.90)];
    p
}

fn measured(accuracy: f64) -> RepeatOutcome {
    RepeatOutcome::Measured {
        metrics: vec![("accuracy".to_string(), accuracy)],
    }
}

// ── append-only history ────────────────────────────────────────────

#[test]
fn reevaluating_a_historical_model_appends_never_rewrites() {
    let (store, snapshots) = store_with_snapshots("append", &["old-linear"]);
    let old = snapshots[0].1;
    let p = protocol("accept-v1");

    // the model's first life: evaluated long ago, borderline
    evaluation::record_evaluation(&store, &operator(), &p, &old, 0, &measured(0.72), 100).unwrap();

    // history re-evaluation under the SAME protocol: a new record
    evaluation::record_evaluation(&store, &operator(), &p, &old, 1, &measured(0.74), 200).unwrap();

    let records = store.evaluations_for(&old).unwrap();
    assert_eq!(records.len(), 2, "records are append-only");
    assert_eq!(records[0].metrics[0].1, 0.72, "the original record is unchanged");
    assert_eq!(records[1].metrics[0].1, 0.74);

    // the comparison consumes both repeats and does not mutate them
    let rows = evaluation::compare(&store, &p, &[old]).unwrap();
    assert_eq!(rows[0].repeats, 2);
    assert!((rows[0].mean[0].1 - 0.73).abs() < 1e-9);
    assert_eq!(records[0].metrics[0].1, 0.72, "comparison must not rewrite records");
}

#[test]
fn records_from_different_protocols_are_never_mixed() {
    let (store, snapshots) = store_with_snapshots("protocols", &["model-a"]);
    let a = snapshots[0].1;

    let strict = protocol("accept-v1");
    let loose = {
        let mut p = protocol("accept-v2-easier");
        p.gates = vec![("accuracy".to_string(), 0.50)];
        p
    };
    evaluation::record_evaluation(&store, &operator(), &strict, &a, 0, &measured(0.55), 1).unwrap();
    evaluation::record_evaluation(&store, &operator(), &loose, &a, 0, &measured(0.95), 2).unwrap();

    // under the strict protocol the model is at 0.55 — the easier
    // protocol's 0.95 must not leak into that comparison
    let rows = evaluation::compare(&store, &strict, &[a]).unwrap();
    assert_eq!(rows[0].repeats, 1);
    assert!((rows[0].mean[0].1 - 0.55).abs() < 1e-9);
    assert!(!rows[0].meets_gates);

    // and publishing against the strict protocol is refused even though an
    // easier protocol said yes
    let err = evaluation::publish_snapshot(&store, &publisher(), &strict, &a, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(err.to_string().contains("hard gates"), "{err}");
}

#[test]
fn a_timed_out_repeat_stays_in_the_denominator() {
    let (store, snapshots) = store_with_snapshots("timeout", &["model-t"]);
    let t = snapshots[0].1;
    let p = protocol("accept-v1");

    evaluation::record_evaluation(&store, &operator(), &p, &t, 0, &measured(0.95), 1)
        .unwrap();
    // the second repeat times out: quality 0.0, still counted
    evaluation::record_evaluation(&store, &operator(), &p, &t, 1, &RepeatOutcome::TimedOut, 2)
        .unwrap();

    let rows = evaluation::compare(&store, &p, &[t]).unwrap();
    assert_eq!(rows[0].repeats, 2, "a timeout never leaves the denominator");
    assert!((rows[0].mean[0].1 - 0.475).abs() < 1e-9, "timeout scores as failure");
    assert!(!rows[0].meets_gates);
}

#[test]
fn non_finite_metrics_are_refused_at_the_boundary() {
    let (store, snapshots) = store_with_snapshots("nan", &["model-n"]);
    let n = snapshots[0].1;
    let p = protocol("accept-v1");
    let err = evaluation::record_evaluation(
        &store,
        &operator(),
        &p,
        &n,
        0,
        &RepeatOutcome::Measured {
            metrics: vec![("accuracy".to_string(), f64::NAN)],
        },
        1,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    assert!(store.evaluations_for(&n).unwrap().is_empty());
}

// ── gates before publication ───────────────────────────────────────

#[test]
fn a_high_mean_cannot_outvote_a_failed_gate() {
    let (store, snapshots) = store_with_snapshots("gate", &["lucky", "solid"]);
    let lucky = snapshots[0].1;
    let solid = snapshots[1].1;
    let p = protocol("accept-v1");

    // "lucky": two great repeats, one crash — mean 0.633, gate 0.90 fails
    evaluation::record_evaluation(&store, &operator(), &p, &lucky, 0, &measured(0.95), 1).unwrap();
    evaluation::record_evaluation(&store, &operator(), &p, &lucky, 1, &measured(0.95), 2).unwrap();
    evaluation::record_evaluation(
        &store,
        &operator(),
        &p,
        &lucky,
        2,
        &RepeatOutcome::Crashed("oom".into()),
        3,
    )
    .unwrap();

    // "solid": three repeats over the gate
    for i in 0..3 {
        evaluation::record_evaluation(&store, &operator(), &p, &solid, i, &measured(0.91), 4 + i as i64)
            .unwrap();
    }

    let rows = evaluation::compare(&store, &p, &[lucky, solid]).unwrap();
    let by_name: Vec<bool> = rows.iter().map(|r| r.meets_gates).collect();
    assert_eq!(by_name, vec![false, true]);

    // publishing the crashed-candidate is refused even though two repeats
    // were excellent
    let err = evaluation::publish_snapshot(&store, &publisher(), &p, &lucky, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    // the eligible one publishes
    assert_eq!(
        evaluation::publish_snapshot(&store, &publisher(), &p, &solid, None).unwrap().0,
        1
    );
    assert_eq!(store.active_publication().unwrap().unwrap().1, solid);
}

#[test]
fn a_stale_expected_version_does_not_replace_the_active_model() {
    let (store, snapshots) = store_with_snapshots("stale", &["first", "second"]);
    let (first, second) = (snapshots[0].1, snapshots[1].1);
    let p = protocol("accept-v1");
    for (i, s) in [first, second].iter().enumerate() {
        evaluation::record_evaluation(&store, &operator(), &p, s, 0, &measured(0.95), 1 + i as i64)
            .unwrap();
    }

    assert_eq!(
        evaluation::publish_snapshot(&store, &publisher(), &p, &first, None).unwrap().0,
        1
    );
    assert_eq!(
        evaluation::publish_snapshot(&store, &publisher(), &p, &second, Some(1)).unwrap().0,
        2
    );
    // a writer that saw version 1 loses
    let err =
        evaluation::publish_snapshot(&store, &publisher(), &p, &first, Some(1)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(store.active_publication().unwrap().unwrap().1, second);
}

#[test]
fn publishing_without_evaluation_is_refused() {
    let (store, snapshots) = store_with_snapshots("unevaled", &["dark-horse"]);
    let p = protocol("accept-v1");
    let err =
        evaluation::publish_snapshot(&store, &publisher(), &p, &snapshots[0].1, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(store.active_publication().unwrap().is_none());
}

#[test]
fn an_operator_cannot_publish() {
    let (store, snapshots) = store_with_snapshots("role", &["m"]);
    let p = protocol("accept-v1");
    evaluation::record_evaluation(&store, &operator(), &p, &snapshots[0].1, 0, &measured(0.95), 1)
        .unwrap();
    let err = evaluation::publish_snapshot(
        &store,
        &Actor::new("trainer", ActorRole::Operator),
        &p,
        &snapshots[0].1,
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

// ── shared acceptance budget ───────────────────────────────────────

#[test]
fn the_acceptance_budget_is_shared_across_branches() {
    let (store, _) = store_with_snapshots("budget", &[]);
    store.provision_acceptance_budget("accept-v1", 5).unwrap();

    // branch A draws 3
    assert_eq!(store.consume_acceptance_budget("accept-v1", 3).unwrap(), 2);
    // branch B inherits the line: only 2 left, no matter how many forks
    assert_eq!(store.consume_acceptance_budget("accept-v1", 2).unwrap(), 0);
    // a fork cannot buy extra holdout looks
    let err = store.consume_acceptance_budget("accept-v1", 1).unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted);

    // an unprovisioned protocol has no budget at all
    assert_eq!(
        store.consume_acceptance_budget("never-provisioned", 1).unwrap_err().kind,
        ErrorKind::ArtifactUnavailable
    );
}

#[test]
fn evaluation_records_round_trip_with_their_protocol() {
    let (store, snapshots) = store_with_snapshots("roundtrip", &["m"]);
    let p = protocol("accept-v1");
    let record = evaluation::record_evaluation(
        &store,
        &operator(),
        &p,
        &snapshots[0].1,
        0,
        &measured(0.93),
        7,
    )
    .unwrap();
    let loaded: EvaluationRecord = store.evaluations_for(&snapshots[0].1).unwrap().remove(0);
    assert_eq!(loaded, record);
    assert_eq!(loaded.protocol_id, "accept-v1");
    assert_eq!(loaded.repeat_index, 0);
    assert_eq!(loaded.device, "cpu");
}
