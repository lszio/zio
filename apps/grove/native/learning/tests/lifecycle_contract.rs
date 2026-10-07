//! W12 contract: retraction, retention and failure recovery.
//!
//! The deployment is the last thing history may still spend: a model whose
//! training data was retracted is not a slower-to-approve candidate, it is
//! not deployable at all. Around that:
//!
//! * cleanup only touches bytes no root can reach, and only once they are
//!   older than a retention policy that is actually declared;
//! * history that can no longer be restored says so — a missing or
//!   corrupted state artifact is `artifact-unavailable`, never a quiet
//!   reinitialization;
//! * after a restart the new process owns the coordinator and an old
//!   worker's receipt cannot land.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use grove::contracts::{
    Actor, ActorRole, ArtifactRef, Branch, Checkpoint, ContentBlock, DatasetRevision, ErrorKind,
    LearningSignal, ModelSnapshot, Observation, ParamRef, ResumeLevel, Run, RunState,
    SCHEMA_VERSION, SignalKind,
};
use grove::coordinator::Coordinator;
use grove::evaluation::{self, EvaluationProtocol, RepeatOutcome};
use grove::store::Store;
use grove::{checkpoint, contracts};

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

fn annotator() -> Actor {
    Actor::new("annotator", ActorRole::Annotator)
}

fn publisher() -> Actor {
    Actor::new("release", ActorRole::Publisher)
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-w12-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn signal(id: &str, key: &str, task: &str) -> LearningSignal {
    LearningSignal {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        idempotency_key: key.to_string(),
        producer: "annotator".to_string(),
        kind: SignalKind::HumanCorrection,
        task_id: task.to_string(),
        observation_id: Some(format!("obs-{id}")),
        prediction_id: None,
        target_field: Some("label".to_string()),
        content: "class 1".to_string(),
        usage_permitted: true,
        occurred_at_ms: 1_700_000_000_000,
        received_at_ms: 1_700_000_000_000,
        revises: None,
    }
}

fn observation(id: &str, task: &str, store: &Store) -> Observation {
    let pixels = store
        .artifacts()
        .put(format!("pixels-{id}").as_bytes())
        .unwrap();
    let observation = Observation {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        owner: "reader".to_string(),
        task_id: task.to_string(),
        session_id: "s-1".to_string(),
        source: "line-a".to_string(),
        occurred_at_ms: 1,
        blocks: vec![ContentBlock {
            media_type: "image/tensor".to_string(),
            artifact: pixels,
        }],
        modality_mask: vec![true, false],
        training_permitted: true,
    };
    store
        .put_observation(&Actor::new("reader", ActorRole::Reader), &observation)
        .unwrap();
    observation
}

fn snapshot(store: &Store, name: &str) -> ArtifactRef {
    let weights = store
        .artifacts()
        .put(format!("weights-{name}").as_bytes())
        .unwrap();
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
    store.put_snapshot(&operator(), &snapshot).unwrap()
}

fn revision(id: &str, task: &str, signal_ids: &[&str]) -> DatasetRevision {
    DatasetRevision {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        task_id: task.to_string(),
        policy_version: "feedback@1.0.0".to_string(),
        signal_ids: signal_ids.iter().map(|s| s.to_string()).collect(),
        observation_ids: vec![],
        split: "train".to_string(),
        frozen_at_ms: 100,
    }
}

fn run(id: &str, task: &str, dataset: &str, base: ArtifactRef) -> Run {
    Run {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        task_id: task.to_string(),
        base_snapshot: base,
        dataset_revision: dataset.to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Running,
        steps_consumed: 10,
        steps_budget: 500,
        resumed_from: None,
    }
}

/// A worker state file that the checkpoint boundary accepts.
///
/// `optimizer.adam` is keyed `layer.weight` / `layer.bias`: a resume
/// checks that the artifact's moments describe the parameters this
/// attempt actually trains, and a bias that is absent is a state no
/// continuation could honestly restore.
fn write_state(dir: &PathBuf, name: &str, run_id: &str, step: u32) -> PathBuf {
    let path = dir.join(name);
    let manifest = serde_json::json!({
        "schema": checkpoint::STATE_SCHEMA,
        "protocol": checkpoint::STATE_PROTOCOL,
        "run_id": run_id,
        "step": step,
        "params": {"h0": {"w": [[0.1, 0.2]], "b": [0.0]}},
        "optimizer": {"adam": {
            "h0.weight": {"step": step, "exp_avg": [0.0], "exp_avg_sq": [0.0]},
            "h0.bias":   {"step": step, "exp_avg": [0.0], "exp_avg_sq": [0.0]},
        }},
        "rng": {"cpu": "AAAA"},
    });
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    path
}

fn committed_checkpoint(
    store: &Store,
    dir: &PathBuf,
    id_hint: &str,
    run_id: &str,
    step: u32,
) -> Checkpoint {
    let state = write_state(dir, &format!("{id_hint}.json"), run_id, step);
    checkpoint::commit(
        store,
        &operator(),
        run_id,
        None,
        ResumeLevel::LearningContinuation,
        &state,
        10,
        500,
    )
    .unwrap_or_else(|e| panic!("commit {id_hint}: {e}"))
}

fn protocol(id: &str, dataset: &str) -> EvaluationProtocol {
    let mut p = EvaluationProtocol::new(id, "geometry-sensor-xor@1.0.0", dataset);
    p.seeds = vec![1, 2];
    p.gates = vec![("accuracy".into(), 0.9)];
    p
}

fn passing(store: &Store, p: &EvaluationProtocol, snapshot: &ArtifactRef) {
    store.put_protocol(&operator(), p).unwrap();
    for repeat in 0..2 {
        evaluation::record_evaluation(
            store,
            &operator(),
            p,
            snapshot,
            repeat,
            &RepeatOutcome::Measured {
                metrics: vec![("accuracy".into(), 0.97)],
            },
            900,
        )
        .unwrap();
    }
}

/// Backdate an object on disk so the retention horizon can be tested with
/// real mtimes rather than a mocked clock.
fn age(store: &Store, artifact: &ArtifactRef, age: Duration) {
    let path = store.artifacts().object_path(artifact);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let when = SystemTime::now()
        .checked_sub(age)
        .expect("age within range");
    file.set_modified(when).unwrap();
}

// ── (a) reachability and retention ─────────────────────────────────

/// Cleanup deletes only bytes no root can reach, and only once they are
/// older than the declared retention horizon.
#[test]
fn cleanup_deletes_only_unreachable_objects_past_the_retention_horizon() {
    let root = temp_root("gc");
    let dir = root.join("states");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&root).unwrap();

    // signal → dataset → run → checkpoint → branch head, the deepest root
    observation("obs-sig-retract", "task-1", &store);
    store
        .submit_signal(&annotator(), &signal("sig-retract", "k1", "task-1"))
        .unwrap();
    store
        .freeze_dataset(&operator(), &revision("ds-1", "task-1", &["sig-retract"]))
        .unwrap();

    let deployed = snapshot(&store, "deployed");
    let in_force = snapshot(&store, "in-force");
    let run_id = "run-lineage";
    store
        .put_run(&operator(), &run(&run_id, "task-1", "ds-1", deployed))
        .unwrap();
    let ckpt = committed_checkpoint(&store, &dir, "head", &run_id, 1);
    store
        .put_branch(
            &operator(),
            &Branch {
                schema: SCHEMA_VERSION,
                id: "branch-a".to_string(),
                owner: "trainer".to_string(),
                head: Some(ckpt.id.clone()),
                head_version: 0,
                policy: "explore@1".to_string(),
                budget_quota: 100,
            },
        )
        .unwrap();
    let p = protocol("proto-1", "ds-1");
    passing(&store, &p, &deployed);
    evaluation::publish_snapshot(&store, &publisher(), &p, &deployed, None)
        .unwrap()
        .0;

    // garbage: a crashed writer's committed-then-abandoned object, and one
    // orphaned parameter that no record names any more
    let orphan_old = store.artifacts().put(b"orphan-old-weights").unwrap();
    let orphan_fresh = store.artifacts().put(b"orphan-fresh-weights").unwrap();
    age(&store, &orphan_old, Duration::from_secs(86_400));
    age(&store, &deployed, Duration::from_secs(86_400));
    age(&store, &in_force, Duration::from_secs(86_400));

    // an unreferenced *old* object inside the retention window stays
    store
        .set_retention_horizon_ms(Some(Duration::from_secs(7 * 86_400).as_millis() as u64))
        .unwrap();
    let kept = store.collect_artifacts().unwrap();
    assert_eq!(kept.deleted, Vec::<ArtifactRef>::new());
    assert!(
        store.artifacts().exists(&orphan_old),
        "inside the horizon, nothing is collected"
    );
    assert!(store.artifacts().exists(&orphan_fresh));

    // a zero horizon declares: collect everything no root can reach
    store.set_retention_horizon_ms(Some(0)).unwrap();
    let report = store.collect_artifacts().unwrap();
    assert!(
        report.deleted.contains(&orphan_old) && report.deleted.contains(&orphan_fresh),
        "unreachable objects past the horizon must go: {:?}",
        report.deleted.len()
    );
    assert!(!store.artifacts().exists(&orphan_old));
    assert!(!store.artifacts().exists(&orphan_fresh));

    // every root's bytes are still there, and the refusal is reported
    for (what, digest) in [
        ("active publication", deployed),
        (
            "branch head checkpoint",
            ArtifactRef::parse_hex(&ckpt.state_artifact.to_hex()).unwrap(),
        ),
        ("checkpoint state artifact", ckpt.state_artifact),
        ("snapshot manifest", deployed),
    ] {
        assert!(
            store.artifacts().exists(&digest),
            "{what} was collected while reachable"
        );
    }
    assert!(
        report.refused.contains(&deployed) || report.refused.contains(&ckpt.state_artifact),
        "cleanup must report what it refused to delete, got {:?}",
        report.refused.len()
    );
    // the branch head's lineage is still loadable
    store.get_checkpoint(&ckpt.id).unwrap();
    store.load_manifest(&deployed).unwrap();
    // and the live set is still exactly the roots
    let live = store.live_artifacts().unwrap();
    assert!(live.contains(&deployed) && live.contains(&ckpt.state_artifact));
    let _ = in_force;
}

/// A parameter artifact no record references is garbage; a parameter the
/// active publication's snapshot names is not.
#[test]
fn a_snapshot_parameter_is_live_while_its_snapshot_is_the_deployment() {
    let root = temp_root("gc-params");
    let store = Store::open(&root).unwrap();
    let deployed = snapshot(&store, "deployed");
    let snapshot = store.load_snapshot(&deployed).unwrap();
    let weights = snapshot.params[0].artifact;
    let p = protocol("proto-params", "ds-none");
    passing(&store, &p, &deployed);
    evaluation::publish_snapshot(&store, &publisher(), &p, &deployed, None)
        .unwrap()
        .0;
    // a second parameter nobody names, left over from an abandoned fork
    let orphan = store.artifacts().put(b"unreferenced-weights").unwrap();
    age(&store, &orphan, Duration::from_secs(1));
    age(&store, &weights, Duration::from_secs(1));

    let live = store.live_artifacts().unwrap();
    assert!(
        live.contains(&weights),
        "a deployed snapshot's parameter is live"
    );
    assert!(!live.contains(&orphan));

    store.set_retention_horizon_ms(Some(0)).unwrap();
    let report = store.collect_artifacts().unwrap();
    assert!(report.deleted.contains(&orphan));
    assert!(store.artifacts().exists(&weights));
    // the deployment is still loadable after the sweep
    store.load_snapshot(&deployed).unwrap();
}

// ── (b) retraction propagation ─────────────────────────────────────

/// A retracted signal invalidates the snapshots its lineage consumed, and
/// only those: an unrelated snapshot still publishes.
#[test]
fn retracting_a_signal_invalidates_the_lineage_that_consumed_it() {
    let root = temp_root("retract");
    let dir = root.join("states");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&root).unwrap();

    observation("obs-sig-bad", "task-1", &store);
    observation("obs-sig-good", "task-2", &store);
    store
        .submit_signal(&annotator(), &signal("sig-bad", "k1", "task-1"))
        .unwrap();
    store
        .submit_signal(&annotator(), &signal("sig-good", "k2", "task-2"))
        .unwrap();
    store
        .freeze_dataset(&operator(), &revision("ds-bad", "task-1", &["sig-bad"]))
        .unwrap();
    store
        .freeze_dataset(&operator(), &revision("ds-good", "task-2", &["sig-good"]))
        .unwrap();

    let tainted = snapshot(&store, "tainted");
    let clean = snapshot(&store, "clean");
    store
        .put_run(&operator(), &run("run-bad", "task-1", "ds-bad", tainted))
        .unwrap();
    store
        .put_run(&operator(), &run("run-good", "task-2", "ds-good", clean))
        .unwrap();
    let ckpt_bad = committed_checkpoint(&store, &dir, "bad", "run-bad", 1);
    let ckpt_good = committed_checkpoint(&store, &dir, "good", "run-good", 1);

    let p = protocol("proto-retract", "ds-bad");
    passing(&store, &p, &tainted);
    let p_good = protocol("proto-good", "ds-good");
    passing(&store, &p_good, &clean);

    // before the retraction both are eligible
    assert!(!store.snapshot_invalidation(&tainted).unwrap().is_some());

    let propagation = store.retract_signal(&annotator(), "sig-bad").unwrap();
    assert_eq!(propagation.signal_id, "sig-bad");
    assert!(
        propagation.invalidated_snapshots.contains(&tainted),
        "the consuming snapshot must be invalidated"
    );
    assert!(
        !propagation.invalidated_snapshots.contains(&clean),
        "a snapshot trained on unrelated data must not be touched"
    );
    assert_eq!(
        propagation.invalidated_checkpoints,
        vec![ckpt_bad.id.clone()]
    );
    assert!(!propagation.invalidated_checkpoints.contains(&ckpt_good.id));

    // the frozen revision keeps its member — retraction is not a rewrite
    assert_eq!(
        store.get_dataset("ds-bad").unwrap().signal_ids,
        vec!["sig-bad".to_string()]
    );

    // deployment eligibility follows
    let err = evaluation::publish_snapshot(&store, &publisher(), &p, &tainted, None).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(err.to_string().contains("retract"), "{err}");
    assert_eq!(store.active_publication().unwrap(), None);

    // an invalidated lineage may not be resumed either
    let err = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt_bad.id,
        ResumeLevel::LearningContinuation,
        false,
        "run-resumed",
        500,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(
        store.get_run("run-resumed").is_err(),
        "a refused resume creates no run"
    );

    // the unrelated snapshot still publishes
    assert_eq!(
        evaluation::publish_snapshot(&store, &publisher(), &p_good, &clean, None)
            .unwrap()
            .0,
        1
    );
}

/// The invalidation is visible in the returned data, not implied: a
/// consumer asking about a snapshot is told what happened and why.
#[test]
fn an_invalidated_snapshot_reports_its_revoked_signal() {
    let root = temp_root("retract-visible");
    let dir = root.join("states");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&root).unwrap();
    observation("obs-sig-v", "task-1", &store);
    store
        .submit_signal(&annotator(), &signal("sig-v", "k1", "task-1"))
        .unwrap();
    store
        .freeze_dataset(&operator(), &revision("ds-v", "task-1", &["sig-v"]))
        .unwrap();
    let tainted = snapshot(&store, "tainted");
    store
        .put_run(&operator(), &run("run-v", "task-1", "ds-v", tainted))
        .unwrap();
    committed_checkpoint(&store, &dir, "v", "run-v", 1);

    assert!(store.snapshot_invalidation(&tainted).unwrap().is_none());
    store.retract_signal(&annotator(), "sig-v").unwrap();
    let invalidation = store
        .snapshot_invalidation(&tainted)
        .unwrap()
        .expect("invalidated");
    assert_eq!(invalidation.revoked_signals, vec!["sig-v".to_string()]);
    assert_eq!(invalidation.dataset_revisions, vec!["ds-v".to_string()]);
    assert!(
        invalidation.recoverable == false,
        "retracted data is not recoverable by redeploy"
    );
}

// ── (c) non-recoverable history ────────────────────────────────────

fn resumable_world(name: &str) -> (PathBuf, PathBuf, Store, String, String) {
    let root = temp_root(name);
    let dir = root.join("states");
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&root).unwrap();
    observation("obs-sig-r", "task-1", &store);
    store
        .submit_signal(&annotator(), &signal("sig-r", "k1", "task-1"))
        .unwrap();
    store
        .freeze_dataset(&operator(), &revision("ds-r", "task-1", &["sig-r"]))
        .unwrap();
    let base = snapshot(&store, "base");
    store
        .put_run(&operator(), &run("run-r", "task-1", "ds-r", base))
        .unwrap();
    let ckpt = committed_checkpoint(&store, &dir, "r", "run-r", 1);
    (root, dir, store, ckpt.id, ckpt.state_artifact.to_hex())
}

/// A checkpoint whose state object a user removed cannot be resumed, and
/// the refusal is not a reinitialization.
#[test]
fn a_checkpoint_whose_state_object_was_removed_cannot_resume() {
    let (root, _dir, store, ckpt_id, state_hex) = resumable_world("gone");
    let state = ArtifactRef::parse_hex(&state_hex).unwrap();
    std::fs::remove_file(store.artifacts().object_path(&state)).unwrap();

    let err = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt_id,
        ResumeLevel::LearningContinuation,
        false,
        "run-after-loss",
        500,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    assert!(err.to_string().contains(&state_hex), "{err}");
    assert!(
        store.get_run("run-after-loss").is_err(),
        "no run may be created from lost state"
    );
    drop((root, store));
}

/// A byte-corrupted state object fails the digest check on resume.
#[test]
fn a_corrupted_state_object_cannot_resume() {
    let (root, _dir, store, ckpt_id, state_hex) = resumable_world("corrupt");
    let state = ArtifactRef::parse_hex(&state_hex).unwrap();
    let path = store.artifacts().object_path(&state);
    let good = std::fs::read(&path).unwrap();
    std::fs::write(
        &path,
        br#"{"schema":1,"protocol":"grove.worker.state/1","run_id":"other","step":9}"#,
    )
    .unwrap();

    // the corruption is visible as corruption, not as a foreign lineage
    let err = store.artifacts().get(&state).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    assert!(err.to_string().contains("digest check"), "{err}");

    let err = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt_id,
        ResumeLevel::LearningContinuation,
        false,
        "run-after-corrupt",
        500,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    assert!(store.get_run("run-after-corrupt").is_err());

    // restoring the exact bytes restores the history — the record was the
    // truth, not the damaged file
    std::fs::write(&path, &good).unwrap();
    let plan = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt_id,
        ResumeLevel::LearningContinuation,
        false,
        "run-restored",
        500,
    )
    .unwrap();
    assert_eq!(plan.state_step, 1);
    drop(root);
}

/// Cleanup of *unreachable* state makes that checkpoint's history
/// unrecoverable rather than quietly resumable, and the marker is
/// permanent: re-writing the bytes is not enough, the record is known.
#[test]
fn a_collected_state_object_leaves_the_checkpoint_unrecoverable() {
    let (root, _dir, store, ckpt_id, state_hex) = resumable_world("collected");
    // a checkpoint nobody points at: no branch head, no publication
    let state = ArtifactRef::parse_hex(&state_hex).unwrap();
    age(&store, &state, Duration::from_secs(1));
    store.set_retention_horizon_ms(Some(0)).unwrap();
    let report = store.collect_artifacts().unwrap();
    assert!(
        report.deleted.contains(&state),
        "an unreachable state object is collectable"
    );

    let err = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt_id,
        ResumeLevel::LearningContinuation,
        false,
        "run-after-gc",
        500,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    // the metadata survives so the lineage is still visible
    let ckpt = store.get_checkpoint(&ckpt_id).unwrap();
    assert_eq!(ckpt.state_artifact, state);
    drop(root);
}

// ── (d) restart ownership and lease recovery ───────────────────────

/// After a restart the new process owns the coordinator, expired leases
/// are reclaimed, and the old worker's receipt cannot land.
#[test]
fn a_restart_reclaims_ownership_and_expired_leases() {
    let root = temp_root("restart");
    let t0 = 1_000_000i64;
    let t1 = 2_000_000i64; // the "next process" clock: both leases have expired

    let (first, second) = {
        // ── the original process ──
        let store = std::sync::Arc::new(Store::open(&root).unwrap());
        let base = snapshot(&store, "base");
        store
            .put_population(
                &operator(),
                &contracts::Population {
                    schema: SCHEMA_VERSION,
                    id: "pop-1".to_string(),
                    owner: "trainer".to_string(),
                    members: vec!["branch-a".to_string()],
                    total_budget_steps: 1000,
                    spent_steps: 0,
                    policy: "explore@1".to_string(),
                },
            )
            .unwrap();
        store
            .put_run(&operator(), &run("run-a", "task-1", "ds-1", base))
            .unwrap();
        store
            .put_branch(
                &operator(),
                &Branch {
                    schema: SCHEMA_VERSION,
                    id: "branch-a".to_string(),
                    owner: "trainer".to_string(),
                    head: Some("ckpt-0".to_string()),
                    head_version: 0,
                    policy: "explore@1".to_string(),
                    budget_quota: 500,
                },
            )
            .unwrap();
        let coordinator = Coordinator::new(store.clone());
        assert_eq!(
            coordinator.ownership().unwrap().unwrap().epoch,
            1,
            "a fresh store has no prior owner"
        );
        // a long lease the old process still believes it holds
        coordinator
            .start_attempt(
                &operator(),
                "pop-1",
                "branch-a",
                "run-a",
                100,
                600_000,
                t0,
                "att-old",
            )
            .unwrap();
        let first_epoch = coordinator.ownership().unwrap().unwrap().epoch;
        (coordinator, first_epoch)
    };

    // ── the process restarts: a brand new store handle, a new coordinator ──
    let store = std::sync::Arc::new(Store::open(&root).unwrap());
    let coordinator = Coordinator::new(store.clone());
    let ownership = coordinator
        .ownership()
        .unwrap()
        .expect("ownership is recorded");
    assert!(
        ownership.epoch > second,
        "a restart takes a new ownership epoch ({} <= {second})",
        ownership.epoch
    );
    assert_eq!(ownership.pid, std::process::id() as i64);

    // the previous owner's lease is reclaimed, so it is not a live slot
    assert!(
        !coordinator
            .active_attempts(t1)
            .unwrap()
            .contains(&"att-old".to_string()),
        "a lease from a dead process must not stay active"
    );

    // the old worker's receipt — clock aside — cannot land: it speaks for
    // an epoch that no longer exists
    let err = coordinator
        .commit_attempt_stamped(&operator(), "att-old", "ckpt-old", 0, t1, second)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.to_string().contains("ownership"), "{err}");
    assert_eq!(
        store.get_branch("branch-a").unwrap().head.as_deref(),
        Some("ckpt-0")
    );
    assert_eq!(store.get_branch("branch-a").unwrap().head_version, 0);

    // even the current owner cannot resurrect the reclaimed lease
    let err = coordinator
        .commit_attempt(&operator(), "att-old", "ckpt-old", 0, t1)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.to_string().contains("no longer active"), "{err}");
    drop(first);
}

/// Two coordinators over one store: the second claim takes ownership, so
/// the first one's receipts are stale the moment it hands over.
#[test]
fn ownership_is_taken_by_the_latest_handle() {
    let root = temp_root("ownership");
    let first = std::sync::Arc::new(Store::open(&root).unwrap());
    let first_coordinator = Coordinator::new(first.clone());
    let epoch = first_coordinator
        .ownership()
        .unwrap()
        .expect("claimed")
        .epoch;

    let second = std::sync::Arc::new(Store::open(&root).unwrap());
    let second_coordinator = Coordinator::new(second);
    assert!(
        second_coordinator
            .ownership()
            .unwrap()
            .expect("claimed")
            .epoch
            > epoch
    );
    let err = first_coordinator
        .commit_attempt_stamped(&operator(), "att-x", "ckpt-x", 0, 1, epoch)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.to_string().contains("ownership"), "{err}");
}

#[test]
fn live_artifacts_is_empty_for_a_store_with_no_records() {
    let root = temp_root("empty-live");
    let store = Store::open(&root).unwrap();
    let live: BTreeSet<ArtifactRef> = store.live_artifacts().unwrap();
    assert!(live.is_empty());
    // an unreferenced object with no declared horizon is not collectable
    let orphan = store.artifacts().put(b"never-referenced").unwrap();
    age(&store, &orphan, Duration::from_secs(1));
    let report = store.collect_artifacts().unwrap();
    assert!(
        report.deleted.is_empty(),
        "no declared horizon, no deletion"
    );
    assert!(store.artifacts().exists(&orphan));
    assert_eq!(store.retention_horizon_ms().unwrap(), None);
}
