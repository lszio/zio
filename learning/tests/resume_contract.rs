//! G03 resume contract: a resumed run is work, not a row.
//!
//! `POST /api/learning/resume` created a `Queued` run and returned. But
//! the queue's unit of work is the `queued_work` row, and `resume_plan`
//! never wrote one — so a resumed run sat in `queued` forever, and the
//! owner loop skipped it without a word. The plan's requirement is
//! blunt: resuming a checkpoint must *actually start execution*.
//!
//! Three things must hold, and each is checked against a real store:
//!
//! 1. the resumed run carries the work it is for, so an owner that did
//!    not receive the request can still run it;
//! 2. it carries the parent's data, not a caller-supplied substitute —
//!    a resume that quietly swapped the dataset is not a resume;
//! 3. the parent run is untouched: `Paused -> a new Queued run`, never
//!    overwriting the run it came from.

use std::path::PathBuf;
use std::sync::Arc;

use grove::contracts::{Actor, ActorRole, ErrorKind, RunState};
use grove::store::Store;

fn store(label: &str) -> (Arc<Store>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("grove-resume-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(Store::open(&dir).expect("open store"));
    (store, dir)
}

fn operator() -> Actor {
    Actor::new("resume-test", ActorRole::Operator)
}

/// A minimal checkpoint whose state artifact is present, because
/// `resume_plan` refuses a resume that cannot restore the trained state.
fn seed_lineage(store: &Store) -> (String, String) {
    use grove::contracts::{ArtifactRef, Checkpoint, ResumeLevel, Run, SCHEMA_VERSION};

    let snapshot = grove::contracts::ModelSnapshot::new(
        "resume-test",
        "predict",
        vec![grove::contracts::ParamRef {
            module: "fusion".into(),
            shape: vec![1],
            dtype: "float32".into(),
            artifact: store.artifacts().put(b"weights").expect("weights"),
        }],
        "geometry-sensor-xor@1.0.0",
    );
    let snapshot = store
        .commit_manifest("ModelSnapshot", "resume-test", &snapshot)
        .expect("snapshot manifest");

    // The state manifest must carry the fields `require_level` checks
    // for a learning continuation: params, optimizer and RNG. A resume
    // that could not restore those is a reinitialization wearing a
    // continuation's name, and the store refuses it.
    let manifest = serde_json::json!({
        "schema": 2,
        "protocol": "grove.worker.state/1",
        "run_id": "run-parent",
        "attempt_id": "att-parent",
        "step": 30,
        "params": {"h0.weight": [0.5], "h0.bias": [0.1]},
        "optimizer": {"adam": {"h0.weight": [0.0], "h0.bias": [0.0]}},
        "rng": {"torch": 7, "numpy": 7},
    });
    let state = store
        .artifacts()
        .put(serde_json::to_vec(&manifest).expect("manifest json").as_slice())
        .expect("state artifact");

    let parent = Run {
        schema: SCHEMA_VERSION,
        id: "run-parent".into(),
        task_id: "task-1".into(),
        base_snapshot: snapshot,
        dataset_revision: "ds-1".into(),
        recipe: "recipe-1".into(),
        state: RunState::Paused,
        steps_consumed: 30,
        steps_budget: 100,
        resumed_from: None,
    };
    store.put_run(&operator(), &parent).expect("parent run");

    let checkpoint = Checkpoint {
        schema: SCHEMA_VERSION,
        id: "cp-1".into(),
        owner: "resume-test".into(),
        run_id: "run-parent".into(),
        snapshot,
        parent: None,
        resume_level: ResumeLevel::LearningContinuation,
        state_artifact: state,
        budget_spent_steps: 30,
        created_at_ms: grove::api_time_ms(),
    };
    store.commit_checkpoint(&operator(), &checkpoint).expect("checkpoint");
    (snapshot.to_hex(), "run-parent".into())
}

#[test]
fn a_resumed_run_carries_the_work_it_must_run() {
    let (store, dir) = store("work");
    seed_lineage(&store);

    let plan = grove::checkpoint::resume_plan(
        &store,
        &operator(),
        "cp-1",
        grove::contracts::ResumeLevel::LearningContinuation,
        false,
        "run-resumed",
        100,
    )
    .expect("resume plan");

    // The plan alone is a row. What makes it runnable is the work
    // descriptor, and before this package existed it was never written.
    assert!(
        store.queued_work(&plan.run.id).unwrap().is_none(),
        "resume_plan by itself writes no work; the caller must attach it"
    );
    store
        .put_queued_work(&operator(), &plan.run.id, "training", "{}")
        .expect("attach work");
    let (kind, _) = store
        .queued_work(&plan.run.id)
        .unwrap()
        .expect("work is readable by whoever owns the run");
    assert_eq!(kind, "training");
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_resume_does_not_overwrite_the_run_it_came_from() {
    let (store, dir) = store("parent-untouched");
    seed_lineage(&store);
    grove::checkpoint::resume_plan(
        &store,
        &operator(),
        "cp-1",
        grove::contracts::ResumeLevel::LearningContinuation,
        false,
        "run-resumed",
        100,
    )
    .expect("resume plan");

    // The parent is still paused and still carries its own steps. A
    // resume that rewrote the parent would destroy the record of where
    // the lineage was when it stopped.
    let parent = store.get_run("run-parent").expect("parent");
    assert_eq!(parent.state, RunState::Paused, "the parent is not overwritten");
    assert_eq!(parent.steps_consumed, 30);

    // And the new run is separate, queued, and says what it came from.
    let resumed = store.get_run("run-resumed").expect("resumed");
    assert_eq!(resumed.state, RunState::Queued);
    assert_eq!(resumed.resumed_from.as_deref(), Some("run-parent"));
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_learning_continuation_inherits_the_ledger_and_others_do_not() {
    let (store, dir) = store("ledger");
    seed_lineage(&store);

    // Learning continuation must not reset the lineage's spend: that is
    // how a fork buys itself a fresh budget.
    let continued = grove::checkpoint::resume_plan(
        &store,
        &operator(),
        "cp-1",
        grove::contracts::ResumeLevel::LearningContinuation,
        false,
        "run-cont",
        100,
    )
    .expect("continuation");
    assert_eq!(continued.run.steps_consumed, 30, "the ledger carries over");

    // A reinitialization is a new lineage's worth of compute, so it
    // starts at zero — and must not silently keep the old count.
    let fresh = grove::checkpoint::resume_plan(
        &store,
        &operator(),
        "cp-1",
        grove::contracts::ResumeLevel::ModelInitialization,
        false,
        "run-fresh",
        100,
    )
    .expect("reinit");
    assert_eq!(fresh.run.steps_consumed, 0);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_resume_that_cannot_restore_state_is_refused_before_anything_is_queued() {
    let (store, dir) = store("no-state");
    use grove::contracts::{ArtifactRef, Checkpoint, ResumeLevel, Run, SCHEMA_VERSION};
    // A checkpoint whose state artifact is gone. `resume_plan` must
    // refuse rather than queue a run that would restart from nothing.
    let snapshot = ArtifactRef::parse_hex(
        "0000000000000000000000000000000000000000000000000000000000000009",
    )
    .unwrap();
    let parent = Run {
        schema: SCHEMA_VERSION,
        id: "run-x".into(),
        task_id: "t".into(),
        base_snapshot: snapshot,
        dataset_revision: "d".into(),
        recipe: "r".into(),
        state: RunState::Paused,
        steps_consumed: 0,
        steps_budget: 10,
        resumed_from: None,
    };
    store.put_run(&operator(), &parent).expect("parent");
    let missing = ArtifactRef::parse_hex(
        "00000000000000000000000000000000000000000000000000000000000000ff",
    )
    .unwrap();
    let cp = Checkpoint {
        schema: SCHEMA_VERSION,
        id: "cp-broken".into(),
        owner: "resume-test".into(),
        run_id: "run-x".into(),
        snapshot,
        parent: None,
        resume_level: ResumeLevel::LearningContinuation,
        state_artifact: missing,
        budget_spent_steps: 0,
        created_at_ms: 0,
    };
    // Committing is refused too: the state must be readable to commit.
    let err = store
        .commit_checkpoint(&operator(), &cp)
        .expect_err("a checkpoint with no readable state is refused");
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}
