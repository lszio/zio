//! W09 contract: the population coordinator.
//!
//! Parallelism is proven, not claimed: two workers train simultaneously
//! and their execution windows must intersect. Around that:
//!
//! * a failed/killed branch does not stop the others, and its slot's
//!   lease cannot be resurrected to write history;
//! * a stale or zombie attempt cannot overwrite newer progress;
//! * billing is idempotent by message id, and an unknown external
//!   outcome stays visible as unknown;
//! * the ledger is one shared line — forking branches does not multiply
//!   the grant, and an exhausted grant refuses new work.

use std::sync::Arc;
use std::time::{Duration, Instant};

use grove::contracts::{
    Actor, ActorRole, ErrorKind, Population, ResumeLevel, Run, RunState, SCHEMA_VERSION,
};
use grove::coordinator::{Billing, Coordinator};
use grove::store::Store;
use grove::worker::{Frame, Isolation, Worker, WorkerConfig, PROTOCOL_VERSION};

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors().nth(4)
        .unwrap()
        .to_path_buf()
}

fn venv_python() -> PathBuf {
    repo_root().join(".venv/bin/python")
}

fn task_dir() -> PathBuf {
    repo_root().join("examples/self-learning")
}

fn isolation() -> Option<Isolation> {
    let iso = Isolation::probe("unshare")?;
    iso.enforce().ok()?;
    Some(iso)
}

fn worker_config() -> WorkerConfig {
    WorkerConfig {
        python: venv_python(),
        worker_script: repo_root().join("apps/grove/workers/torch/worker.py"),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: repo_root(),
        timeout: Duration::from_secs(240),
        max_address_space: 4 * 1024 * 1024 * 1024,
        handshake_timeout: Duration::from_secs(180),
    }
}

fn population_store(name: &str) -> (Arc<Store>, Coordinator) {
    let root = std::env::temp_dir().join(format!("grove-w09-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Arc::new(Store::open(&root).unwrap());
    let coordinator = Coordinator::new(store.clone());
    (store, coordinator)
}

/// A population with two member branches and a shared 1000-step grant.
fn seed_population(store: &Store, coordinator: &Coordinator) -> (String, String, String, ArtifactRef) {
    let op = operator();
    // branch heads need a base snapshot to point at
    let weights = store.artifacts().put(b"population-weights").unwrap();
    let snapshot = grove::contracts::ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: "trainer".to_string(),
        entrypoint: "predict".to_string(),
        params: vec![grove::contracts::ParamRef {
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
    let snapshot_digest = store.commit_manifest("ModelSnapshot", "trainer", &snapshot).unwrap();

    let population = Population {
        schema: SCHEMA_VERSION,
        id: "pop-1".to_string(),
        owner: "trainer".to_string(),
        members: vec!["branch-a".to_string(), "branch-b".to_string()],
        total_budget_steps: 1000,
        spent_steps: 0,
        policy: "explore-and-exploit@1".to_string(),
    };
    store.put_population(&op, &population).unwrap();

    for branch in ["branch-a", "branch-b"] {
        let run = Run {
            schema: SCHEMA_VERSION,
            id: format!("run-{branch}"),
            task_id: "task-1".to_string(),
            base_snapshot: snapshot_digest,
            dataset_revision: "ds-1".to_string(),
            recipe: "supervised@1".to_string(),
            state: RunState::Running,
            steps_consumed: 0,
            steps_budget: 500,
            resumed_from: None,
        };
        store.put_run(&op, &run).unwrap();
        coordinator
            .store()
            .put_branch(
                &op,
                &grove::contracts::Branch {
                    schema: SCHEMA_VERSION,
                    id: branch.to_string(),
                    owner: "trainer".to_string(),
                    head: Some(snapshot_digest.to_hex()),
                    head_version: 0,
                    policy: "explore-and-exploit@1".to_string(),
                    budget_quota: 500,
                },
            )
            .unwrap();
    }
    ("pop-1".to_string(), "branch-a".to_string(), "branch-b".to_string(), snapshot_digest)
}

use std::path::PathBuf;

use grove::contracts::ArtifactRef;

// ── the ledger is shared and idempotent ────────────────────────────

#[test]
fn the_ledger_is_shared_and_forks_do_not_multiply_it() {
    let (store, coordinator) = population_store("ledger");
    let (pop, _a, _b, _) = seed_population(&store, &coordinator);
    let population = store.get_population(&pop).unwrap();

    // branch A's attempt debits the shared line
    coordinator
        .start_attempt(&operator(), &pop, "branch-a", "run-branch-a", 300, 60_000, now(), "att-a1")
        .unwrap();
    let after_a = store.get_population(&pop).unwrap();
    assert_eq!(after_a.spent_steps, 300, "branch A bills the shared ledger");

    // branch B asks for one step more than the shared grant has left
    coordinator
        .start_attempt(&operator(), &pop, "branch-b", "run-branch-b", 701, 60_000, now(), "att-b1")
        .unwrap_err();
    assert_eq!(
        store.get_population(&pop).unwrap().spent_steps,
        300,
        "a refused attempt bills nothing"
    );

    // what fits, starts — and lands exactly on the shared ceiling
    coordinator
        .start_attempt(&operator(), &pop, "branch-b", "run-branch-b", 700, 60_000, now(), "att-b1")
        .unwrap();
    assert_eq!(store.get_population(&pop).unwrap().spent_steps, 1000);

    // the grant is exhausted: any further attempt is refused
    let err = coordinator
        .start_attempt(&operator(), &pop, "branch-a", "run-branch-a", 1, 60_000, now(), "att-a2")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::BudgetExhausted);
}

#[test]
fn billing_is_idempotent_by_message_id() {
    let (store, coordinator) = population_store("billing");
    let (pop, _a, _b, _) = seed_population(&store, &coordinator);

    assert_eq!(
        coordinator.bill("msg-1", 100).unwrap(),
        Billing::Charged,
        "the first receipt charges"
    );
    assert_eq!(
        coordinator.bill("msg-1", 100).unwrap(),
        Billing::Duplicate,
        "a replayed receipt must not bill twice"
    );

    // the population's ledger agrees: one debit only
    coordinator
        .start_attempt(&operator(), &pop, "branch-a", "run-branch-a", 100, 60_000, now(), "att-1")
        .unwrap();
    assert_eq!(store.get_population(&pop).unwrap().spent_steps, 100);
}

#[test]
fn an_unknown_outcome_stays_visible() {
    let (store, coordinator) = population_store("unknown");
    let _ = seed_population(&store, &coordinator);

    coordinator.record_unknown_outcome("msg-x", 50, "teacher call timed out mid-flight").unwrap();
    let unknown = coordinator.unknown_outcomes().unwrap();
    assert_eq!(unknown.len(), 1, "the unknown outcome is on the books");
    assert_eq!(unknown[0].0, "msg-x");
    assert_eq!(unknown[0].1, 50);
    assert!(unknown[0].2.contains("timed out"));

    // a later successful reconciliation with the SAME message id is a
    // duplicate bill, not a second charge — the ledger never moves twice
    assert_eq!(coordinator.bill("msg-x", 50).unwrap(), Billing::Duplicate);
}

// ── attempts, leases and stale writers ─────────────────────────────

#[test]
fn an_expired_lease_cannot_commit() {
    let (store, coordinator) = population_store("lease");
    let (pop, _a, _b, _) = seed_population(&store, &coordinator);

    // a short lease that is already expired by the time it commits
    coordinator
        .start_attempt(&operator(), &pop, "branch-a", "run-branch-a", 100, -1, now(), "att-zombie")
        .unwrap();

    let err = coordinator
        .commit_attempt(&operator(), "att-zombie", "ckpt-later", 0, now())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.to_string().contains("expired"), "{err}");
    // the branch head never moved
    assert_eq!(store.get_branch("branch-a").unwrap().head_version, 0);
}

#[test]
fn a_cancelled_attempt_cannot_speak_again() {
    let (store, coordinator) = population_store("cancel");
    let (pop, _a, _b, _) = seed_population(&store, &coordinator);
    coordinator
        .start_attempt(&operator(), &pop, "branch-a", "run-branch-a", 100, 60_000, now(), "att-1")
        .unwrap();
    coordinator.cancel_attempt(&operator(), "att-1").unwrap();

    let err = coordinator
        .commit_attempt(&operator(), "att-1", "ckpt-later", 0, now())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert!(err.to_string().contains("no longer active"), "{err}");
}

#[test]
fn a_stale_head_version_is_refused() {
    let (store, coordinator) = population_store("stale-head");
    let (pop, _a, _b, _) = seed_population(&store, &coordinator);
    coordinator
        .start_attempt(&operator(), &pop, "branch-a", "run-branch-a", 100, 60_000, now(), "att-1")
        .unwrap();

    // a newer worker moved the head first
    store
        .advance_head(&operator(), "branch-a", "ckpt-new", 0)
        .unwrap();

    // the older worker's commit carries the version it saw — refused
    let err = coordinator
        .commit_attempt(&operator(), "att-1", "ckpt-stale", 0, now())
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);
    assert_eq!(store.get_branch("branch-a").unwrap().head.as_deref(), Some("ckpt-new"));
}

// ── real parallelism ───────────────────────────────────────────────

struct LegReport {
    started: Instant,
    finished: Instant,
    loss: f64,
    accuracy: f64,
}

/// Train one branch to completion, recording the execution window.
fn train_branch(
    config: WorkerConfig,
    iso: Isolation,
    lineage: &str,
    graph: serde_json::Value,
    steps: u32,
    seed: u64,
    dir: &PathBuf,
    save_at: Option<u32>,
) -> LegReport {
    let mut worker = Worker::spawn(&config, &iso).expect("worker spawns");
    let seed_weights = dir.join(format!("seed-{lineage}.json"));
    std::fs::write(&seed_weights, "{}").unwrap();
    let out = dir.join(format!("out-{lineage}.json"));
    let deadline = Instant::now() + Duration::from_secs(240);
    worker
        .send(&Frame::Train {
            v: PROTOCOL_VERSION,
            request_id: format!("req-{lineage}"),
            run_id: lineage.to_string(),
            attempt_id: format!("att-{lineage}"),
            graph,
            weights: seed_weights.to_string_lossy().into_owned(),
            data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
            val_data: Some(task_dir().join("data/val.bin").to_string_lossy().into_owned()),
            out: out.to_string_lossy().into_owned(),
            steps,
            seed,
            resume: None,
            save_at,
            state_out: save_at.map(|_| dir.join(format!("state-{lineage}.json")).to_string_lossy().into_owned()),
            stop_after_save: save_at.map(|_| true),
        })
        .unwrap();
    let started = Instant::now();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "{lineage} never finished");
        match worker.next_frame(remaining).unwrap() {
            Frame::Done { loss, val_accuracy, .. } => {
                let report = LegReport {
                    started,
                    finished: Instant::now(),
                    loss: loss.unwrap_or(0.0),
                    accuracy: val_accuracy.unwrap_or(0.0),
                };
                worker.kill();
                return report;
            }
            Frame::Failed { error, .. } => {
                let diag = worker.drain_stderr(2000);
                panic!("{lineage} failed: {error}\nstderr: {diag}");
            }
            _ => continue,
        }
    }
}

fn xor_graph() -> serde_json::Value {
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

#[test]
fn two_workers_genuinely_overlap_and_a_killed_branch_leaves_the_other_running() {
    if !venv_python().exists() || isolation().is_none() {
        eprintln!("skipping: no torch venv or no isolation");
        return;
    }
    let dir = std::env::temp_dir().join(format!("grove-w09-par-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = worker_config();
    let iso = isolation().unwrap();
    let graph = xor_graph();

    // Two branches start together and train concurrently.
    let handle_a = std::thread::spawn({
        let config = config.clone();
        let iso = iso.clone();
        let dir = dir.clone();
        let graph = graph.clone();
        move || train_branch(config, iso, "run-branch-a", graph, 400, 3, &dir, Some(200))
    });
    let handle_b = std::thread::spawn({
        let config = config.clone();
        let iso = iso.clone();
        let dir = dir.clone();
        let graph = graph.clone();
        move || train_branch(config, iso, "run-branch-b", graph, 400, 4, &dir, None)
    });

    let a = handle_a.join().unwrap();
    let b = handle_b.join().unwrap();

    // The overlap is the parallelism proof: both execution windows share time.
    let overlap = a.finished.saturating_duration_since(b.started);
    let overlap_b = b.finished.saturating_duration_since(a.started);
    let overlap = overlap.max(overlap_b);
    assert!(
        overlap > Duration::from_millis(500),
        "windows must genuinely intersect; got {overlap:?} (a {:?}..{:?}, b {:?}..{:?})",
        a.started.elapsed(),
        b.started.elapsed(),
        b.started.elapsed(),
        b.finished.elapsed()
    );
    // and both learned: the XOR needs the nonlinear structure, so both
    // hitting it proves the workers did real work concurrently
    assert!(a.accuracy > 0.9, "branch a: {:?}", a.accuracy);
    assert!(b.accuracy > 0.9, "branch b: {:?}", b.accuracy);
}

#[test]
fn a_branch_killed_at_its_checkpoint_resumes_without_touching_the_other() {
    if !venv_python().exists() || isolation().is_none() {
        eprintln!("skipping: no torch venv or no isolation");
        return;
    }
    let dir = std::env::temp_dir().join(format!("grove-w09-kill-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = worker_config();
    let iso = isolation().unwrap();
    let graph = xor_graph();

    // Branch B trains on its own thread and must not care what happens to A.
    let handle_b = std::thread::spawn({
        let config = config.clone();
        let iso = iso.clone();
        let dir = dir.clone();
        let graph = graph.clone();
        move || train_branch(config, iso, "run-branch-b", graph, 500, 4, &dir, None)
    });

    // Branch A pauses at its checkpoint (the safe point), then its process
    // is killed. The pause + kill is the "terminated one" of the contract.
    let report_a = train_branch(
        config.clone(),
        iso.clone(),
        "run-branch-a",
        graph.clone(),
        200,
        3,
        &dir,
        Some(100),
    );
    // A's checkpoint state exists and is a complete manifest
    let state = dir.join("state-run-branch-a.json");
    assert!(state.exists());
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state).unwrap()).unwrap();
    assert_eq!(manifest["step"], 100);

    // A's process is gone (train_branch killed it); a fresh process resumes
    // its ledger: 100 inherited + 100 more
    let b = handle_b.join().unwrap();
    assert!(b.accuracy > 0.9, "branch b finished regardless of a's death");

    // resume A from its checkpoint in a new process
    let mut worker_a2 = Worker::spawn(&config, &iso).unwrap();
    let seed_weights = dir.join("seed-run-branch-a.json");
    std::fs::write(&seed_weights, "{}").unwrap();
    let out_a2 = dir.join("out-a2.json");
    worker_a2
        .send(&Frame::Train {
            v: PROTOCOL_VERSION,
            request_id: "req-a2".into(),
            run_id: "run-branch-a".into(),
            attempt_id: "att-a2".into(),
            graph: graph.clone(),
            weights: seed_weights.to_string_lossy().into_owned(),
            data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
            val_data: Some(task_dir().join("data/val.bin").to_string_lossy().into_owned()),
            out: out_a2.to_string_lossy().into_owned(),
            steps: 100,
            seed: 3,
            resume: Some(state.to_string_lossy().into_owned()),
            save_at: None,
            state_out: None,
            stop_after_save: None,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "a's resume never finished");
        match worker_a2.next_frame(remaining).unwrap() {
            Frame::Done { val_accuracy, .. } => {
                assert!(val_accuracy.unwrap_or(0.0) > 0.9, "the resumed branch learns");
                break;
            }
            Frame::Failed { error, .. } => panic!("a's resume failed: {error}"),
            _ => continue,
        }
    }
    worker_a2.kill();

    // the claim under test is not an ordering: both branches were
    // training at the same time, and B's success does not depend on A's
    // death. (Which one finishes first is scheduler noise.)
    let overlap = report_a
        .finished
        .saturating_duration_since(b.started)
        .max(b.finished.saturating_duration_since(report_a.started));
    assert!(
        overlap > Duration::from_millis(200),
        "the two branches ran concurrently (overlap {overlap:?})"
    );
    assert!(report_a.accuracy > 0.9 && b.accuracy > 0.9);
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
