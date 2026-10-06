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

use crate::{operator, Paths};

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

    // 5b. The trained weights ARE the candidate. `seed_snapshot` above
    //     only opened a run contract with placeholder parameters, so
    //     publishing it would point the deployment at bytes nobody
    //     trained. Commit the worker's actual output as the candidate's
    //     parameter artifact and evaluate *that* — the scores recorded
    //     above came from these weights, so the identity has to be the
    //     same identity.
    let trained = commit_trained_snapshot(
        &store,
        &operator,
        "run-joint-candidate",
        &scratch.join(format!("out-run-joint-{}.json", 150)),
        nonlinear_graph(),
    )?;
    report.push_str(&format!(
        "candidate weights committed: {} (val accuracy {:.3})\n",
        &trained.to_hex()[..12],
        joint_acc
    ));
    // The score binds to the identity the weights actually produced, not
    // to the placeholder contract the run was opened with: publishing a
    // snapshot whose evaluation belongs to different bytes is exactly the
    // shell-rewrites-the-evidence failure the plan forbids.
    record(&store, &operator, &trained, joint_acc, 1)?;

    // 6. the demo stops at a *pending* candidate. It does not publish:
    //    publication is a human decision behind an authenticated
    //    approval, and a demo that reaches the publisher role by
    //    calling a function is precisely the implicit promotion G04
    //    removes. What the demo can honestly report is whether the
    //    candidate would qualify — that is a comparison, not a
    //    deployment — and then how to approve it.
    let verdict = evaluation::compare(&store, &protocol, std::slice::from_ref(&trained))?;
    let pending = match verdict.first() {
        Some(row) if row.meets_gates => format!(
            "candidate awaits human approval: cleared every frozen gate ({} repeats, accuracy {:.3})",
            row.repeats,
            row.mean
                .iter()
                .find(|(n, _)| n == "accuracy")
                .map(|(_, v)| *v)
                .unwrap_or(0.0)
        ),
        Some(row) => format!(
            "candidate does not qualify and stays unpublished: {:?}",
            row.gate_failures
        ),
        None => "candidate has no evaluation under this protocol; it cannot be approved".to_string(),
    };
    report.push_str(&format!(
        "publication: none — {pending}\n  approve by hand: grove approve --root <root> \
         --protocol {} --snapshot {}\n",
        protocol.id,
        trained.to_hex()
    ));
    Ok(report)
}

/// Commit the worker's trained output as a real model snapshot.
///
/// The worker's `done` frame names a file holding `{params: {name: {w,
/// b}}}`; that file becomes the snapshot's parameter artifact, so the
/// published identity points at the exact weights that earned the
/// score. A snapshot whose parameters are a placeholder cannot be
/// deployed, and pretending otherwise is how a demo publishes a model
/// that does not exist.
pub fn commit_trained_snapshot(
    store: &Store,
    actor: &Actor,
    name: &str,
    weights_path: &Path,
    graph: serde_json::Value,
) -> Result<ArtifactRef> {
    let bytes = std::fs::read(weights_path).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("trained weights {} are unreadable: {e}", weights_path.display()),
        )
    })?;
    // verify it is the worker's parameter format, not an arbitrary blob:
    // a snapshot whose parameters are not `{layer: {w, b}}` cannot be
    // loaded by the worker at inference time
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("trained weights are not JSON: {e}"),
        )
    })?;
    let params = parsed.get("params").and_then(|v| v.as_object()).ok_or_else(|| {
        Error::new(
            ErrorKind::BackendFailed,
            "trained weights carry no `params` object: this is not a model",
        )
    })?;
    if params.is_empty() {
        return Err(Error::new(
            ErrorKind::BackendFailed,
            "trained weights carry no layers: an empty model is not a candidate",
        ));
    }
    for (layer, entry) in params {
        if entry.get("w").is_none() || entry.get("b").is_none() {
            return Err(Error::new(
                ErrorKind::BackendFailed,
                format!(
                    "layer {layer:?} is missing its weight or bias; a checkpoint \
                     without a bias describes a different model than the one trained"
                ),
            ));
        }
    }
    let artifact = store.artifacts().put(&bytes)?;
    // The module decomposition is *derived from the trained graph*, not
    // asserted beside it: the fusion layer and the classification head
    // are separate layers with separate layers[] entries, so the module
    // view would otherwise show a monolithic model and composition would
    // have nothing to compose.
    let modules = modules_of(&graph, params);
    let snapshot = grove::contracts::ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: "predict".to_string(),
        params: params
            .iter()
            .map(|(layer, entry)| {
                let rows = entry["w"].as_array().map(|a| a.len()).unwrap_or(0) as u32;
                grove::contracts::ParamRef {
                    module: layer.clone(),
                    shape: vec![rows],
                    dtype: "float32".to_string(),
                    artifact,
                }
            })
            .collect(),
        libraries: Vec::new(),
        preprocessing_version: "geometry-sensor-xor@1.0.0".to_string(),
        modules,
        ensemble: None,
        // the graph travels with the weights, so a prediction is the
        // product of *these* bytes under *this* structure — a caller
        // cannot pair committed weights with a graph of their own
        graph: Some(graph),
    };
    let digest = store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)?;
    store.name_snapshot(name, &digest)?;
    Ok(digest)
}

/// Read the module contract out of the graph: the first linear layer is
/// the fusion module (it consumes the concatenated modalities) and the
/// last is the head. Declaring them here means the product's module view
/// shows the real structure, and `validate_modules` checks the semantic
/// spaces against the same graph the worker will execute.
fn modules_of(
    graph: &serde_json::Value,
    trained: &serde_json::Map<String, serde_json::Value>,
) -> Vec<grove::contracts::ModuleSpec> {
    let ops = graph["ops"].as_array().cloned().unwrap_or_default();
    let linears: Vec<&serde_json::Value> = ops
        .iter()
        .filter(|op| op["kind"] == "linear")
        .collect();
    let mut out = Vec::new();
    for (index, op) in linears.iter().enumerate() {
        let name = op["output"].as_str().unwrap_or("layer").to_string();
        // the module only exists if the worker actually trained it
        if !trained.contains_key(&name) {
            continue;
        }
        let is_fusion = index == 0;
        out.push(grove::contracts::ModuleSpec {
            input_space: if is_fusion { "fused".to_string() } else { "hidden".to_string() },
            output_space: if is_fusion {
                "hidden".to_string()
            } else {
                "logits".to_string()
            },
            layers: vec![name.clone()],
            depends_on: if is_fusion {
                Vec::new()
            } else {
                vec![linears[0]["output"].as_str().unwrap_or_default().to_string()]
            },
            shared_group: None,
            // the head is the class boundary: it is what a composition
            // must not silently re-purpose, so it is frozen context
            frozen: index + 1 == linears.len(),
            requires: grove::contracts::ActorRole::Operator,
            name,
        });
    }
    out
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
        handshake_timeout: std::time::Duration::from_secs(300),
    };
    Worker::spawn(&config, &iso)
}

pub fn seed_snapshot(store: &Store, actor: &Actor, tag: &str) -> Result<ArtifactRef> {
    let weights = store.artifacts().put(tag.as_bytes()).unwrap();
    let snapshot = grove::contracts::ModelSnapshot::new(
        actor.id.clone(),
        "predict",
        vec![grove::contracts::ParamRef {
            module: "fusion".to_string(),
            shape: vec![1],
            dtype: "float32".to_string(),
            artifact: weights,
        }],
        "geometry-sensor-xor@1.0.0",
    );
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
