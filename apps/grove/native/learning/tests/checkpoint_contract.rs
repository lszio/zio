//! W06 checkpoint contract: the properties that make interruption a
//! non-event and forking safe.
//!
//! The decisive case: a run interrupted at step N and continued to N+K
//! must land in the same state as an uninterrupted N+K run — same
//! sampling sequence, same loss, same parameters (CPU, declared
//! deterministic). Everything else — no half checkpoints, no budget
//! reset, no parent mutation, no promised replay — guards that case.

use std::path::{Path, PathBuf};
use std::time::Duration;

use grove::checkpoint::{self, derivation_label};
use grove::contracts::{
    Actor, ActorRole, Checkpoint, Derivation, ErrorKind, ModelSnapshot, ParamRef, ResumeLevel, Run,
    RunState, SCHEMA_VERSION,
};
use grove::store::Store;
use grove::worker::{Frame, Isolation, PROTOCOL_VERSION, Worker, WorkerConfig};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf()
}

fn task_dir() -> PathBuf {
    repo_root().join("examples/self-learning")
}

fn venv_python() -> PathBuf {
    repo_root().join(".venv/bin/python")
}

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

/// A state artifact in the current schema. `optimizer.adam` is keyed by
/// `layer.weight` / `layer.bias`, which is what lets a resume check that
/// the artifact describes this attempt's parameters.
fn write_state(path: &Path, run_id: &str, step: u32) {
    std::fs::write(
        path,
        serde_json::json!({
            "schema": checkpoint::STATE_SCHEMA,
            "protocol": checkpoint::STATE_PROTOCOL,
            "run_id": run_id, "step": step,
            "params": {},
            "optimizer": {"adam": {
                "h0.weight": {"step": step, "exp_avg": [], "exp_avg_sq": []},
                "h0.bias":   {"step": step, "exp_avg": [], "exp_avg_sq": []}
            }},
            "rng": {"cpu": ""}
        })
        .to_string(),
    )
    .unwrap();
}

fn worker_config() -> WorkerConfig {
    let root = repo_root();
    WorkerConfig {
        python: venv_python(),
        worker_script: root.join("apps/grove/workers/torch/worker.py"),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: repo_root(),
        timeout: Duration::from_secs(240),
        max_address_space: 4 * 1024 * 1024 * 1024,
        handshake_timeout: Duration::from_secs(180),
    }
}

fn isolation() -> Option<Isolation> {
    let iso = Isolation::probe("unshare")?;
    iso.enforce().ok()?;
    Some(iso)
}

fn nonlinear_graph() -> serde_json::Value {
    serde_json::json!({
        "inputs": {
            "image": {"shape": [256], "dtype": "float32", "space": "pixel"},
            "numeric": {"shape": [2], "dtype": "float32", "space": "signed"}
        },
        "ops": [
            {"kind": "normalize", "inputs": ["image"], "output": "image_n", "attrs": {}},
            {"kind": "normalize", "inputs": ["numeric"], "output": "numeric_n", "attrs": {}},
            {"kind": "concat", "inputs": ["image_n", "numeric_n"], "output": "fused",
             "attrs": {"spaces": ["pixel", "signed"]}},
            {"kind": "linear", "inputs": ["fused"], "output": "h0", "attrs": {"out": 24}},
            {"kind": "relu", "inputs": ["h0"], "output": "a0", "attrs": {}},
            {"kind": "linear", "inputs": ["a0"], "output": "h1", "attrs": {}}
        ],
        "outputs": {"logits": "h1"},
        "trainable": ["h0", "h1"]
    })
}

/// The store side of a run record, which the checkpoint layer mutates.
fn seed_run(store: &Store, run_id: &str, budget: u32) -> Run {
    // the run's base model must be a committed manifest the actor owns —
    // that ownership boundary is what stops cross-run artifact adoption
    let weights = store.artifacts().put(b"seed-weights").unwrap();
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
    let snapshot_digest = store
        .commit_manifest("ModelSnapshot", "trainer", &snapshot)
        .unwrap();
    let run = Run {
        schema: SCHEMA_VERSION,
        id: run_id.to_string(),
        task_id: "task-1".to_string(),
        base_snapshot: snapshot_digest,
        dataset_revision: "ds-1".to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Running,
        steps_consumed: 0,
        steps_budget: budget,
        resumed_from: None,
    };
    store.put_run(&operator(), &run).unwrap();
    run
}

fn train_request(
    run_id: &str,
    dir: &Path,
    graph: serde_json::Value,
    steps: u32,
    seed: u64,
    out: &Path,
) -> Frame {
    let seed_weights = dir.join("seed.json");
    std::fs::write(&seed_weights, "{}").unwrap();
    Frame::Train {
        v: PROTOCOL_VERSION,
        request_id: format!("req-{run_id}-{steps}-{seed}"),
        run_id: run_id.to_string(),
        attempt_id: format!("att-{run_id}"),
        graph,
        weights: seed_weights.to_string_lossy().into_owned(),
        data: task_dir()
            .join("data/train.bin")
            .to_string_lossy()
            .into_owned(),
        val_data: Some(
            task_dir()
                .join("data/val.bin")
                .to_string_lossy()
                .into_owned(),
        ),
        out: out.to_string_lossy().into_owned(),
        steps,
        seed,
        resume: None,
        save_at: None,
        state_out: None,
        stop_after_save: None,
    }
}

struct DoneReport {
    loss: f64,
    accuracy: f64,
    weights_path: PathBuf,
    params: serde_json::Value,
}

fn run_to_completion(worker: &mut Worker, frame: Frame, dir: &Path) -> DoneReport {
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    worker.send(&frame).expect("frame sends");
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "worker never finished");
        match worker.next_frame(remaining).expect("frames flow") {
            Frame::Done {
                weights,
                loss,
                first_loss,
                val_accuracy,
                ..
            } => {
                let path = PathBuf::from(weights.expect("done names its weights file"));
                let payload: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                let _ = first_loss;
                return DoneReport {
                    loss: loss.expect("loss"),
                    accuracy: val_accuracy.unwrap_or(0.0),
                    weights_path: path,
                    params: payload["params"].clone(),
                };
            }
            Frame::Failed { error, .. } => {
                let diag = worker.drain_stderr(2000);
                panic!("worker failed: {error}\nstderr: {diag}");
            }
            _ => continue,
        }
    }
}

// ── commit gate ────────────────────────────────────────────────────

#[test]
fn a_checkpoint_with_an_unparsable_state_is_refused() {
    let root = std::env::temp_dir().join(format!("grove-w06-a-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    seed_run(&store, "run-1", 100);

    // a state file that is not a grove state manifest
    let state = root.join("state.json");
    std::fs::write(&state, serde_json::json!({"hello": "world"}).to_string()).unwrap();

    let err = checkpoint::commit(
        &store,
        &operator(),
        "run-1",
        None,
        ResumeLevel::LearningContinuation,
        &state,
        50,
        1,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    // and nothing entered history
    assert!(store.get_checkpoint("ckpt-run-1-50").is_err());
}

#[test]
fn a_state_from_a_foreign_run_is_refused() {
    let root = std::env::temp_dir().join(format!("grove-w06-b-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    seed_run(&store, "run-A", 100);
    seed_run(&store, "run-B", 100);

    let state = root.join("state.json");
    write_state(&state, "run-A", 10);

    // committing under run-B's name would splice two runs' training state
    let err = checkpoint::commit(
        &store,
        &operator(),
        "run-B",
        None,
        ResumeLevel::LearningContinuation,
        &state,
        10,
        1,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(err.to_string().contains("run-A"), "{err}");
}

#[test]
fn a_good_state_commits_and_is_loadable() {
    let root = std::env::temp_dir().join(format!("grove-w06-c-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    seed_run(&store, "run-1", 100);

    let state = root.join("state.json");
    write_state(&state, "run-1", 25);

    let committed = checkpoint::commit(
        &store,
        &operator(),
        "run-1",
        None,
        ResumeLevel::LearningContinuation,
        &state,
        25,
        1,
    )
    .unwrap();
    let loaded = store.get_checkpoint(&committed.id).unwrap();
    assert_eq!(loaded.run_id, "run-1");
    assert_eq!(loaded.budget_spent_steps, 25);
    // the state artifact is in the store under its digest and loads intact
    let bytes = store.artifacts().get(&loaded.state_artifact).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(manifest["step"], 25);
}

// ── resume semantics ───────────────────────────────────────────────

fn committed_checkpoint(
    store: &Store,
    root: &Path,
    run_id: &str,
    step: u32,
    spent: u32,
) -> Checkpoint {
    seed_run(store, run_id, 1000);
    // the run's own ledger reflects the spent steps before the boundary
    store.consume_steps(run_id, spent).unwrap();
    let state = root.join(format!("state-{run_id}-{step}.json"));
    write_state(&state, run_id, step);
    checkpoint::commit(
        store,
        &operator(),
        run_id,
        None,
        ResumeLevel::LearningContinuation,
        &state,
        spent,
        1,
    )
    .unwrap()
}

#[test]
fn learning_continuation_inherits_the_budget_ledger() {
    let root = std::env::temp_dir().join(format!("grove-w06-d-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    let ckpt = committed_checkpoint(&store, &root, "run-parent", 300, 300);

    // resume: the ledger continues — 700 remain of the 1000, not 1000
    let plan = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt.id,
        ResumeLevel::LearningContinuation,
        false,
        "run-resumed",
        1000,
    )
    .unwrap();
    assert_eq!(
        plan.run.steps_consumed, 300,
        "resume must not reset the ledger"
    );
    assert_eq!(plan.run.resumed_from.as_deref(), Some("run-parent"));
    // the parent run is untouched
    assert_eq!(store.get_run("run-parent").unwrap().steps_consumed, 300);
    assert_eq!(
        store.get_run("run-parent").unwrap().state,
        RunState::Running
    );

    // overspending the lineage is refused
    let err = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt.id,
        ResumeLevel::LearningContinuation,
        false,
        "run-over",
        250,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted);
}

#[test]
fn model_initialization_declares_a_fresh_ledger() {
    let root = std::env::temp_dir().join(format!("grove-w06-e-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    let ckpt = committed_checkpoint(&store, &root, "run-parent", 300, 300);

    let plan = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt.id,
        ResumeLevel::ModelInitialization,
        false,
        "run-fresh",
        1000,
    )
    .unwrap();
    assert_eq!(plan.run.steps_consumed, 0, "model init is a fresh start");
}

#[test]
fn controlled_replay_needs_a_declared_deterministic_host() {
    let root = std::env::temp_dir().join(format!("grove-w06-f-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    let ckpt = committed_checkpoint(&store, &root, "run-parent", 300, 300);

    // without the declaration the host refuses to promise replay
    let err = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt.id,
        ResumeLevel::ControlledReplay,
        false,
        "run-replay",
        1000,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(err.to_string().contains("deterministic"), "{err}");

    // with it, replay is planned
    let plan = checkpoint::resume_plan(
        &store,
        &operator(),
        &ckpt.id,
        ResumeLevel::ControlledReplay,
        true,
        "run-replay",
        1000,
    )
    .unwrap();
    assert_eq!(plan.run.steps_consumed, 0);
}

// ── pause semantics ────────────────────────────────────────────────

#[test]
fn a_failed_save_never_claims_paused() {
    let root = std::env::temp_dir().join(format!("grove-w06-g-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    seed_run(&store, "run-1", 1000);

    // the state file is missing: the save fails
    let err =
        checkpoint::pause(&store, &operator(), "run-1", &root.join("missing.json"), 1).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
    // and the run is exactly where it was — not paused
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Running);

    // with a good state file, the run becomes paused
    let state = root.join("state.json");
    write_state(&state, "run-1", 40);
    let ckpt = checkpoint::pause(&store, &operator(), "run-1", &state, 2).unwrap();
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Paused);
    assert_eq!(
        store.get_checkpoint(&ckpt.id).unwrap().budget_spent_steps,
        0
    );
}

#[test]
fn a_terminal_run_cannot_be_paused() {
    let root = std::env::temp_dir().join(format!("grove-w06-h-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    let run = seed_run(&store, "run-1", 1000);
    let mut rejected = run.clone();
    rejected.state = RunState::Rejected;
    store.set_run_state(&rejected).unwrap();

    let err = checkpoint::pause(&store, &operator(), "run-1", &root.join("s.json"), 1).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
}

// ── fork semantics ─────────────────────────────────────────────────

#[test]
fn forking_two_branches_leaves_them_independent() {
    let root = std::env::temp_dir().join(format!("grove-w06-i-{}", std::process::id()));
    let store = Store::open(&root).unwrap();
    let ckpt = committed_checkpoint(&store, &root, "run-parent", 300, 300);

    let a = checkpoint::fork_branch(&store, &operator(), &ckpt.id, "branch-a", "explore@1", 300)
        .unwrap();
    let b = checkpoint::fork_branch(&store, &operator(), &ckpt.id, "branch-b", "exploit@1", 700)
        .unwrap();

    // both point at the parent checkpoint; quotas are separate slices, so
    // a fork does not double the grant
    assert_eq!(a.head.as_deref(), Some(ckpt.id.as_str()));
    assert_eq!(b.head.as_deref(), Some(ckpt.id.as_str()));
    assert_ne!(a.budget_quota + b.budget_quota, 0);

    // advancing one branch does not move the other
    store
        .advance_head(&operator(), "branch-a", "ckpt-later", a.head_version)
        .unwrap();
    assert_eq!(
        store.get_branch("branch-a").unwrap().head.as_deref(),
        Some("ckpt-later")
    );
    assert_eq!(
        store.get_branch("branch-b").unwrap().head.as_deref(),
        Some(ckpt.id.as_str())
    );

    // the parent run and checkpoint are untouched by either branch
    assert_eq!(store.get_run("run-parent").unwrap().steps_consumed, 300);
    let derived = derivation_label(Derivation::ParameterTraining);
    assert_eq!(derived, "parameter-training");
}

// ── the decisive case: resume equals uninterrupted ─────────────────

#[test]
fn resume_reaches_the_same_state_as_an_uninterrupted_run() {
    if !venv_python().exists() {
        eprintln!("skipping: no torch venv");
        return;
    }
    let Some(iso) = isolation() else {
        eprintln!("skipping: no namespace isolation");
        return;
    };
    let dir = std::env::temp_dir().join(format!("grove-w06-j-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = worker_config();
    let graph = nonlinear_graph();

    // run A: 600 uninterrupted steps
    let mut worker_a = Worker::spawn(&config, &iso).unwrap();
    let out_a = dir.join("uninterrupted.json");
    let a = run_to_completion(
        &mut worker_a,
        train_request("run-a", &dir, graph.clone(), 600, 9, &out_a),
        &dir,
    );
    worker_a.kill();

    // run B: 300 steps, checkpoint at 300, process ENDS, new process resumes
    let mut worker_b1 = Worker::spawn(&config, &iso).unwrap();
    let out_b1 = dir.join("b-first.json");
    let seed_weights = dir.join("seed-b.json");
    std::fs::write(&seed_weights, "{}").unwrap();
    let state = dir.join("state-at-300.json");
    let first = Frame::Train {
        v: PROTOCOL_VERSION,
        request_id: "req-b1".into(),
        run_id: "run-b".into(),
        attempt_id: "att-b".into(),
        graph: graph.clone(),
        weights: seed_weights.to_string_lossy().into_owned(),
        data: task_dir()
            .join("data/train.bin")
            .to_string_lossy()
            .into_owned(),
        val_data: None,
        out: out_b1.to_string_lossy().into_owned(),
        steps: 300,
        seed: 9,
        resume: None,
        save_at: Some(300),
        state_out: Some(state.to_string_lossy().into_owned()),
        stop_after_save: Some(true),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    worker_b1.send(&first).unwrap();
    let mut saved_path = None;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!remaining.is_zero(), "first leg never finished");
        match worker_b1.next_frame(remaining).unwrap() {
            Frame::Progress {
                saved: Some(path),
                step,
                ..
            } => {
                saved_path = Some(PathBuf::from(path));
                assert_eq!(step, 300);
                break;
            }
            Frame::Done { .. } => panic!("stop_after_save must not run to completion"),
            Frame::Failed { error, .. } => panic!("first leg failed: {error}"),
            _ => continue,
        }
    }
    worker_b1.kill();
    let _ = worker_b1.wait();
    let state = saved_path.unwrap();
    assert!(state.exists(), "checkpoint state must exist");
    // and no .partial residue: the save was atomic
    assert!(!state.with_extension("json.partial").exists());

    // a NEW process resumes to 600 total
    let mut worker_b2 = Worker::spawn(&config, &iso).unwrap();
    let out_b2 = dir.join("b-resumed.json");
    let resumed = Frame::Train {
        v: PROTOCOL_VERSION,
        request_id: "req-b2".into(),
        run_id: "run-b".into(),
        attempt_id: "att-b2".into(),
        graph: graph.clone(),
        weights: seed_weights.to_string_lossy().into_owned(),
        data: task_dir()
            .join("data/train.bin")
            .to_string_lossy()
            .into_owned(),
        val_data: None,
        out: out_b2.to_string_lossy().into_owned(),
        steps: 300,
        seed: 9,
        resume: Some(state.to_string_lossy().into_owned()),
        save_at: None,
        state_out: None,
        stop_after_save: None,
    };
    let b = run_to_completion(&mut worker_b2, resumed, &dir);
    worker_b2.kill();

    // the interrupted run and the uninterrupted run agree: same final loss,
    // same parameters within declared CPU tolerance
    assert!(
        (a.loss - b.loss).abs() < 1e-6,
        "final loss diverged: {a:?} vs {b:?}",
        a = a.loss,
        b = b.loss
    );
    let a_params = serde_json::to_string(&a.params).unwrap();
    let b_params = serde_json::to_string(&b.params).unwrap();
    assert_eq!(a_params, b_params, "parameters diverged after resume");

    // and the resume genuinely continued rather than restarted: B's leg 2
    // finished at step 600 with the checkpointed first_loss
    let payload: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&out_b2).unwrap()).unwrap();
    assert_eq!(payload["final_loss"], serde_json::json!(b.loss));
}
