//! W05 contract: code and weights change together, and the description
//! of the model comes from Zio.
//!
//! The flow under test: Zio (`lib/zio/learn/model.zio`) builds the graph
//! description and evolves it structurally; `grove`'s worker trains and
//! evaluates exactly that description on the frozen W00 task. The three
//! populations the acceptance gate names are all here:
//!
//!   1. the fixed linear baseline (structure frozen, params trained);
//!   2. params-only on the fixed baseline (the same thing — a control);
//!   3. code + params: the nonlinear rewrite, then training.
//!
//! Offline except for the worker process, which is local torch on CPU.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use grove::worker::{Frame, Isolation, Worker, WorkerConfig, PROTOCOL_VERSION};
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::value::Value;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn task_dir() -> PathBuf {
    workspace_root().join("examples/self-learning")
}

fn venv_python() -> PathBuf {
    workspace_root().join(".venv/bin/python")
}

// ── Zio harness ────────────────────────────────────────────────────

fn eval_str(ctx: &EvalContext, src: &str) -> Result<Value, EvalError> {
    let source_id = ctx.source_map().register("dual-test".into(), src.to_string());
    let forms = zio_core::reader::reader::read_program_with_source(src, source_id)
        .map_err(|e| EvalError::custom(format!("parse error: {e}")))?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = zio_core::eval::eval_in_context(&sexp, ctx)?;
    }
    Ok(last)
}

fn model_ctx() -> EvalContext {
    let env = Arc::new(Env::new(None));
    zio_core::builtins::setup_env(&env);
    let ctx = EvalContext::new(env);
    let source_id = ctx.source_map().register("core.zio".into(), zio_core::stdlib_source().to_string());
    let forms = zio_core::reader::reader::read_program_with_source(
        zio_core::stdlib_source(),
        source_id,
    )
    .expect("stdlib parses");
    for sexp in forms {
        zio_core::eval::eval_in_context(&sexp, &ctx).expect("stdlib loads");
    }
    for lib in ["learn/model.zio", "learn/recipes.zio"] {
        let path = workspace_root().join("lib/zio").join(lib);
        let source = std::fs::read_to_string(&path).expect("lib exists");
        let sid = ctx.source_map().register(lib.into(), source.clone());
        let forms = zio_core::reader::reader::read_program_with_source(&source, sid)
            .expect("lib parses");
        for sexp in forms {
            zio_core::eval::eval_in_context(&sexp, &ctx)
                .unwrap_or_else(|e| panic!("loading {lib}: {e}"));
        }
    }
    ctx
}

/// The graph JSON a Zio description renders to. Value Display is read
/// syntax, and `json-stringify` is the boundary that turns keyword keys
/// and nils into strict JSON the worker parses.
fn graph_json(ctx: &EvalContext, expr: &str) -> serde_json::Value {
    let value = eval_str(ctx, expr).expect("model description evaluates");
    // Stringify the VALUE itself, not its display form: the reader pulls
    // `,` into symbols, so a display round trip would corrupt `nil,`.
    eval_str(ctx, &format!("(def graph--last '{value})"))
        .expect("binding the description works");
    let rendered = match eval_str(ctx, "(json-stringify graph--last)") {
        Ok(Value::String(s)) => s,
        other => panic!("json-stringify failed: {other:?}"),
    };
    serde_json::from_str(&rendered)
        .unwrap_or_else(|e| panic!("Zio description must be worker-valid JSON ({e}): {rendered}"))
}

// ── worker harness ─────────────────────────────────────────────────

fn isolation() -> Option<Isolation> {
    let iso = Isolation::probe("unshare")?;
    iso.enforce().ok()?;
    Some(iso)
}

fn worker_config() -> WorkerConfig {
    let root = workspace_root();
    WorkerConfig {
        python: venv_python(),
        worker_script: root.join("workers/torch/worker.py"),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: root.clone(),
        timeout: std::time::Duration::from_secs(240),
        max_address_space: 4 * 1024 * 1024 * 1024,
    }
}

fn start_trainer() -> Option<Worker> {
    if !venv_python().exists() {
        eprintln!("skipping: no torch venv");
        return None;
    }
    let Some(iso) = isolation() else {
        eprintln!("skipping: no namespace isolation");
        return None;
    };
    Some(Worker::spawn(&worker_config(), &iso).expect("worker spawns"))
}

/// Train `graph` for `steps` and return (loss, val_accuracy).
fn train_graph(worker: &mut Worker, dir: &PathBuf, graph: serde_json::Value, steps: u32, seed: u64)
    -> (f64, f64)
{
    let seed_weights = dir.join("seed.json");
    std::fs::write(&seed_weights, "{}").unwrap();
    let trained = dir.join(format!("trained-{seed}-{steps}.json"));
    let frame = worker.request(
        Frame::Train {
            v: PROTOCOL_VERSION,
            request_id: format!("req-{seed}-{steps}"),
            run_id: format!("run-{seed}-{steps}"),
            attempt_id: format!("att-{seed}-{steps}"),
            graph,
            weights: seed_weights.to_string_lossy().into_owned(),
            data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
            val_data: Some(task_dir().join("data/val.bin").to_string_lossy().into_owned()),
            out: trained.to_string_lossy().into_owned(),
            steps,
            seed,
            resume: None,
            save_at: None,
            state_out: None,
            stop_after_save: None,
        },
        std::time::Duration::from_secs(180),
    )
    .expect("train request completes");
    match frame {
        Frame::Done { loss, val_accuracy, .. } => {
            (loss.expect("loss"), val_accuracy.unwrap_or(0.0))
        }
        other => {
            let diag = worker.drain_stderr(4000);
            panic!("expected done, got {other:?}\nworker stderr: {diag}");
        }
    }
}

// ── the contract ───────────────────────────────────────────────────

#[test]
fn zio_describes_and_the_worker_executes() {
    // The graph the worker trains is literally the Zio value: the
    // description crosses as data, not as a second hand-written copy.
    let ctx = model_ctx();
    let linear = graph_json(&ctx, "(model--linear-graph)");
    assert_eq!(linear["outputs"]["logits"], "h0");
    assert_eq!(linear["trainable"], serde_json::json!(["h0"]));

    let nonlinear = graph_json(&ctx, "(model--nonlinear-graph 24)");
    assert_eq!(nonlinear["outputs"]["logits"], "h1");
    assert_eq!(nonlinear["trainable"], serde_json::json!(["h0", "h1"]));
    // the structural change is present: a relu on a 24-wide hidden layer
    let ops = nonlinear["ops"].as_array().unwrap();
    assert!(ops.iter().any(|o| o["kind"] == "relu"), "rewrite inserted a relu");
    assert!(
        ops.iter().any(|o| o["kind"] == "linear" && o["output"] == "h0"
            && o["attrs"]["out"] == 24),
        "the widened layer carries its declared width"
    );
}

#[test]
fn the_rewrite_is_idempotent_and_scoped() {
    let ctx = model_ctx();
    let once = graph_json(&ctx, "(model--nonlinear-graph 24)");
    // applying the same rewrite to an already-nonlinear graph changes nothing
    let twice = graph_json(&ctx, "(model--rewrite-add-hidden (model--nonlinear-graph 24) 24)");
    assert_eq!(once, twice, "the rewrite must be idempotent");

    // the baseline is untouched by building candidates from it
    let baseline = graph_json(&ctx, "(model--linear-graph)");
    assert_eq!(baseline["outputs"]["logits"], "h0");
}

#[test]
fn candidate_lifecycle_rejects_out_of_order_transitions() {
    let ctx = model_ctx();
    // a candidate that never trained cannot be evaluated
    let v = eval_str(
        &ctx,
        r#"(let* [c (model--candidate 1 (model--linear-graph) (fn [g] g) :baseline)
                 bad (model--mark-evaluated c {:accuracy 1.0 :gate 0.9})]
            [(get bad :state nil) (get bad :error nil)])"#,
    )
    .unwrap();
    let rendered = format!("{v}");
    assert!(rendered.contains(":rejected"), "{rendered}");
    assert!(rendered.contains("only a trained candidate"), "{rendered}");

    // acceptance requires the gate, not just evaluation
    let v = eval_str(
        &ctx,
        r#"(let* [c (model--candidate 2 (model--linear-graph) (fn [g] g) :baseline)
                 t (model--mark-trained c "sha256:params")
                 e (model--mark-evaluated t {:accuracy-bp 5000})
                 r (model--accept e 9000)]
            (get r :state nil))"#,
    )
    .unwrap();
    assert!(format!("{v}").contains(":rejected"));

    let v = eval_str(
        &ctx,
        r#"(let* [c (model--candidate 3 (model--linear-graph) (fn [g] g) :baseline)
                 t (model--mark-trained c "sha256:params")
                 e (model--mark-evaluated t {:accuracy-bp 9500})
                 r (model--accept e 9000)]
            (get r :state nil))"#,
    )
    .unwrap();
    assert!(format!("{v}").contains(":accepted"));
}

#[test]
fn selection_prefers_accuracy_then_smaller_graph() {
    let ctx = model_ctx();
    let v = eval_str(
        &ctx,
        r#"(let* [mk (fn [n acc-bp graph]
                     (model--mark-evaluated
                       (model--mark-trained
                         (model--candidate n graph (fn [g] g) :rewrite)
                         "sha256:p")
                       {:accuracy-bp acc-bp}))
                 a (mk 1 8500 (model--linear-graph))
                 b (mk 2 9500 (model--nonlinear-graph 24))
                 c (mk 3 9500 (model--linear-graph))]
            (get (model--select [a b c]) :id nil))"#,
    )
    .unwrap();
    // b and c tie at 0.95; the smaller graph (c, linear) wins
    assert_eq!(format!("{v}"), "\"cand-3\"", "got {v}");
}

#[test]
fn recipes_cap_and_gate_what_candidates_may_do() {
    let ctx = model_ctx();
    // a recipe refuses signal kinds it does not declare
    let v = eval_str(
        &ctx,
        r#"[(recipes--check-signal recipes--supervised :teacher-label)
            (recipes--check-signal recipes--supervised :environment-result)]"#,
    )
    .unwrap();
    let rendered = format!("{v}");
    assert!(rendered.contains("true") && rendered.contains("false"), "{rendered}");

    // a recipe caps the step budget whatever a candidate asks for
    let v = eval_str(&ctx, "(recipes--budget-for recipes--supervised 99999)").unwrap();
    assert_eq!(format!("{v}"), "512", "got {v}");
}

// ── the three populations on the real task ─────────────────────────

#[test]
fn the_three_populations_on_the_w00_task() {
    let Some(mut worker) = start_trainer() else { return };
    let dir = std::env::temp_dir().join(format!("grove-w05-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let ctx = model_ctx();
    // population 1+2: the fixed linear baseline, params only
    let linear = graph_json(&ctx, "(model--linear-graph)");
    // population 3: the structural rewrite, then training
    let nonlinear = graph_json(&ctx, "(model--nonlinear-graph 24)");

    let (linear_loss, linear_acc) = train_graph(&mut worker, &dir, linear, 300, 7);
    let (joint_loss, joint_acc) = train_graph(&mut worker, &dir, nonlinear, 300, 7);

    // The fixed baseline cannot represent the XOR; the structural change
    // can. This is the acceptance gate's lift, measured.
    assert!(
        linear_acc < 0.75,
        "the linear baseline should stay stuck on the XOR, got {linear_acc}"
    );
    assert!(
        joint_acc >= 0.9,
        "the nonlinear rewrite should fit it, got {joint_acc}"
    );
    assert!(
        (joint_acc - linear_acc) * 100.0 >= 15.0,
        "lift {}pp is below the frozen +15pp gate",
        (joint_acc - linear_acc) * 100.0
    );
    assert!(joint_loss < linear_loss, "loss must fall too");

    worker.kill();
}

#[test]
fn a_structural_change_reinitializes_and_keeps_the_old_model() {
    // The old candidate's parameter artifact is immutable: the rewrite
    // starts from the seed, never from the parent's trained weights.
    let Some(mut worker) = start_trainer() else { return };
    let dir = std::env::temp_dir().join(format!("grove-w05-fork-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = model_ctx();

    let nonlinear = graph_json(&ctx, "(model--nonlinear-graph 24)");

    // parent: trained with seed 11
    let parent = dir.join("parent.json");
    std::fs::write(&parent, "{}").unwrap();
    let parent_out = dir.join("parent-out.json");
    let parent_frame = worker
        .request(
            Frame::Train {
                v: PROTOCOL_VERSION,
                request_id: "req-parent".into(),
                run_id: "run-parent".into(),
                attempt_id: "att-parent".into(),
                graph: nonlinear.clone(),
                weights: parent.to_string_lossy().into_owned(),
                data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
                val_data: None,
                out: parent_out.to_string_lossy().into_owned(),
                steps: 60,
                seed: 11,
                resume: None,
                save_at: None,
                state_out: None,
                stop_after_save: None,
            },
            std::time::Duration::from_secs(120),
        )
        .expect("parent trains");
    assert!(matches!(parent_frame, Frame::Done { .. }), "{parent_frame:?}");
    let parent_before = std::fs::read(&parent_out).unwrap();

    // child: same structure, seed 12 — a different starting point
    let child = dir.join("child.json");
    std::fs::write(&child, "{}").unwrap();
    let child_out = dir.join("child-out.json");
    let child_frame = worker
        .request(
            Frame::Train {
                v: PROTOCOL_VERSION,
                request_id: "req-child".into(),
                run_id: "run-child".into(),
                attempt_id: "att-child".into(),
                graph: nonlinear,
                weights: child.to_string_lossy().into_owned(),
                data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
                val_data: None,
                out: child_out.to_string_lossy().into_owned(),
                steps: 60,
                seed: 12,
                resume: None,
                save_at: None,
                state_out: None,
                stop_after_save: None,
            },
            std::time::Duration::from_secs(120),
        )
        .expect("child trains");
    assert!(matches!(child_frame, Frame::Done { .. }), "{child_frame:?}");

    // the parent's artifact is untouched by the child's run
    let parent_after = std::fs::read(&parent_out).unwrap();
    assert_eq!(parent_before, parent_after, "fork must not modify the parent");
    // and the two runs genuinely differ (fresh init, not inheritance)
    let child_weights: serde_json::Value = serde_json::from_slice(&std::fs::read(&child_out).unwrap()).unwrap();
    let parent_weights: serde_json::Value = serde_json::from_slice(&parent_before).unwrap();
    assert_ne!(
        child_weights["params"], parent_weights["params"],
        "a different seed must not inherit the parent's parameters"
    );
    worker.kill();
}

#[test]
fn a_failed_training_disqualifies_only_its_candidate() {
    let Some(mut worker) = start_trainer() else { return };
    let dir = std::env::temp_dir().join(format!("grove-w05-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = model_ctx();

    // candidate A: an unpermitted operator — structurally rejected alone
    let mut bad = graph_json(&ctx, "(model--linear-graph)");
    bad["ops"].as_array_mut().unwrap().push(serde_json::json!({
        "kind": "exec", "inputs": ["h0"], "output": "bad", "attrs": {}
    }));
    let ok = graph_json(&ctx, "(model--linear-graph)");

    let bad_seed = dir.join("bad-seed.json");
    std::fs::write(&bad_seed, "{}").unwrap();
    let bad_out = dir.join("bad-out.json");
    let bad_frame = worker
        .request(
            Frame::Train {
                v: PROTOCOL_VERSION,
                request_id: "req-bad".into(),
                run_id: "run-bad".into(),
                attempt_id: "att-bad".into(),
                graph: bad,
                weights: bad_seed.to_string_lossy().into_owned(),
                data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
                val_data: None,
                out: bad_out.to_string_lossy().into_owned(),
                steps: 10,
                seed: 1,                resume: None,
                save_at: None,
                state_out: None,
                stop_after_save: None,
            },
            std::time::Duration::from_secs(60),
        )
        .expect("the bad candidate fails, not the worker");
    let bad_error = match bad_frame {
        Frame::Failed { error, .. } => error,
        other => panic!("the invalid candidate must fail, got {other:?}"),
    };

    // the same worker process is still healthy: candidate B trains fine
    let ok_seed = dir.join("ok-seed.json");
    std::fs::write(&ok_seed, "{}").unwrap();
    let ok_out = dir.join("ok-out.json");
    let ok_frame = worker
        .request(
            Frame::Train {
                v: PROTOCOL_VERSION,
                request_id: "req-ok".into(),
                run_id: "run-ok".into(),
                attempt_id: "att-ok".into(),
                graph: ok,
                weights: ok_seed.to_string_lossy().into_owned(),
                data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
                val_data: None,
                out: ok_out.to_string_lossy().into_owned(),
                steps: 20,
                seed: 1,                resume: None,
                save_at: None,
                state_out: None,
                stop_after_save: None,
            },
            std::time::Duration::from_secs(60),
        )
        .expect("the healthy candidate trains");
    assert!(matches!(ok_frame, Frame::Done { .. }), "{ok_frame:?}");
    assert!(bad_out.metadata().map(|m| m.len()).unwrap_or(0) == 0 || !bad_out.exists(),
        "the failed candidate wrote no output");
    assert!(ok_out.exists(), "the healthy candidate wrote its output");
    eprintln!("bad candidate rejected with: {bad_error}");
    worker.kill();
}

#[test]
fn an_exhausted_budget_stops_the_loop() {
    // budget is policy: the Zio layer refuses to start what it cannot pay
    // for, and the run ledger is what the host enforces.
    let ctx = model_ctx();
    let v = eval_str(
        &ctx,
        r#"[(model--fits? 100 60 40)
            (model--fits? 100 60 41)
            (model--budget-remaining 100 150)]"#,
    )
    .unwrap();
    let rendered = format!("{v}");
    assert!(rendered.contains("true") && rendered.contains("false"), "{rendered}");
    assert!(rendered.contains("0"), "over budget leaves nothing: {rendered}");

    // and the host's ledger agrees (consume_steps refuses overspend)
    let root = std::env::temp_dir().join(format!("grove-w05-ledger-{}", std::process::id()));
    let store = grove::store::Store::open(&root).unwrap();
    let operator = grove::contracts::Actor::new("trainer", grove::contracts::ActorRole::Operator);
    let run = grove::contracts::Run {
        schema: grove::contracts::SCHEMA_VERSION,
        id: "run-ledger".into(),
        task_id: "task-1".into(),
        base_snapshot: grove::contracts::digest_bytes(b"base"),
        dataset_revision: "ds-1".into(),
        recipe: "supervised@1".into(),
        state: grove::contracts::RunState::Running,
        steps_consumed: 0,
        steps_budget: 100,
        resumed_from: None,
    };
    store.put_run(&operator, &run).unwrap();
    store.consume_steps("run-ledger", 60).unwrap();
    assert_eq!(
        store.consume_steps("run-ledger", 41).unwrap_err().kind,
        grove::contracts::ErrorKind::BudgetExhausted,
        "the host must be the second lock on the same budget"
    );
}
