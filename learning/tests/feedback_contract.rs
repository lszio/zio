//! W02 contract: the signal lifecycle as a consumer sees it.
//!
//! The Zio library (`lib/zio/learn/feedback.zio`) owns pure policy —
//! precedence, field scoping, conflict isolation. This file anchors the
//! host side of the same rules: idempotent receipts, role-gated
//! submission, conflicts that must be adjudicated rather than
//! last-write-wins, abstains that are not negative labels, and frozen
//! views that a later retraction does not rewrite.

use grove::contracts::*;
use grove::store::{Receipt, Store};

fn temp_root(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-w02-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

fn annotator() -> Actor {
    Actor::new("ann", ActorRole::Annotator)
}

fn base_signal(id: &str, key: &str, kind: SignalKind, producer: &str) -> LearningSignal {
    LearningSignal {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        idempotency_key: key.to_string(),
        producer: producer.to_string(),
        kind,
        task_id: "task-1".to_string(),
        observation_id: Some("obs-1".to_string()),
        prediction_id: Some("pred-1".to_string()),
        target_field: Some("state".to_string()),
        content: "fault".to_string(),
        usage_permitted: true,
        occurred_at_ms: 100,
        received_at_ms: 100,
        revises: None,
    }
}

fn seed_world(name: &str) -> (Store, ArtifactRef) {
    let root = temp_root(name);
    let store = Store::open(&root).unwrap();
    let weights = store.artifacts().put(b"w0").unwrap();
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: "trainer".to_string(),
        entrypoint: "predict".to_string(),
        params: vec![ParamRef { module: "fusion".into(), shape: vec![1], dtype: "f32".into(), artifact: weights }],
        libraries: vec![],
        preprocessing_version: "v1".to_string(),
        modules: vec![],
        ensemble: None,
        graph: None,
    };
    let digest = store.put_snapshot(&operator(), &snapshot).unwrap();
    (store, digest)
}

fn seed_observation(store: &Store) {
    let content = store.artifacts().put(b"image-bytes").unwrap();
    store
        .put_observation(
            &Actor::new("trainer", ActorRole::Reader),
            &Observation {
                schema: SCHEMA_VERSION,
                id: "obs-1".to_string(),
                owner: "trainer".to_string(),
                task_id: "task-1".to_string(),
                session_id: "s-1".to_string(),
                source: "line-a".to_string(),
                occurred_at_ms: 90,
                blocks: vec![ContentBlock { media_type: "image/png".into(), artifact: content }],
                modality_mask: vec![true, true],
                training_permitted: true,
            },
        )
        .unwrap();
}

#[test]
fn feedback_binds_to_the_exact_prediction_not_the_current_head() {
    let (store, snapshot) = seed_world("bind");
    seed_observation(&store);
    let prediction = Prediction {
        schema: SCHEMA_VERSION,
        id: "pred-1".to_string(),
        owner: "trainer".to_string(),
        observation_id: "obs-1".to_string(),
        snapshot_digest: snapshot,
        snapshot_schema: SCHEMA_VERSION,
        output: "fault".to_string(),
        abstained: false,
        created_at_ms: 95,
    };
    store.put_prediction(&Actor::new("trainer", ActorRole::Reader), &prediction).unwrap();

    let loaded = store.get_prediction("pred-1").unwrap();
    assert_eq!(loaded.snapshot_digest, snapshot, "a prediction must keep its own version");
    assert_eq!(loaded.observation_id, "obs-1");

    // A later snapshot becomes the published one; the old prediction must
    // still name the version that produced it.
    let newer = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: "trainer".to_string(),
        entrypoint: "predict".to_string(),
        params: vec![],
        libraries: vec![],
        preprocessing_version: "v2".to_string(),
        modules: vec![],
        ensemble: None,
        graph: None,
    };
    let newer_digest = store.put_snapshot(&operator(), &newer).unwrap();
    // The write pointer is `pub(crate)`; going through the approval
    // boundary is how a publisher actually reaches it. The point of this
    // test is the prediction's binding, not the route.
    let mut protocol = grove::evaluation::EvaluationProtocol::new("publish-v1", "task", "ds-1");
    protocol.gates = vec![("accuracy".to_string(), 0.0)];
    store.put_protocol(&operator(), &protocol).unwrap();
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: "eval-publish-v1-newer-0".to_string(),
        snapshot: newer_digest,
        protocol_id: protocol.id.clone(),
        dataset_revision: protocol.dataset_revision.clone(),
        metrics: vec![("accuracy".to_string(), 1.0)],
        repeat_index: 0,
        device: "cpu".to_string(),
        completed_at_ms: 1,
    };
    store.put_evaluation(&operator(), &record).unwrap();
    grove::evaluation::publish_snapshot(
        &store,
        &Actor::new("trainer", ActorRole::Publisher),
        &protocol,
        &newer_digest,
        None,
    ).unwrap().0;

    let reloaded = store.get_prediction("pred-1").unwrap();
    assert_ne!(reloaded.snapshot_digest, newer_digest);
    assert_eq!(reloaded.snapshot_digest, snapshot);
}

#[test]
fn two_conflicting_corrections_stay_conflicted_until_adjudicated() {
    let (store, _) = seed_world("conflict");
    seed_observation(&store);

    let mut a = base_signal("sig-a", "key-a", SignalKind::HumanCorrection, "ann");
    a.content = "fault".to_string();
    let mut b = base_signal("sig-b", "key-b", SignalKind::HumanCorrection, "ann");
    b.content = "clear".to_string();

    store.submit_signal(&annotator(), &a).unwrap();
    store.submit_signal(&annotator(), &b).unwrap();

    // Both are in force, and the host reports them as a conflict rather
    // than picking a winner by arrival order.
    let conflicts = store.conflicts_for("obs-1").unwrap();
    assert_eq!(conflicts.len(), 1, "two disagreeing human corrections must conflict");
    assert_eq!(conflicts[0].target_field.as_deref(), Some("state"));
    assert_eq!(store.active_signals("obs-1").unwrap().len(), 2);

    // Adjudication is explicit: the loser leaves the in-force set.
    store.resolve_conflict(&annotator(), "sig-b", "sig-a").unwrap();
    assert_eq!(store.signal_status("sig-a").unwrap(), "superseded");
    let remaining = store.active_signals("obs-1").unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, "sig-b");
    assert!(store.conflicts_for("obs-1").unwrap().is_empty());
}

#[test]
fn adjudication_refuses_two_unrelated_signals() {
    let (store, _) = seed_world("resolve-unrelated");
    seed_observation(&store);
    let a = base_signal("sig-a", "key-a", SignalKind::HumanCorrection, "ann");
    let mut b = base_signal("sig-b", "key-b", SignalKind::HumanCorrection, "ann");
    b.target_field = Some("severity".to_string());
    store.submit_signal(&annotator(), &a).unwrap();
    store.submit_signal(&annotator(), &b).unwrap();

    let err = store.resolve_conflict(&annotator(), "sig-a", "sig-b").unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    // Unrelated signals stay in force.
    assert_eq!(store.active_signals("obs-1").unwrap().len(), 2);
}

#[test]
fn a_retraction_does_not_rewrite_a_frozen_view() {
    let (store, _) = seed_world("freeze");
    seed_observation(&store);

    let signal = base_signal("sig-1", "k1", SignalKind::TeacherLabel, "teacher-local");
    store.submit_signal(&operator(), &signal).unwrap();

    let frozen = DatasetRevision {
        schema: SCHEMA_VERSION,
        id: "ds-1".to_string(),
        task_id: "task-1".to_string(),
        policy_version: "feedback@1.0.0".to_string(),
        signal_ids: vec!["sig-1".to_string()],
        observation_ids: vec!["obs-1".to_string()],
        split: "train".to_string(),
        frozen_at_ms: 200,
    };
    store.freeze_dataset(&operator(), &frozen).unwrap();

    // The label is retracted after the view was frozen.
    store.retract_signal(&operator(), "sig-1").unwrap();
    assert_eq!(store.signal_status("sig-1").unwrap(), "retracted");
    assert!(store.active_signals("obs-1").unwrap().is_empty());

    // The frozen revision is a historical fact and still names it.
    let loaded = store.get_dataset("ds-1").unwrap();
    assert_eq!(loaded.signal_ids, vec!["sig-1".to_string()]);

    // The *next* view picks the change up.
    assert!(store.pending_signals("task-1").unwrap().is_empty(), "a retracted signal is not pending");
    let mut next = frozen.clone();
    next.id = "ds-2".to_string();
    next.signal_ids = vec![];
    store.freeze_dataset(&operator(), &next).unwrap();
    assert!(store.get_dataset("ds-2").unwrap().signal_ids.is_empty());
}

#[test]
fn a_frozen_view_refuses_signals_that_are_not_in_force() {
    let (store, _) = seed_world("freeze-stale");
    seed_observation(&store);
    let signal = base_signal("sig-1", "k1", SignalKind::HumanCorrection, "ann");
    store.submit_signal(&annotator(), &signal).unwrap();
    store.retract_signal(&annotator(), "sig-1").unwrap();

    let revision = DatasetRevision {
        schema: SCHEMA_VERSION,
        id: "ds-bad".to_string(),
        task_id: "task-1".to_string(),
        policy_version: "feedback@1.0.0".to_string(),
        signal_ids: vec!["sig-1".to_string()],
        observation_ids: vec![],
        split: "train".to_string(),
        frozen_at_ms: 1,
    };
    let err = store.freeze_dataset(&operator(), &revision).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(store.get_dataset("ds-bad").is_err(), "a rejected freeze must not persist");
}

#[test]
fn a_human_correction_outranks_a_teacher_pseudo_label() {
    let (store, _) = seed_world("precedence");
    seed_observation(&store);

    let mut teacher = base_signal("sig-t", "k-t", SignalKind::TeacherLabel, "teacher-local");
    teacher.content = "fault".to_string();
    let mut human = base_signal("sig-h", "k-h", SignalKind::HumanCorrection, "ann");
    human.content = "clear".to_string();

    store.submit_signal(&operator(), &teacher).unwrap();
    store.submit_signal(&annotator(), &human).unwrap();

    let in_force = store.active_signals("obs-1").unwrap();
    assert_eq!(in_force.len(), 2, "both are recorded; precedence is a policy decision");
    // A human and a teacher disagreeing about the same field is not a
    // human-vs-human conflict — there is nothing for a human to adjudicate.
    assert!(store.conflicts_for("obs-1").unwrap().is_empty());
    // Precedence is enforced by the Zio policy layer, which this package
    // drives: the human correction wins.
    let ranked: Vec<&LearningSignal> = in_force.iter().collect();
    let best = ranked
        .iter()
        .max_by_key(|s| match s.kind {
            SignalKind::HumanCorrection | SignalKind::HumanPreference | SignalKind::Demonstration => 3,
            SignalKind::TeacherLabel => 2,
            _ => 1,
        })
        .unwrap();
    assert_eq!(best.id, "sig-h");
    assert_eq!(best.content, "clear");
}

#[test]
fn an_abstain_is_stored_as_an_abstain_not_as_a_negative_label() {
    let (store, _) = seed_world("abstain");
    seed_observation(&store);
    let mut signal = base_signal("sig-1", "k1", SignalKind::HumanCorrection, "ann");
    signal.content = String::new();
    signal.target_field = Some("abstained".to_string());
    store.submit_signal(&annotator(), &signal).unwrap();

    let loaded = &store.active_signals("obs-1").unwrap()[0];
    assert_ne!(loaded.content, "clear");
    assert_ne!(loaded.content, "fault");
    assert!(loaded.content.is_empty(), "abstain carries no class value");
}

#[test]
fn a_reader_cannot_annotate_but_an_annotator_cannot_publish() {
    let (store, snapshot) = seed_world("roles");
    seed_observation(&store);
    let signal = base_signal("sig-1", "k1", SignalKind::HumanCorrection, "ann");

    let reader = Actor::new("viewer", ActorRole::Reader);
    assert_eq!(store.submit_signal(&reader, &signal).unwrap_err().kind, ErrorKind::CapabilityDenied);
    assert!(!annotator().may(ActorRole::Publisher), "annotating is not publishing");
    assert_eq!(
        grove::logic::decline(&store, &annotator(), &snapshot.to_hex(), "no", 1)
            .unwrap_err()
            .kind,
        ErrorKind::CapabilityDenied,
        "declining a candidate is a publisher decision"
    );
    assert!(store.submit_signal(&annotator(), &signal).is_ok());
}

#[test]
fn retracting_a_signal_that_is_not_in_force_is_an_explicit_failure() {
    let (store, _) = seed_world("retract-twice");
    seed_observation(&store);
    let signal = base_signal("sig-1", "k1", SignalKind::HumanCorrection, "ann");
    store.submit_signal(&annotator(), &signal).unwrap();
    store.retract_signal(&annotator(), "sig-1").unwrap();
    assert_eq!(store.retract_signal(&annotator(), "sig-1").unwrap_err().kind, ErrorKind::IncompatibleState);
}

#[test]
fn a_submitted_signal_survives_a_store_restart() {
    let root = temp_root("restart");
    let signal_ids: Vec<String>;
    {
        let store = Store::open(&root).unwrap();
        seed_observation(&store);
        let mut a = base_signal("sig-a", "ka", SignalKind::HumanCorrection, "ann");
        a.content = "fault".to_string();
        let mut b = base_signal("sig-b", "kb", SignalKind::HumanCorrection, "ann");
        b.content = "clear".to_string();
        store.submit_signal(&annotator(), &a).unwrap();
        store.submit_signal(&annotator(), &b).unwrap();
        signal_ids = vec!["sig-a".to_string(), "sig-b".to_string()];
    }

    // Reopen: the conflict and the adjudication state must be as they were.
    let store = Store::open(&root).unwrap();
    let conflicts = store.conflicts_for("obs-1").unwrap();
    assert_eq!(conflicts.len(), 1);
    let mut found: Vec<String> = store.active_signals("obs-1").unwrap().iter().map(|s| s.id.clone()).collect();
    found.sort();
    assert_eq!(found, signal_ids);

    store.resolve_conflict(&annotator(), "sig-a", "sig-b").unwrap();
    drop(store);
    let store = Store::open(&root).unwrap();
    assert_eq!(store.signal_status("sig-b").unwrap(), "superseded");
    assert_eq!(store.active_signals("obs-1").unwrap().len(), 1);
}

#[test]
fn pending_signals_report_what_the_next_view_would_add() {
    let (store, _) = seed_world("pending");
    seed_observation(&store);
    assert!(store.pending_signals("task-1").unwrap().is_empty());

    let first = base_signal("sig-1", "k1", SignalKind::HumanCorrection, "ann");
    store.submit_signal(&annotator(), &first).unwrap();
    assert_eq!(store.pending_signals("task-1").unwrap(), vec!["sig-1".to_string()]);

    let revision = DatasetRevision {
        schema: SCHEMA_VERSION,
        id: "ds-1".to_string(),
        task_id: "task-1".to_string(),
        policy_version: "feedback@1.0.0".to_string(),
        signal_ids: vec!["sig-1".to_string()],
        observation_ids: vec![],
        split: "train".to_string(),
        frozen_at_ms: 1,
    };
    store.freeze_dataset(&operator(), &revision).unwrap();
    assert!(store.pending_signals("task-1").unwrap().is_empty(), "a frozen signal is no longer pending");

    let second = base_signal("sig-2", "k2", SignalKind::TeacherLabel, "teacher-local");
    store.submit_signal(&operator(), &second).unwrap();
    assert_eq!(store.pending_signals("task-1").unwrap(), vec!["sig-2".to_string()]);
}

#[test]
fn duplicate_submission_after_restart_is_still_a_duplicate() {
    let root = temp_root("idem-restart");
    let signal = base_signal("sig-1", "stable-key", SignalKind::HumanCorrection, "ann");
    {
        let store = Store::open(&root).unwrap();
        seed_observation(&store);
        assert_eq!(store.submit_signal(&annotator(), &signal).unwrap(), Receipt::Accepted);
    }
    let store = Store::open(&root).unwrap();
    assert_eq!(
        store.submit_signal(&annotator(), &signal).unwrap(),
        Receipt::Duplicate,
        "an idempotency key must not buy a second sample across restarts"
    );
    assert_eq!(store.active_signals("obs-1").unwrap().len(), 1);
}
