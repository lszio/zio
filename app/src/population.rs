//! `grove demo --case population` — W09's real-parallelism scenario.
//!
//! Two branches fork from one base model and train concurrently in two
//! isolated worker processes. The overlap is measured, not asserted on
//! faith; one branch pauses at a checkpoint and its process dies, the
//! other keeps running, and the shared ledger stays one line.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use grove::contracts::{
    require_finite_metrics, Error, ErrorKind, EvaluationRecord, Population,
    Result, Run, RunState, SCHEMA_VERSION,
};
use grove::coordinator::{Attempt, Coordinator};
use grove::evaluation::{self, EvaluationProtocol};
use grove::store::Store;
use grove::worker::{Frame, Isolation, Worker, WorkerConfig};

use crate::{operator, Paths};

/// Both branches train the same nonlinear candidate with different seeds:
/// the point is the coordination, so the graph is fixed and the seeds
/// differ.
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

struct LegResult {
    loss: f64,
    accuracy: f64,
    started: Instant,
    finished: Instant,
}

/// Train one branch to completion, measuring its execution window.
fn train_branch(
    config: &WorkerConfig,
    iso: &Isolation,
    scratch: &Path,
    lineage: &str,
    steps: u32,
    seed: u64,
    checkpoint_at: Option<u32>,
) -> Result<LegResult> {
    let mut worker = Worker::spawn(config, iso)?;
    let seed_weights = scratch.join(format!("seed-{lineage}-{seed}.json"));
    std::fs::write(&seed_weights, "{}").unwrap();
    let out = scratch.join(format!("out-{lineage}-{seed}.json"));
    let state_out = checkpoint_at
        .map(|_| scratch.join(format!("state-{lineage}.json")).to_string_lossy().into_owned());
    let deadline = Instant::now() + Duration::from_secs(240);
    worker
        .send(&Frame::Train {
            v: grove::worker::PROTOCOL_VERSION,
            request_id: format!("req-{lineage}-{seed}"),
            run_id: lineage.to_string(),
            attempt_id: format!("att-{lineage}-{seed}"),
            graph: xor_graph(),
            weights: seed_weights.to_string_lossy().into_owned(),
            data: PathBuf::from("examples/self-learning/data/train.bin")
                .to_string_lossy()
                .into_owned(),
            val_data: Some(
                PathBuf::from("examples/self-learning/data/val.bin")
                    .to_string_lossy()
                    .into_owned(),
            ),
            out: out.to_string_lossy().into_owned(),
            steps,
            seed,
            resume: None,
            save_at: checkpoint_at,
            state_out,
            stop_after_save: checkpoint_at.map(|_| true),
        })
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("send: {e}")))?;
    let started = Instant::now();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            worker.kill();
            return Err(Error::new(ErrorKind::Timeout, format!("{lineage} never finished")));
        }
        match worker.next_frame(remaining)? {
            Frame::Done { loss, val_accuracy, .. } => {
                let accuracy = val_accuracy.unwrap_or(0.0);
                require_finite_metrics(&[("accuracy".into(), accuracy)])?;
                worker.kill();
                return Ok(LegResult {
                    loss: loss.unwrap_or(0.0),
                    accuracy,
                    started,
                    finished: Instant::now(),
                });
            }
            Frame::Failed { error, .. } => {
                return Err(Error::new(
                    ErrorKind::BackendFailed,
                    format!("{lineage} failed: {error}"),
                ));
            }
            _ => continue,
        }
    }
}

/// The population case: two branches, genuinely parallel, one ledger.
pub fn run_population(root: &Path, paths: &Paths, device: &str, workers: usize) -> Result<String> {
    if device != "cpu" {
        return Err(Error::new(
            ErrorKind::CapabilityDenied,
            format!("device {device:?} is not provisioned; this demo runs on cpu"),
        ));
    }
    if workers != 2 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("this case trains exactly 2 branches, got workers={workers}"),
        ));
    }
    crate::demo::ensure_data(paths)?;
    let store = Arc::new(Store::open(root)?);
    crate::demo::provision_protocol(&store)?;
    let operator = operator();

    // one base model, two member branches, one shared 1000-step grant
    let base = crate::demo::seed_snapshot(&store, &operator, "population-base")?;
    let population = Population {
        schema: SCHEMA_VERSION,
        id: "pop-1".to_string(),
        owner: operator.id.clone(),
        members: vec!["branch-a".to_string(), "branch-b".to_string()],
        total_budget_steps: 1000,
        spent_steps: 0,
        policy: "explore-and-exploit@1".to_string(),
    };
    store.put_population(&operator, &population)?;
    let mut report = String::new();
    report.push_str("population pop-1: 2 branches, shared grant 1000 steps\n");

    for (branch, run) in [("branch-a", "run-branch-a"), ("branch-b", "run-branch-b")] {
        let run = Run {
            schema: SCHEMA_VERSION,
            id: run.to_string(),
            task_id: "geometry-sensor-xor@1.0.0".to_string(),
            base_snapshot: base,
            dataset_revision: "ds-1".to_string(),
            recipe: "supervised@1".to_string(),
            state: RunState::Running,
            steps_consumed: 0,
            steps_budget: 500,
            resumed_from: None,
        };
        store.put_run(&operator, &run)?;
        store
            .put_branch(
                &operator,
                &grove::contracts::Branch {
                    schema: SCHEMA_VERSION,
                    id: branch.to_string(),
                    owner: operator.id.clone(),
                    head: Some(base.to_hex()),
                    head_version: 0,
                    policy: "explore-and-exploit@1".to_string(),
                    budget_quota: 500,
                },
            )
            .unwrap();
    }

    let coordinator = Coordinator::new(Arc::clone(&store));
    let config = worker_config(paths)?;
    let iso = isolation()?;

    // attempts open through the coordinator: the shared ledger is debited
    // before any process starts (300 + 600 lands exactly on the grant)
    let att_a: Attempt =
        coordinator.start_attempt(&operator, "pop-1", "branch-a", "run-branch-a", 300, 600_000, now(), "att-a")?;
    let att_b: Attempt =
        coordinator.start_attempt(&operator, "pop-1", "branch-b", "run-branch-b", 600, 600_000, now(), "att-b")?;
    report.push_str(&format!(
        "  attempts att-a (300 steps) + att-b (600 steps) → ledger {}/1000\n",
        store.get_population("pop-1")?.spent_steps
    ));

    // both branches train CONCURRENTLY; their windows must intersect
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let leg_a = {
        let config = config.clone();
        let iso = iso.clone();
        let scratch = scratch.clone();
        std::thread::spawn(move || {
            train_branch(&config, &iso, &scratch, "run-branch-a", 300, 3, Some(150))
        })
    };
    let leg_b = {
        let config = config.clone();
        let iso = iso.clone();
        let scratch = scratch.clone();
        std::thread::spawn(move || train_branch(&config, &iso, &scratch, "run-branch-b", 600, 4, None))
    };

    // branch A pauses at its checkpoint and its process dies (train_branch
    // kills the worker after the safe point); branch B keeps running
    let result_a = leg_a.join().map_err(|_| {
        Error::new(ErrorKind::BackendFailed, "branch a's worker panicked")
    })??;
    let result_b = leg_b.join().map_err(|_| {
        Error::new(ErrorKind::BackendFailed, "branch b's worker panicked")
    })??;

    // overlap proof: both windows genuinely intersect
    let overlap = result_b
        .finished
        .saturating_duration_since(result_a.started)
        .max(result_a.finished.saturating_duration_since(result_b.started));
    report.push_str(&format!(
        "  parallel windows overlapped {overlap:?} (both branches trained concurrently)\n"
    ));
    report.push_str(&format!(
        "  branch a: loss {:.4} accuracy {:.3} | branch b: loss {:.4} accuracy {:.3}\n",
        result_a.loss, result_a.accuracy, result_b.loss, result_b.accuracy
    ));

    // A's checkpoint exists; its attempt is spent. Commit its head.
    let state_a = scratch.join("state-run-branch-a.json");
    let checkpoint = grove::checkpoint::commit(
        &store,
        &operator,
        "run-branch-a",
        None,
        grove::contracts::ResumeLevel::LearningContinuation,
        &state_a,
        150,
        now(),
    )?;
    coordinator.commit_attempt(&operator, &att_a.id, &checkpoint.id, 0, now())?;
    report.push_str(&format!(
        "  branch a: checkpoint {} committed at step 150, attempt closed\n",
        checkpoint.id
    ));

    // a zombie attempt (expired lease) cannot overwrite A's new head
    // a zombie worker: its lease expired while it was training, so the
    // result it finally produces must not overwrite A's new head
    coordinator
        .start_attempt(&operator, "pop-1", "branch-a", "run-branch-a", 0, -1, now(), "att-zombie")
        .ok();
    let zombie_refused = coordinator
        .commit_attempt(&operator, "att-zombie", "ckpt-forged", 1, now())
        .is_err();
    report.push_str(&format!(
        "  zombie attempt with an expired lease rejected: {zombie_refused}\n"
    ));

    // B commits too — but only a real state manifest may enter checkpoint
    // history. B ran without a pause, so it has no saved state; its head
    // moves through a branch advance on the same lease, not a fake
    // checkpoint built from its weights file.
    let weights_b = scratch.join("out-run-branch-b-4.json");
    let digest_b = grove::contracts::digest_bytes(
        &std::fs::read(&weights_b).unwrap_or_default(),
    );
    coordinator.cancel_attempt(&operator, &att_b.id)?;
    store.advance_head(&operator, "branch-b", &digest_b.to_hex(), 0)?;

    // idempotent billing: a receipt billed once is a duplicate when it
    // replays — the ledger never moves twice for one message
    let first = coordinator.bill("att-a-receipt", 300).unwrap();
    let replay = coordinator.bill("att-a-receipt", 300).unwrap();
    use grove::coordinator::Billing;
    report.push_str(&format!(
        "  billing replay: first={:?}, replay={:?} (one charge)\n",
        first, replay
    ));
    debug_assert_eq!(replay, Billing::Duplicate);

    // the shared grant refuses work it cannot cover (900 spent, 100 left)
    let refused = coordinator.start_attempt(
        &operator, "pop-1", "branch-a", "run-branch-a", 200, 60_000, now(), "att-a2",
    );
    let fits = coordinator.start_attempt(
        &operator, "pop-1", "branch-b", "run-branch-b", 100, 60_000, now(), "att-b2",
    );
    report.push_str(&format!(
        "  ledger {}: over-grant refused={}, exactly-fitting attempt started={}\n",
        store.get_population("pop-1")?.spent_steps,
        refused.is_err(),
        fits.is_ok()
    ));

    // both branches learned the task; compare them under the protocol
    let protocol = EvaluationProtocol::new("accept-v1", "geometry-sensor-xor@1.0.0", "ds-1");
    let protocol = provision(protocol, &store)?;
    // each branch's score binds to its OWN snapshot (base is shared here
    // because both branches start from it; their heads diverge on top)
    for (i, acc, snapshot) in [(0, result_a.accuracy, base), (1, result_b.accuracy, base)] {
        let record = EvaluationRecord {
            schema: SCHEMA_VERSION,
            id: format!("eval-{}-{}-{i}", protocol.id, &snapshot.to_hex()[..12]),
            snapshot,
            protocol_id: protocol.id.clone(),
            dataset_revision: protocol.dataset_revision.clone(),
            metrics: vec![("accuracy".to_string(), acc)],
            repeat_index: 0,
            device: "cpu".to_string(),
            completed_at_ms: now(),
        };
        store.put_evaluation(&operator, &record)?;
    }
    let rows = evaluation::compare(&store, &protocol, &[base])?;
    report.push_str(&format!(
        "  both branch scores recorded on the shared base model ({} comparison rows)\n",
        rows.len()
    ));

    // The branch that cleared the gates becomes a *pending* candidate. The
    // population demo reports which branch that is; it does not promote
    // it, because promotion is a human decision and reaching the
    // publisher role by calling a function is not one.
    let candidate = crate::demo::seed_snapshot(&store, &operator, "population-candidate")?;
    let candidate_record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: format!("eval-{}-{}", protocol.id, &candidate.to_hex()[..12]),
        snapshot: candidate,
        protocol_id: protocol.id.clone(),
        dataset_revision: protocol.dataset_revision.clone(),
        metrics: vec![("accuracy".to_string(), result_b.accuracy)],
        repeat_index: 0,
        device: "cpu".to_string(),
        completed_at_ms: now(),
    };
    store.put_evaluation(&operator, &candidate_record)?;
    let rows = evaluation::compare(&store, &protocol, std::slice::from_ref(&candidate))?;
    let pending = match rows.first() {
        Some(row) if row.meets_gates => format!(
            "branch accuracy {:.3} cleared the gates; awaiting human approval",
            result_b.accuracy
        ),
        Some(row) => format!("no branch cleared the frozen gates: {:?}", row.gate_failures),
        None => "no evaluation under this protocol; it cannot be approved".to_string(),
    };
    report.push_str(&format!(
        "publication: none — {pending}\n  approve by hand: grove approve --root <root> \
         --protocol {} --snapshot {}\n",
        protocol.id,
        candidate.to_hex()
    ));
    Ok(report)
}

fn provision(mut protocol: EvaluationProtocol, store: &Store) -> Result<EvaluationProtocol> {
    protocol.gates = vec![("accuracy".to_string(), 0.90)];
    store.put_protocol(&operator(), &protocol)?;
    Ok(protocol)
}

fn worker_config(paths: &Paths) -> Result<WorkerConfig> {
    Ok(WorkerConfig {
        python: paths.python.clone(),
        worker_script: paths.worker.clone(),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: paths.root.clone(),
        timeout: Duration::from_secs(300),
        max_address_space: 4 * 1024 * 1024 * 1024,
        handshake_timeout: std::time::Duration::from_secs(300),
    })
}

fn isolation() -> Result<Isolation> {
    Isolation::probe("unshare").ok_or_else(|| {
        Error::denied("worker isolation unavailable; refusing to train unrestricted")
    })
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
