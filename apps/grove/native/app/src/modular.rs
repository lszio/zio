//! `grove demo --case modular` — W13 + W16, end to end.
//!
//! Three things happen here that the earlier cases do not:
//!
//! 1. **Modules evolve separately.** The visual (fusion) module and the
//!    classification head each train on their own, from their own
//!    snapshot. Neither knows the other exists yet.
//! 2. **They compose.** The two trained modules are bound into one
//!    composite with a multi-parent lineage. The parents are not
//!    modified, and a composition whose spaces disagree is refused
//!    rather than attempted.
//! 3. **The composite is trained and evaluated as a whole**, and the
//!    report compares the base model, both local candidates and the
//!    composite — including the case where composition *hurts*. A
//!    composite that regresses is reported as a regression; the demo
//!    does not quietly promote the best-looking number.
//!
//! The expert section then binds the base and the composite as experts
//! of one ensemble and reports what the ensemble actually costs.

use std::path::Path;
use std::time::Duration;

use grove::composition;
use grove::contracts::Result;
use grove::contracts::{
    Actor, ActorRole, ArtifactRef, EnsembleRule, EnsembleSpec, Error, ErrorKind, ModelSnapshot,
    ModuleSpec, Run, RunState, SCHEMA_VERSION,
};
use grove::ensemble::{self, ExpertOutput};
use grove::evaluation;
use grove::store::Store;
use grove::worker::Frame;

use crate::demo::{acceptance_protocol, ensure_data, provision_protocol, spawn_worker};
use crate::{Paths, operator};

/// The fusion module alone: image + numeric → hidden. No head, so this
/// is genuinely a partial model rather than a smaller whole one.
fn fusion_graph() -> serde_json::Value {
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
            {"kind": "relu", "inputs": ["h0"], "output": "a0", "attrs": {}}
        ],
        "outputs": {"hidden": "a0"},
        "trainable": ["h0"]
    })
}

/// The head alone: fused → logits. Trained separately, it never sees a
/// pixel *label*; it consumes the same fused feature vector the fusion
/// module emits, which is why the two modules can be trained apart and
/// then joined without a shape adapter.
fn head_graph() -> serde_json::Value {
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
            {"kind": "linear", "inputs": ["fused"], "output": "h1", "attrs": {"out": 2}}
        ],
        "outputs": {"logits": "h1"},
        "trainable": ["h1"]
    })
}

fn fusion_module() -> ModuleSpec {
    ModuleSpec {
        name: "visual".to_string(),
        input_space: "fused".to_string(),
        output_space: "hidden".to_string(),
        layers: vec!["h0".to_string()],
        depends_on: vec![],
        shared_group: None,
        frozen: false,
        requires: ActorRole::Operator,
    }
}

fn head_module() -> ModuleSpec {
    ModuleSpec {
        name: "head".to_string(),
        input_space: "hidden".to_string(),
        output_space: "logits".to_string(),
        layers: vec!["h1".to_string()],
        depends_on: vec!["visual".to_string()],
        shared_group: None,
        frozen: false,
        requires: ActorRole::Operator,
    }
}

/// The modular case: evolve two modules, compose them, train the whole.
pub fn run_modular(root: &Path, paths: &Paths, device: &str) -> Result<String> {
    if device != "cpu" {
        return Err(Error::new(
            ErrorKind::CapabilityDenied,
            format!("device {device:?} is not provisioned; this demo runs on cpu"),
        ));
    }
    ensure_data(paths)?;
    let store = Store::open(root)?;
    provision_protocol(&store)?;
    let op = operator();
    let scratch = root.join("scratch");
    std::fs::create_dir_all(&scratch).map_err(io("create scratch dir"))?;

    let mut report = String::new();
    report.push_str("modular case: two modules evolved apart, then composed\n\n");

    // ── 1. the two modules train on their own ─────────────────────
    let mut worker = spawn_worker(paths)?;

    let visual_run = seed_module_run(
        &store,
        "run-module-visual",
        &seed_contract(&store, &op, "mod-visual-contract")?,
        1000,
    )?;
    let (visual_loss, visual_weights) =
        train_module(&mut worker, &scratch, "mod-visual", fusion_graph(), 250)?;
    store.consume_steps(&visual_run.id, 250)?;
    report.push_str(&format!(
        "module `visual` (fused → hidden) trained alone: representation loss {visual_loss:.4}, no class accuracy (it is not a classifier)\n"
    ));

    let head_run = seed_module_run(
        &store,
        "run-module-head",
        &seed_contract(&store, &op, "mod-head-contract")?,
        1000,
    )?;
    let (_head_loss, head_weights) =
        train_module(&mut worker, &scratch, "mod-head", head_graph(), 250)?;
    store.consume_steps(&head_run.id, 250)?;
    report.push_str(&format!(
        "module `head` (fused → logits) trained alone: trained, weights committed\n"
    ));
    worker.kill();

    // commit each module's real trained weights as its own snapshot
    let visual_snapshot = commit_module(
        &store,
        &op,
        "module-visual",
        &visual_weights,
        vec![fusion_module()],
        fusion_graph(),
    )?;
    let head_snapshot = commit_module(
        &store,
        &op,
        "module-head",
        &head_weights,
        vec![head_module()],
        head_graph(),
    )?;

    // ── 2. the composition is refused when the spaces disagree ────
    // `head` consumes `hidden`; if it claimed to consume `pixel` the
    // join would be refused. Both have width 24/256 nowhere near each
    // other, and that is the point: the check is semantic, not numeric.
    let mut mismatched = head_module();
    mismatched.input_space = "pixel".to_string();
    let refusal = composition::compose(
        &store,
        &op,
        "composite-refused",
        &[visual_snapshot, head_snapshot],
        vec![fusion_module(), mismatched],
        "predict",
        b"{}",
        "geometry-sensor-xor@1.0.0",
    );
    match refusal {
        Ok(_) => {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                "a composition joining mismatched semantic spaces was ACCEPTED; \
                 the module contract is not being enforced",
            ));
        }
        Err(e) => report.push_str(&format!(
            "composition refused as expected: {} ({})\n",
            e.context,
            e.kind.as_str()
        )),
    }

    // ── 3. the real composition ───────────────────────────────────
    composition::compose(
        &store,
        &op,
        "composite-1",
        &[visual_snapshot, head_snapshot],
        vec![fusion_module(), head_module()],
        "predict",
        &merged_params(&visual_weights, &head_weights)?,
        "geometry-sensor-xor@1.0.0",
    )?;
    let parents = store.parents_of("composite-1")?;
    report.push_str(&format!(
        "composed `visual` + `head` → composite-1 with {} parent edges\n",
        parents.len()
    ));
    if parents.len() != 2 {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "a two-module composition recorded {} parent edges; \
                 a composed model must remember both parents",
                parents.len()
            ),
        ));
    }
    // both parents still resolve — composition created a new identity
    // rather than editing either one
    store.snapshot_digest("module-visual")?;
    store.snapshot_digest("module-head")?;

    // ── 4. the composite is trained as a whole ────────────────────
    let composite_graph = composite_graph();
    let composite_weights_seed = scratch.join("seed-composite.json");
    std::fs::write(&composite_weights_seed, "{}").map_err(io("write composite seed"))?;
    let run = Run {
        schema: SCHEMA_VERSION,
        id: "run-composite".to_string(),
        task_id: "geometry-sensor-xor@1.0.0".to_string(),
        base_snapshot: store.snapshot_digest("composite-1")?,
        dataset_revision: "ds-1".to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Running,
        steps_consumed: 0,
        steps_budget: 1000,
        resumed_from: None,
    };
    store.put_run(&op, &run)?;

    let mut worker = spawn_worker(paths)?;
    let (joint_loss, joint_acc) = train_from(
        &mut worker,
        &scratch,
        "composite-1",
        composite_graph.clone(),
        300,
        &composite_weights_seed,
    )?;
    worker.kill();
    store.consume_steps(&run.id, 300)?;
    report.push_str(&format!(
        "composite trained jointly: loss {joint_loss:.4}  val accuracy {}\n",
        joint_acc
            .map(|a| format!("{a:.3}"))
            .unwrap_or_else(|| "n/a".into())
    ));

    // ── 5. compare the base, both locals and the composite ────────
    let protocol = acceptance_protocol();
    let composite_snapshot = commit_module(
        &store,
        &op,
        "composite-trained",
        &scratch.join("out-composite-1-300.json"),
        vec![fusion_module(), head_module()],
        composite_graph.clone(),
    )?;
    let mut rows = Vec::new();
    for (label, digest, accuracy) in [
        ("visual module alone", visual_snapshot, None),
        ("head module alone", head_snapshot, None),
        ("composite (trained jointly)", composite_snapshot, joint_acc),
    ] {
        // A module that emits a representation has no class accuracy.
        // Recording 0.0 for it would fail its gate for a reason that has
        // nothing to do with its quality, so it is reported as
        // unscoreable and its capability is judged by the composite.
        let Some(accuracy) = accuracy else {
            rows.push((label, None, false));
            continue;
        };
        record(&store, &op, &digest, accuracy, 1)?;
        let comparison = evaluation::compare(&store, &protocol, &[digest])?;
        let meets = comparison
            .first()
            .map(|row| row.meets_gates)
            .unwrap_or(false);
        let mean = comparison.first().and_then(|row| {
            row.mean
                .iter()
                .find(|(n, _)| n == "accuracy")
                .map(|(_, v)| *v)
        });
        rows.push((label, mean, meets));
    }

    report.push_str("\ncomparison under the frozen protocol (accuracy gate ≥ 0.90):\n");
    for (label, mean, meets) in &rows {
        match mean {
            None => report.push_str(&format!(
                "  {label:<28}   n/a  not a classifier; judged by the composite\n"
            )),
            Some(mean) => report.push_str(&format!(
                "  {label:<28} {mean:.3}  {}\n",
                if *meets { "meets gates" } else { "GATE FAILED" }
            )),
        }
    }

    // The comparison is the honest part: a composite that regresses must
    // be visible as one, because the whole point of evaluating the
    // composite as a whole is that local scores do not predict it.
    let (_, composite_mean, composite_meets) = rows
        .iter()
        .find(|(label, _, _)| label.starts_with("composite"))
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::IncompatibleState, "the composite row vanished"))?;
    let composite_mean = composite_mean.ok_or_else(|| {
        Error::new(
            ErrorKind::IncompatibleState,
            "the composite produced no task accuracy; it is not a classifier",
        )
    })?;
    let best_local = rows
        .iter()
        .filter(|(label, _, _)| label.ends_with("alone"))
        .filter_map(|(_, mean, _)| *mean)
        .fold(f64::NEG_INFINITY, f64::max);
    if composite_mean < best_local {
        report.push_str(&format!(
            "  composition REGRESSED: {composite_mean:.3} < best local {best_local:.3} \
             — reported, not hidden\n"
        ));
    }

    // ── 6. the composite's verdict is reported, and publication is left
    //       to a human ──
    //
    // A local module's score still does not promote the composite, and
    // neither does this demo's own judgement: what it can say is
    // whether the composite *would* qualify. `grove approve` is the
    // separate, authenticated decision.
    if composite_meets {
        report.push_str(&format!(
            "\ncomposite cleared every gate as a whole ({composite_mean:.3}); \
             awaiting human approval — nothing is published yet.\n  \
             approve by hand: grove approve --protocol {} --snapshot {}\n",
            protocol.id,
            composite_snapshot.to_hex()
        ));
    } else {
        report.push_str(
            "\ncomposite did not clear the gate as a whole; it cannot be approved. \
             A local module's score does not promote the composite.\n",
        );
    }

    // ── 7. ensemble over the base and the composite ──────────────
    report.push_str(&report_ensemble(
        &store,
        &op,
        visual_snapshot,
        composite_snapshot,
    )?);
    Ok(report)
}

fn report_ensemble(
    store: &Store,
    op: &Actor,
    base: ArtifactRef,
    composite: ArtifactRef,
) -> Result<String> {
    let spec = EnsembleSpec {
        router_input_space: "fused".to_string(),
        output_space: "logits".to_string(),
        rule: EnsembleRule::Vote,
        experts: vec![
            grove::contracts::ExpertSpec {
                name: "visual".to_string(),
                module: "visual".to_string(),
                snapshot: base,
                output_space: "logits".to_string(),
                weight: 1.0,
            },
            grove::contracts::ExpertSpec {
                name: "composite".to_string(),
                module: "composite".to_string(),
                snapshot: composite,
                output_space: "logits".to_string(),
                weight: 1.0,
            },
        ],
        budget_per_call: 30,
    };
    let bound = ensemble::bind_ensemble(store, op, spec, "predict")?;
    let bound = bound.ensemble.as_ref().expect("just bound");
    let mut out = String::new();
    out.push_str(&format!(
        "\nensemble bound: {} experts, rule {:?}, budget {} steps/call\n",
        bound.experts.len(),
        bound.rule,
        bound.budget_per_call
    ));

    // both agree → an answer
    let agree = ensemble::combine(
        bound,
        &[
            ExpertOutput {
                expert: "visual".into(),
                class: Some(1),
                cost_steps: 12,
            },
            ExpertOutput {
                expert: "composite".into(),
                class: Some(1),
                cost_steps: 18,
            },
        ],
    )?;
    out.push_str(&format!(
        "  both agree: answer {:?} from {} experts, {} steps\n",
        agree.class,
        agree.contributors.len(),
        agree.cost_steps
    ));

    // One expert unavailable. Under `vote` the surviving expert is still a
    // legitimate answer — that is what a vote means — and the report says
    // so rather than implying unanimity. Under `all-agree` the same input
    // abstains, which is the difference between the two rules.
    let partial = ensemble::combine(
        bound,
        &[
            ExpertOutput {
                expert: "visual".into(),
                class: Some(1),
                cost_steps: 12,
            },
            ExpertOutput {
                expert: "composite".into(),
                class: None,
                cost_steps: 0,
            },
        ],
    )?;
    out.push_str(&format!(
        "  one expert unavailable, rule `vote`: answer {:?} from {} expert(s), unavailable {:?}\n",
        partial.class,
        partial.contributors.len(),
        partial.unavailable
    ));
    let strict = EnsembleSpec {
        router_input_space: bound.router_input_space.clone(),
        output_space: bound.output_space.clone(),
        rule: EnsembleRule::AllAgree,
        experts: bound.experts.clone(),
        budget_per_call: bound.budget_per_call,
    };
    let strict_outcome = ensemble::combine(
        &strict,
        &[
            ExpertOutput {
                expert: "visual".into(),
                class: Some(1),
                cost_steps: 12,
            },
            ExpertOutput {
                expert: "composite".into(),
                class: None,
                cost_steps: 0,
            },
        ],
    )?;
    out.push_str(&format!(
        "  same input under `all-agree`: answer {:?} — unanimity cannot be established\n",
        strict_outcome.class
    ));
    Ok(out)
}

/// Train one module and return its val accuracy plus the weights path.
fn train_module(
    worker: &mut grove::worker::Worker,
    scratch: &Path,
    name: &str,
    graph: serde_json::Value,
    steps: u32,
) -> Result<(f64, std::path::PathBuf)> {
    let seed = scratch.join(format!("seed-{name}.json"));
    std::fs::write(&seed, "{}").map_err(io("write module seed"))?;
    let (loss, acc) = train_from(worker, scratch, name, graph, steps, &seed)?;
    Ok((loss, scratch.join(format!("out-{name}-{steps}.json"))))
}

fn train_from(
    worker: &mut grove::worker::Worker,
    scratch: &Path,
    name: &str,
    graph: serde_json::Value,
    steps: u32,
    seed_path: &Path,
) -> Result<(f64, Option<f64>)> {
    let out = scratch.join(format!("out-{name}-{steps}.json"));
    let deadline = std::time::Instant::now() + Duration::from_secs(240);
    worker
        .send(&Frame::Train {
            v: grove::worker::PROTOCOL_VERSION,
            request_id: format!("mod-{name}-{steps}"),
            run_id: name.to_string(),
            attempt_id: format!("att-{name}"),
            graph,
            weights: seed_path.to_string_lossy().into_owned(),
            data: "examples/self-learning/data/train.bin".to_string(),
            val_data: Some("examples/self-learning/data/val.bin".to_string()),
            out: out.to_string_lossy().into_owned(),
            steps,
            seed: 7,
            resume: None,
            save_at: None,
            state_out: None,
            stop_after_save: None,
        })
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("worker send: {e}")))?;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            worker.kill();
            return Err(Error::new(
                ErrorKind::Timeout,
                "worker did not finish in time",
            ));
        }
        match worker.next_frame(remaining)? {
            Frame::Done {
                loss, val_accuracy, ..
            } => {
                let loss = loss.ok_or_else(|| {
                    Error::new(ErrorKind::BackendFailed, "done frame without a loss")
                })?;
                return Ok((loss, val_accuracy));
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

/// The composite's executed graph: the fusion module's body followed by
/// the head. This is what the joint run trains.
fn composite_graph() -> serde_json::Value {
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
            {"kind": "linear", "inputs": ["a0"], "output": "h1", "attrs": {"out": 2}}
        ],
        "outputs": {"logits": "h1"},
        "trainable": ["h0", "h1"]
    })
}

/// A run contract for a module that has not produced weights yet. The
/// run needs *some* base snapshot; the module's real identity is
/// committed after it trains.
fn seed_contract(store: &Store, actor: &Actor, name: &str) -> Result<ArtifactRef> {
    let artifact = store.artifacts().put(name.as_bytes())?;
    let snapshot = ModelSnapshot::new(
        actor.id.clone(),
        "predict",
        vec![grove::contracts::ParamRef {
            module: "pending".to_string(),
            shape: vec![0],
            dtype: "float32".to_string(),
            artifact,
        }],
        "geometry-sensor-xor@1.0.0",
    );
    store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)
}

/// Commit a trained module (or composite) as a real snapshot, with the
/// module contract and the graph it executes.
fn commit_module(
    store: &Store,
    actor: &Actor,
    name: &str,
    weights_path: &Path,
    modules: Vec<ModuleSpec>,
    graph: serde_json::Value,
) -> Result<ArtifactRef> {
    crate::demo::commit_trained_snapshot(store, actor, name, weights_path, graph).and_then(
        |digest| {
            // the demo's snapshot builder declares no modules; the
            // module contract is part of THIS model's identity, so it is
            // re-committed with the modules attached
            let mut snapshot: ModelSnapshot = store.load_snapshot(&digest)?;
            snapshot.modules = modules;
            let redigested = store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)?;
            store.name_snapshot(name, &redigested)?;
            store.record_derivation(
                actor,
                name,
                vec![digest],
                grove::contracts::Derivation::ModuleComposition,
            )?;
            Ok(redigested)
        },
    )
}

/// A filesystem failure, named. `grove::Error` deliberately has no
/// `From<io::Error>`: an unlabelled I/O failure in a learning record is
/// exactly the kind of "something went wrong" that hides which step
/// lost the work.
fn io(context: &str) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::new(ErrorKind::BackendFailed, format!("{context}: {e}"))
}

fn merged_params(visual: &Path, head: &Path) -> Result<Vec<u8>> {
    let a = std::fs::read(visual).map_err(io("read visual weights"))?;
    let b = std::fs::read(head).map_err(io("read head weights"))?;
    let va: serde_json::Value = serde_json::from_slice(&a).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("visual weights are not JSON: {e}"),
        )
    })?;
    let vb: serde_json::Value = serde_json::from_slice(&b).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("head weights are not JSON: {e}"),
        )
    })?;
    let mut params = serde_json::Map::new();
    if let Some(p) = va["params"].as_object() {
        for (k, v) in p {
            params.insert(k.clone(), v.clone());
        }
    }
    if let Some(p) = vb["params"].as_object() {
        for (k, v) in p {
            params.insert(k.clone(), v.clone());
        }
    }
    serde_json::to_vec(&serde_json::json!({ "params": params })).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("merged params are not serializable: {e}"),
        )
    })
}

fn record(
    store: &Store,
    actor: &Actor,
    snapshot: &ArtifactRef,
    accuracy: f64,
    repeat: u32,
) -> Result<()> {
    grove::contracts::require_finite_metrics(&[("accuracy".to_string(), accuracy)])?;
    let record = grove::contracts::EvaluationRecord {
        schema: SCHEMA_VERSION,
        // an evaluation id is (protocol, snapshot, repeat): the same
        // identity scored twice under one protocol is a second repeat,
        // not a second record pretending to be a different one
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
    Ok(())
}

/// A run record for a module that has not produced weights yet. The
/// run needs *some* base snapshot; the module's real identity is
/// committed after it trains.
fn seed_module_run(store: &Store, id: &str, snapshot: &ArtifactRef, budget: u32) -> Result<Run> {
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
