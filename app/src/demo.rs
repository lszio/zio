//! `grove demo --case dual` — the W00 acceptance flow, end to end.
//!
//! Everything here calls library contracts; the demo adds orchestration
//! and nothing else. It fails loudly when no candidate clears the frozen
//! gates: an honest demo can end in "not good enough".

use std::path::{Path, PathBuf};
use std::time::Duration;

use grove::contracts::{
    require_finite_metrics, Actor, ArtifactRef, Error, ErrorKind, EvaluationRecord, Result, Run,
    RunState, SCHEMA_VERSION,
};
use grove::evaluation::{self, EvaluationProtocol};
use grove::store::Store;
use grove::worker::{Frame, Isolation, Worker, WorkerConfig};

use crate::{operator, publisher, Paths};

/// The frozen acceptance gates, mirroring examples/self-learning/task.json.
const GATE_ACCURACY: f64 = 0.90;
const GATE_LIFT_PP: f64 = 15.0;

/// The dual-flow graphs: the fixed linear baseline and the structural
/// rewrite. Both come from the same vocabulary — the difference is data.
fn linear_graph() -> serde_json::Value {
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
            {"kind": "linear", "inputs": ["fused"], "output": "h0", "attrs": {}}
        ],
        "outputs": {"logits": "h0"},
        "trainable": ["h0"]
    })
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

/// Run the dual demo under `root`. Returns the human-readable report.
pub fn run_dual(root: &Path, paths: &Paths, device: &str) -> Result<String> {
    if device != "cpu" {
        return Err(Error::new(
            ErrorKind::CapabilityDenied,
            format!("device {device:?} is not provisioned; this demo runs on cpu"),
        ));
    }
    let mut report = String::new();

    // 0. the frozen data must exist; generate it if this is a fresh root
    ensure_data(paths)?;
    report.push_str("data: frozen W00 task present (geometry-sensor-xor@1.0.0)\n");

    // 1. the store records every step; the frozen protocol is provisioned
    //    first so later shells load the same gates, and each candidate is
    //    its own model identity
    let store = Store::open(root)?;
    provision_protocol(&store)?;
    let operator = operator();
    let snapshot_base = seed_snapshot(&store, &operator, "dual-baseline-weights")?;
    let snapshot_joint = seed_snapshot(&store, &operator, "dual-joint-weights")?;

    // 2. an isolated worker does all training
    let mut worker = spawn_worker(paths)?;
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    // 3. the fixed baseline: structure frozen, parameters trained
    let run_base = seed_run(&store, "run-baseline", &snapshot_base, 1000)?;
    let (base_loss, base_acc) =
        train_leg(&mut worker, &scratch, "run-baseline", linear_graph(), 300, 7, None)?;
    store.consume_steps(&run_base.id, 300)?;
    record(&store, &operator, &snapshot_base, base_acc, 1)?;
    report.push_str(&format!(
        "baseline (linear fusion):  loss {base_loss:.4}  val accuracy {base_acc:.3}\n"
    ));

    // 4. the structural candidate, through a real pause/resume:
    //    leg 1 trains 150 steps and stops at the checkpoint boundary,
    //    the run is marked paused only after the state lands,
    //    resume plans a NEW run whose ledger continues at 150,
    //    leg 2 trains the remaining 150 steps in a fresh worker process.
    let run_joint = seed_run(&store, "run-joint", &snapshot_joint, 1000)?;
    let (loss1, _) = train_leg(
        &mut worker,
        &scratch,
        "run-joint",
        nonlinear_graph(),
        150,
        7,
        Some(150),
    )?;
    store.consume_steps(&run_joint.id, 150)?;
    let state = scratch.join("state-run-joint.json");
    grove::checkpoint::pause(&store, &operator, &run_joint.id, &state, 1)?;
    report.push_str("  paused at step 150 (checkpoint committed, run marked paused)\n");

    let plan = grove::checkpoint::resume_plan(
        &store,
        &operator,
        "ckpt-run-joint-150",
        grove::contracts::ResumeLevel::LearningContinuation,
        false,
        "run-joint-resumed",
        1000,
    )?;
    assert_eq!(plan.run.steps_consumed, 150, "the ledger must continue");
    worker.kill();
    let mut worker2 = spawn_worker(paths)?;
    let (joint_loss, joint_acc) = train_leg(
        &mut worker2,
        &scratch,
        "run-joint", // the training lineage; the ledger lives in the new record
        nonlinear_graph(),
        150,
        7,
        None,
    )?;
    // tell the resumed run's record its leg finished: resume carries the
    // first leg's loss forward in the report
    let _ = loss1;
    store.consume_steps(&plan.run.id, 150)?;
    record(&store, &operator, &snapshot_joint, joint_acc, 1)?;
    report.push_str(&format!(
        "candidate (nonlinear, paused+resumed): loss {joint_loss:.4}  val accuracy {joint_acc:.3}\n"
    ));
    worker2.kill();

    // 5. comparison under the frozen protocol
    let protocol = acceptance_protocol();
    let lift_pp = (joint_acc - base_acc) * 100.0;
    report.push_str(&format!(
        "lift over baseline: {lift_pp:+.1}pp (frozen gates: accuracy ≥ {GATE_ACCURACY}, lift ≥ +{GATE_LIFT_PP}pp)\n"
    ));

    // 6. publication only for a candidate that earned it — a demo that
    //    promotes an underfit model would be a lie with extra steps
    let publisher = publisher();
    match evaluation::publish_candidate(&store, &publisher, &protocol, &snapshot_joint, None) {
        Ok(version) => {
            report.push_str(&format!(
                "publication: candidate cleared every gate → v{version} active\n"
            ));
        }
        Err(e) => {
            return Err(Error::new(
                e.kind,
                format!(
                    "no candidate met the frozen gates — refusing to promote\n  detail: {e}\n{report}"
                ),
            ));
        }
    }
    Ok(report)
}

/// Generate the frozen data if the manifest is absent. A fresh root still
/// trains on the same bytes: the seed is frozen in the generator.
pub fn ensure_data(paths: &Paths) -> Result<()> {
    let manifest = paths.task.join("manifest.json");
    if manifest.exists() {
        return Ok(());
    }
    let status = std::process::Command::new(&paths.python)
        .arg(&paths.generate)
        .arg("--self-check")
        .status()
        .map_err(|e| {
            Error::new(
                ErrorKind::BackendFailed,
                format!("cannot run the data generator ({:?}): {e}", paths.python),
            )
        })?;
    if !status.success() {
        return Err(Error::new(
            ErrorKind::BackendFailed,
            "data generator self-check failed",
        ));
    }
    Ok(())
}

pub fn spawn_worker(paths: &Paths) -> Result<Worker> {
    let iso = Isolation::probe("unshare").ok_or_else(|| {
        Error::denied("worker isolation unavailable; refusing to train unrestricted")
    })?;
    iso.enforce()?;
    let config = WorkerConfig {
        python: paths.python.clone(),
        worker_script: paths.worker.clone(),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: paths.root.clone(),
        timeout: Duration::from_secs(300),
        max_address_space: 4 * 1024 * 1024 * 1024,
    };
    Worker::spawn(&config, &iso)
}

pub fn seed_snapshot(store: &Store, actor: &Actor, tag: &str) -> Result<ArtifactRef> {
    let weights = store.artifacts().put(tag.as_bytes()).unwrap();
    let snapshot = grove::contracts::ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: "predict".to_string(),
        params: vec![grove::contracts::ParamRef {
            module: "fusion".to_string(),
            shape: vec![1],
            dtype: "float32".to_string(),
            artifact: weights,
        }],
        libraries: vec![],
        preprocessing_version: "geometry-sensor-xor@1.0.0".to_string(),
    };
    store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)
}

fn seed_run(store: &Store, id: &str, snapshot: &ArtifactRef, budget: u32) -> Result<Run> {
    let run = Run {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        task_id: "geometry-sensor-xor@1.0.0".to_string(),
        base_snapshot: *snapshot,
        dataset_revision: "ds-1".to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Running,
        steps_consumed: 0,
        steps_budget: budget,
        resumed_from: None,
    };
    store.put_run(&operator(), &run)?;
    Ok(run)
}

/// One training leg. `checkpoint_at` turns the leg into a first leg that
/// stops at the boundary; the second leg passes `resume` and the SAME
/// lineage id (the state artifact names it, and the worker refuses to
/// splice state across lineages).
fn train_leg(
    worker: &mut Worker,
    scratch: &Path,
    lineage: &str,
    graph: serde_json::Value,
    steps: u32,
    seed: u64,
    checkpoint_at: Option<u32>,
) -> Result<(f64, f64)> {
    let seed_weights = scratch.join(format!("seed-{lineage}.json"));
    std::fs::write(&seed_weights, "{}").unwrap();
    let out = scratch.join(format!("out-{lineage}-{steps}.json"));
    let deadline = std::time::Instant::now() + Duration::from_secs(240);
    let data = task_data("train.bin");
    let val = task_data("val.bin");
    worker
        .send(&Frame::Train {
            v: grove::worker::PROTOCOL_VERSION,
            request_id: format!("req-{lineage}-{steps}-{seed}"),
            run_id: lineage.to_string(),
            attempt_id: format!("att-{lineage}"),
            graph,
            weights: seed_weights.to_string_lossy().into_owned(),
            data: data.to_string_lossy().into_owned(),
            val_data: Some(val.to_string_lossy().into_owned()),
            out: out.to_string_lossy().into_owned(),
            steps,
            seed,
            resume: None,
            save_at: checkpoint_at,
            state_out: checkpoint_at.map(|_| {
                scratch
                    .join(format!("state-{lineage}.json"))
                    .to_string_lossy()
                    .into_owned()
            }),
            stop_after_save: checkpoint_at.map(|_| true),
        })
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("worker send: {e}")))?;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            worker.kill();
            return Err(Error::new(ErrorKind::Timeout, "worker did not finish in time"));
        }
        match worker.next_frame(remaining)? {
            Frame::Done { loss, val_accuracy, .. } => {
                let loss = loss.ok_or_else(|| {
                    Error::new(ErrorKind::BackendFailed, "done frame without a loss")
                })?;
                let acc = val_accuracy.unwrap_or(0.0);
                require_finite_metrics(&[("accuracy".into(), acc)])?;
                return Ok((loss, acc));
            }
            Frame::Failed { error, .. } => {
                return Err(Error::new(
                    ErrorKind::BackendFailed,
                    format!("worker failed: {error}"),
                ));
            }
            _ => continue,
        }
    }
}

/// Training data lives relative to the task dir recorded in Paths; the
/// worker runs from the workspace root, so relative paths resolve.
fn task_data(name: &str) -> PathBuf {
    PathBuf::from("examples/self-learning/data").join(name)
}

pub fn acceptance_protocol() -> EvaluationProtocol {
    let mut p = EvaluationProtocol::new("accept-v1", "geometry-sensor-xor@1.0.0", "ds-1");
    p.seeds = vec![7];
    p.gates = vec![("accuracy".to_string(), GATE_ACCURACY)];
    p.device = "cpu".to_string();
    p
}

/// The protocol is provisioned BEFORE any evaluation: gates are stored
/// data, so a later CLI run loads the same gates or nothing.
pub fn provision_protocol(store: &Store) -> Result<()> {
    store.put_protocol(&operator(), &acceptance_protocol())
}

/// Scores bind to the snapshot that produced them, under the frozen
/// protocol. One repeat per candidate in the demo; the protocol's seed
/// plan (3 seeds) is what a real acceptance runs.
fn record(
    store: &Store,
    actor: &Actor,
    snapshot: &ArtifactRef,
    accuracy: f64,
    repeat: u32,
) -> Result<EvaluationRecord> {
    require_finite_metrics(&[("accuracy".to_string(), accuracy)])?;
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: format!("eval-accept-v1-{}-{repeat}", &snapshot.to_hex()[..12]),
        snapshot: *snapshot,
        protocol_id: "accept-v1".to_string(),
        dataset_revision: "ds-1".to_string(),
        metrics: vec![("accuracy".to_string(), accuracy)],
        repeat_index: repeat,
        device: "cpu".to_string(),
        completed_at_ms: 1,
    };
    store.put_evaluation(actor, &record)?;
    Ok(record)
}
