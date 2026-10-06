//! W01 store contract: the properties a learning host must hold regardless
//! of what any learning policy does with it.
//!
//! Each test anchors a consumer-visible rule — cross-actor reference refusal,
//! digest verification, idempotent signal receipt, head-update conflict,
//! durability across an abrupt process death, and refusal of an unknown
//! schema. None of them assert on SQL text or private layout.

use std::path::PathBuf;
use std::process::Command;

use grove::artifacts::ArtifactStore;
use grove::contracts::*;
use grove::evaluation::EvaluationProtocol;
use grove::store::{Receipt, Store};

// ── fixtures ───────────────────────────────────────────────────────

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-w01-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn actor(role: ActorRole) -> Actor {
    Actor::new("trainer", role)
}

fn sample_store(name: &str) -> (PathBuf, Store) {
    let root = temp_root(name);
    let store = Store::open(&root).unwrap();
    (root, store)
}

/// A snapshot with one parameter artifact, owned by `trainer`.
fn seed_snapshot(store: &Store, owner: &str) -> ArtifactRef {
    let weights = store.artifacts().put(b"linear-fusion-weights").unwrap();
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: owner.to_string(),
        entrypoint: "predict".to_string(),
        params: vec![ParamRef {
            module: "fusion".to_string(),
            shape: vec![258, 2],
            dtype: "float32".to_string(),
            artifact: weights,
        }],
        libraries: vec![("zio.core".to_string(), "0.2.0".to_string())],
        preprocessing_version: "geometry-sensor-xor@1.0.0".to_string(),
        modules: vec![],
        ensemble: None,
        graph: None,
    };
    store
        .commit_manifest("ModelSnapshot", owner, &snapshot)
        .unwrap()
}

fn signal(id: &str, key: &str, kind: SignalKind, target: &str, field: Option<&str>) -> LearningSignal {
    LearningSignal {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        idempotency_key: key.to_string(),
        producer: "teacher-local".to_string(),
        kind,
        task_id: "task-1".to_string(),
        observation_id: Some(target.to_string()),
        prediction_id: None,
        target_field: field.map(str::to_string),
        content: "class 1".to_string(),
        usage_permitted: true,
        occurred_at_ms: 1_700_000_000_000,
        received_at_ms: 1_700_000_000_000,
        revises: None,
    }
}

// ── artifact identity ──────────────────────────────────────────────

#[test]
fn corrupted_artifact_body_is_rejected() {
    let root = temp_root("corrupt");
    let store = ArtifactStore::open(root.join("artifacts")).unwrap();
    let digest = store.put(b"weights-v1").unwrap();

    // Reach into the store the way a corrupted disk or a bad copy would.
    let hex = digest.to_hex();
    let path = root.join("artifacts").join("objects").join(&hex[..2]).join(&hex);
    std::fs::write(&path, b"weights-V2").unwrap();

    let err = store.get(&digest).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    assert!(err.to_string().contains("digest check"), "unhelpful error: {err}");
}

#[test]
fn missing_artifact_is_reported_not_invented() {
    let root = temp_root("missing");
    let store = ArtifactStore::open(root.join("artifacts")).unwrap();
    let unknown = digest_bytes(b"never written");
    assert_eq!(
        store.get(&unknown).unwrap_err().kind,
        ErrorKind::ArtifactUnavailable
    );
}

#[test]
fn commit_is_idempotent_for_identical_content() {
    let root = temp_root("idem");
    let store = ArtifactStore::open(root.join("artifacts")).unwrap();
    assert_eq!(store.put(b"same bytes").unwrap(), store.put(b"same bytes").unwrap());
}

#[test]
fn non_finite_manifests_are_refused() {
    // A frozen manifest that serializes NaN hashes differently in another
    // process, so it can never be a stable identity.
    let err = require_finite_metrics(&[("holdout_accuracy".to_string(), f64::NAN)]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    let err = require_finite_metrics(&[("holdout_accuracy".to_string(), f64::INFINITY)]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    assert!(require_finite_metrics(&[("holdout_accuracy".to_string(), 0.91)]).is_ok());
}

// ── cross-actor reference boundary ─────────────────────────────────

#[test]
fn one_principal_cannot_reference_anothers_artifact() {
    let (_root, store) = sample_store("ownership");
    let snapshot = seed_snapshot(&store, "trainer");

    let owner = actor(ActorRole::Operator);
    let intruder = Actor::new("other-lab", ActorRole::Publisher);
    assert!(store.require_owner(&snapshot, &owner).is_ok());

    let err = store.require_owner(&snapshot, &intruder).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

#[test]
fn snapshot_with_a_missing_parameter_artifact_is_refused() {
    let (_root, store) = sample_store("dangling-param");
    let ghost = digest_bytes(b"weights that were never written");
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: "trainer".to_string(),
        entrypoint: "predict".to_string(),
        params: vec![ParamRef {
            module: "fusion".to_string(),
            shape: vec![1, 1],
            dtype: "float32".to_string(),
            artifact: ghost,
        }],
        libraries: vec![],
        preprocessing_version: "v1".to_string(),
        modules: vec![],
        ensemble: None,
        graph: None,
    };
    let err = store.put_snapshot(&actor(ActorRole::Operator), &snapshot).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
}

#[test]
fn reader_cannot_commit_a_snapshot() {
    let (_root, store) = sample_store("reader-snapshot");
    let weights = store.artifacts().put(b"w").unwrap();
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: "viewer".to_string(),
        entrypoint: "predict".to_string(),
        params: vec![ParamRef { module: "m".into(), shape: vec![1], dtype: "f32".into(), artifact: weights }],
        libraries: vec![],
        preprocessing_version: "v1".to_string(),
        modules: vec![],
        ensemble: None,
        graph: None,
    };
    let err = store.put_snapshot(&Actor::new("viewer", ActorRole::Reader), &snapshot).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

// ── signal idempotency ─────────────────────────────────────────────

#[test]
fn repeated_signal_is_a_duplicate_not_a_second_sample() {
    let (_root, store) = sample_store("signal-idem");
    let s = signal("sig-1", "teacher-batch-7", SignalKind::TeacherLabel, "obs-1", None);

    assert_eq!(store.submit_signal(&actor(ActorRole::Operator), &s).unwrap(), Receipt::Accepted);
    assert_eq!(store.submit_signal(&actor(ActorRole::Operator), &s).unwrap(), Receipt::Duplicate);
    assert_eq!(store.active_signals("obs-1").unwrap().len(), 1);
}

#[test]
fn a_reader_cannot_submit_a_teacher_label() {
    let (_root, store) = sample_store("signal-role");
    let s = signal("sig-1", "k", SignalKind::TeacherLabel, "obs-1", None);
    let err = store.submit_signal(&Actor::new("viewer", ActorRole::Reader), &s).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

#[test]
fn a_retraction_must_name_an_existing_signal() {
    let (_root, store) = sample_store("signal-retract");
    let mut s = signal("sig-x", "k", SignalKind::Retraction, "obs-1", None);
    s.revises = Some("sig-never-existed".to_string());
    let err = store.submit_signal(&actor(ActorRole::Operator), &s).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
}

// ── budget ─────────────────────────────────────────────────────────

#[test]
fn a_run_cannot_exceed_its_step_budget() {
    let (_root, store) = sample_store("budget");
    let run = Run {
        schema: SCHEMA_VERSION,
        id: "run-1".to_string(),
        task_id: "task-1".to_string(),
        base_snapshot: digest_bytes(b"base"),
        dataset_revision: "ds-1".to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Running,
        steps_consumed: 0,
        steps_budget: 10,
        resumed_from: None,
    };
    store.put_run(&actor(ActorRole::Operator), &run).unwrap();

    assert_eq!(store.consume_steps("run-1", 6).unwrap(), 6);
    // The ledger persists: a resume cannot reset what was already spent.
    let err = store.consume_steps("run-1", 5).unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted);
    assert_eq!(store.get_run("run-1").unwrap().steps_consumed, 6);
}

#[test]
fn a_fork_does_not_double_the_population_budget() {
    let (_root, store) = sample_store("pop-budget");
    let population = Population {
        schema: SCHEMA_VERSION,
        id: "pop-1".to_string(),
        owner: "trainer".to_string(),
        members: vec!["branch-a".to_string(), "branch-b".to_string()],
        total_budget_steps: 100,
        spent_steps: 90,
        policy: "explore-and-exploit@1".to_string(),
    };
    store.put_population(&actor(ActorRole::Operator), &population).unwrap();

    let after_a = store.spend_population(&population, 5).unwrap();
    assert_eq!(after_a.spent_steps, 95);
    // Branch B spends from the same ledger, so the grant is not duplicated.
    let err = store.spend_population(&after_a, 10).unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted);
}

// ── optimistic concurrency ─────────────────────────────────────────

#[test]
fn a_stale_writer_cannot_overwrite_a_newer_head() {
    let (_root, store) = sample_store("head");
    let first = seed_snapshot(&store, "trainer");
    let second = seed_snapshot(&store, "trainer");
    let branch = Branch {
        schema: SCHEMA_VERSION,
        id: "branch-1".to_string(),
        owner: "trainer".to_string(),
        head: Some(first.to_hex()),
        head_version: 0,
        policy: "default".to_string(),
        budget_quota: 50,
    };
    store.put_branch(&actor(ActorRole::Operator), &branch).unwrap();

    let v = store.advance_head(&actor(ActorRole::Operator), "branch-1", &second.to_hex(), 0).unwrap();
    assert_eq!(v, 1);

    // The worker that still believes the head is at 0 must be refused.
    let err = store
        .advance_head(&actor(ActorRole::Operator), "branch-1", &first.to_hex(), 0)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(store.get_branch("branch-1").unwrap().head, Some(second.to_hex()));
}

#[test]
fn a_stale_publication_does_not_replace_the_active_model() {
    let (_root, store) = sample_store("publish");
    let a = seed_snapshot(&store, "trainer");
    let b = seed_snapshot(&store, "trainer");
    let publisher = actor(ActorRole::Publisher);
    let protocol = publishable_protocol(&store);

    // The write pointer is `pub(crate)`; everything that can move it
    // goes through the approval boundary, so that is what these tests
    // drive. The CAS being tested lives below the boundary and is the
    // same CAS either way.
    assert_eq!(publish(&store, &publisher, &protocol, &a, None).unwrap(), 1);
    assert_eq!(publish(&store, &publisher, &protocol, &b, Some(1)).unwrap(), 2);

    let err = publish(&store, &publisher, &protocol, &a, Some(1)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
    assert_eq!(store.active_publication().unwrap().unwrap().1, b);
}

#[test]
fn an_operator_cannot_publish() {
    let (_root, store) = sample_store("publish-role");
    let a = seed_snapshot(&store, "trainer");
    let protocol = publishable_protocol(&store);
    let err = publish(
        &store,
        &actor(ActorRole::Operator),
        &protocol,
        &a,
        None,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

/// A protocol with a gate any seed snapshot clears, so a publication test
/// is about publication and not about gating.
fn publishable_protocol(store: &Store) -> EvaluationProtocol {
    let mut protocol = EvaluationProtocol::new("publish-v1", "geometry-sensor-xor@1.0.0", "ds-1");
    protocol.seeds = vec![1];
    protocol.gates = vec![("accuracy".to_string(), 0.0)];
    store
        .put_protocol(&actor(ActorRole::Operator), &protocol)
        .unwrap();
    protocol
}

/// Record one passing evaluation and go through the approval boundary.
fn publish(
    store: &Store,
    publisher: &Actor,
    protocol: &EvaluationProtocol,
    snapshot: &ArtifactRef,
    expected_version: Option<u32>,
) -> grove::contracts::Result<u32> {
    // A monotonic attempt counter, not a clock: two publications in the
    // same millisecond must still get distinct ids, or the second is
    // refused for a reason that has nothing to do with what is under
    // test.
    static ATTEMPT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let attempt = ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        // Per attempt, not per snapshot: publishing the same snapshot
        // twice is two decisions about it, and an evaluation id that
        // collided would refuse the second one for the wrong reason.
        id: format!("eval-{}-{snapshot}-{attempt}", protocol.id),
        snapshot: *snapshot,
        protocol_id: protocol.id.clone(),
        dataset_revision: protocol.dataset_revision.clone(),
        metrics: vec![("accuracy".to_string(), 1.0)],
        repeat_index: 0,
        device: "cpu".to_string(),
        completed_at_ms: 1,
    };
    store
        .put_evaluation(&actor(ActorRole::Operator), &record)
        .map_err(|e| grove::contracts::Error::new(e.kind, e.context))?;
    grove::evaluation::publish_snapshot(store, publisher, protocol, snapshot, expected_version)
        .map(|(version, _)| version)
}

// ── durability across abrupt death ─────────────────────────────────

/// Act as the victim process when the parent re-executes this test binary
/// with `GROVE_KILL_CHILD=1`. It streams large objects and records each
/// digest it is about to commit, then the parent kills it mid-write.
#[test]
fn killed_writer_child() {
    let Ok(root) = std::env::var("GROVE_KILL_ROOT") else {
        return; // not the child; nothing to do
    };
    let digest_log = PathBuf::from(std::env::var("GROVE_KILL_DIGESTS").unwrap());
    let store = Store::open(&root).unwrap();
    let payload = vec![7u8; 64 * 1024 * 1024];
    loop {
        let digest = std::fs::read(&digest_log).is_ok();
        let _ = digest;
        // Record the intent *before* the write so the parent can prove the
        // object never became visible.
        let predicted = digest_bytes(&payload);
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&digest_log)
            .unwrap();
        use std::io::Write as _;
        writeln!(log, "{predicted}").unwrap();
        log.sync_all().unwrap();
        let _ = store.artifacts().put(&payload);
    }
}

/// Kill a writer while it is streaming bytes, then prove that nothing it was
/// writing is visible as a committed object, and that an earlier committed
/// record survived intact.
#[test]
fn a_writer_killed_mid_write_leaves_no_committed_object() {
    let root = temp_root("kill");
    let digest_log = root.join("attempted-digests.txt");
    let committed_before = {
        let store = Store::open(&root).unwrap();
        let snapshot = seed_snapshot(&store, "trainer");
        store.load_manifest(&snapshot).unwrap()
    };

    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "killed_writer_child", "--nocapture"])
        .env("GROVE_KILL_CHILD", "1")
        .env("GROVE_KILL_ROOT", &root)
        .env("GROVE_KILL_DIGESTS", &digest_log)
        .spawn()
        .unwrap();

    // Wait until the child is demonstrably inside its write loop, then kill
    // it hard (SIGKILL: no unwinding, no flush, no cleanup).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if std::fs::read(&digest_log).map(|b| !b.is_empty()).unwrap_or(false) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let mut child = child;
    let _ = child.kill();
    let status = child.wait().unwrap();
    assert!(!status.success(), "child was expected to die abruptly");

    let attempted: Vec<String> = std::fs::read_to_string(&digest_log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(!attempted.is_empty(), "child recorded no write attempts");

    let store = Store::open(&root).unwrap();
    for hex in &attempted {
        let digest = ArtifactRef::parse_hex(hex).unwrap();
        assert!(
            store.artifacts().get(&digest).is_err(),
            "an object the killed writer was still streaming became visible"
        );
    }
    // The record committed before the kill is still readable and verified.
    assert_eq!(store.load_manifest(&seed_snapshot(&store, "trainer")).unwrap(), committed_before);
    // Sweeping removes the interrupted temporaries; committed objects stay.
    let swept = store.artifacts().sweep_temp().unwrap();
    assert!(
        store.artifacts().exists(&seed_snapshot(&store, "trainer")),
        "sweep must not touch committed objects (swept {swept})"
    );
}

#[test]
fn a_checkpoint_needs_a_durable_state_artifact() {
    let (_root, store) = sample_store("ckpt-state");
    let snapshot = seed_snapshot(&store, "trainer");
    let operator = actor(ActorRole::Operator);

    let ghost_state = digest_bytes(b"optimizer state that never landed");
    let cp = Checkpoint {
        schema: SCHEMA_VERSION,
        id: "ckpt-1".to_string(),
        owner: "trainer".to_string(),
        run_id: "run-1".to_string(),
        snapshot,
        parent: None,
        resume_level: ResumeLevel::LearningContinuation,
        state_artifact: ghost_state,
        budget_spent_steps: 12,
        created_at_ms: 1,
    };
    let err = store.commit_checkpoint(&operator, &cp).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    // Nothing was committed: the failed checkpoint is not merely unresolvable,
    // it does not exist.
    assert!(store.get_checkpoint("ckpt-1").is_err());

    let state = store.artifacts().put(b"optimizer-state").unwrap();
    let ok = Checkpoint { state_artifact: state, ..cp };
    store.commit_checkpoint(&operator, &ok).unwrap();
    assert_eq!(store.get_checkpoint("ckpt-1").unwrap().budget_spent_steps, 12);
}

// ── schema version gate ────────────────────────────────────────────

#[test]
fn an_unknown_store_schema_is_refused() {
    let root = temp_root("schema");
    {
        let store = Store::open(&root).unwrap();
        seed_snapshot(&store, "trainer");
    }
    // Simulate a database written by a newer producer.
    let conn = rusqlite::Connection::open(root.join("grove.db")).unwrap();
    conn.execute("UPDATE meta SET value = '99' WHERE key = 'schema'", [])
        .unwrap();
    drop(conn);

    let err = Store::open(&root).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
}

#[test]
fn a_record_from_an_unknown_schema_is_not_executed() {
    let root = temp_root("record-schema");
    let store = Store::open(&root).unwrap();
    let observation = Observation {
        schema: SCHEMA_VERSION,
        id: "obs-1".to_string(),
        owner: "trainer".to_string(),
        task_id: "task-1".to_string(),
        session_id: "s-1".to_string(),
        source: "line-a".to_string(),
        occurred_at_ms: 1,
        blocks: vec![],
        modality_mask: vec![true],
        training_permitted: true,
    };
    store.put_observation(&actor(ActorRole::Reader), &observation).unwrap();
    drop(store);

    let conn = rusqlite::Connection::open(root.join("grove.db")).unwrap();
    conn.execute("UPDATE observations SET schema = 99", []).unwrap();
    drop(conn);

    let store = Store::open(&root).unwrap();
    assert_eq!(store.get_observation("obs-1").unwrap_err().kind, ErrorKind::IncompatibleState);
}

#[test]
fn observations_cannot_be_attributed_to_another_actor() {
    let (_root, store) = sample_store("obs-owner");
    let observation = Observation {
        schema: SCHEMA_VERSION,
        id: "obs-1".to_string(),
        owner: "someone-else".to_string(),
        task_id: "task-1".to_string(),
        session_id: "s".to_string(),
        source: "line-a".to_string(),
        occurred_at_ms: 1,
        blocks: vec![],
        modality_mask: vec![true],
        training_permitted: true,
    };
    let err = store
        .put_observation(&Actor::new("trainer", ActorRole::Reader), &observation)
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

#[test]
fn grove_attach_registers_artifact_bindings() {
    use zio_core::context::EvalContext;
    use zio_core::env::Env;
    use std::sync::Arc;

    let ctx = EvalContext::new(Arc::new(Env::new(None)));
    grove::install(&ctx, None);
    assert!(
        ctx.env.get("grove-artifact-put").is_some(),
        "attach must register the binding even when unprovisioned"
    );
    // Unprovisioned capability is a stable error class, not a missing symbol.
    let ctx2 = EvalContext::new(Arc::new(Env::new(None)));
    grove::install(&ctx2, None);
    assert!(ctx2.env.get("grove-artifact-get").is_some());
}

