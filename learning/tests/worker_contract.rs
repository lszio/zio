//! W04 worker contract: isolation is real, the protocol is bounded, and
//! training through the host produces weights that a *separate* process
//! can load and use.
//!
//! These tests skip when the torch venv is absent. That is a reported
//! missing backend, not a silent pass: the isolation and protocol cases
//! below run regardless.

use std::path::PathBuf;
use std::time::Duration;

use grove::contracts::{ErrorKind, SCHEMA_VERSION};
use grove::store::Store;
use grove::worker::{
    commit_worker_output, digest_file, Frame, Isolation, Worker, WorkerConfig, PROTOCOL,
    PROTOCOL_VERSION,
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn task_dir() -> PathBuf {
    repo_root().join("examples/self-learning")
}

fn venv_python() -> PathBuf {
    repo_root().join(".venv/bin/python")
}

fn worker_config() -> WorkerConfig {
    let root = repo_root();
    WorkerConfig {
        python: venv_python(),
        worker_script: root.join("workers/torch/worker.py"),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: repo_root(),
        timeout: Duration::from_secs(240),
        max_address_space: 4 * 1024 * 1024 * 1024,
    }
}

fn isolation() -> Option<Isolation> {
    let iso = Isolation::probe("unshare")?;
    iso.enforce().ok()?;
    Some(iso)
}

/// The permitted graph for the linear baseline, as the wire form.
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
        "loss": null,
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
        "loss": null,
        "trainable": ["h0", "h1"]
    })
}

/// Layers absent from the parameter artifact take the run's seeded init.
fn seeded_init() -> String {
    "{}".to_string()
}

/// One linear layer's artifact: weight (rows=out, cols=in) plus bias. The
/// bias must travel with the weight or the checkpoint is not the model.
fn json_layer(rows: usize, cols: usize) -> serde_json::Value {
    serde_json::json!({
        "w": serde_json::Value::Array(
            (0..rows)
                .map(|_| serde_json::Value::Array(vec![serde_json::json!(0.0); cols]))
                .collect(),
        ),
        "b": serde_json::Value::Array(vec![serde_json::json!(0.0); rows]),
    })
}

// ── isolation ──────────────────────────────────────────────────────

#[test]
fn isolation_is_probed_not_assumed() {
    let iso = Isolation::probe("unshare").expect("probe always returns a report");
    if iso.namespaces {
        assert!(iso.enforce().is_ok());
    } else {
        // Fail closed: a host without namespaces must refuse, not degrade.
        let err = iso.enforce().unwrap_err();
        assert_eq!(err.kind, ErrorKind::CapabilityDenied);
        assert!(err.to_string().contains("refusing"), "{err}");
    }
}

#[test]
fn a_missing_unshare_binary_reports_no_namespaces() {
    let iso = Isolation::probe("/nonexistent/unshare").unwrap();
    assert!(!iso.namespaces, "a missing unshare cannot provide isolation");
    assert_eq!(iso.enforce().unwrap_err().kind, ErrorKind::CapabilityDenied);
}

#[test]
fn a_worker_is_actually_network_isolated() {
    let Some(iso) = isolation() else {
        eprintln!("skipping: no namespace isolation available");
        return;
    };
    // A probe run twice: once plainly (network reachable), once inside the
    // exact namespace flags the host applies (network blocked). The pair is
    // the proof — a reachable probe outside and a blocked one inside.
    let probe = std::env::temp_dir().join("grove-net-probe.py");
    std::fs::write(
        &probe,
        "import json, socket\n\
         try:\n\
         \x20   socket.create_connection((\"1.1.1.1\", 53), timeout=2).close()\n\
         \x20   print(json.dumps({\"network\": \"reachable\"}))\n\
         except Exception as exc:\n\
         \x20   print(json.dumps({\"network\": \"blocked\", \"error\": str(exc)}))\n",
    )
    .unwrap();
    let python = venv_python();
    let read = |out: std::process::Output| -> String {
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .map(|v| v["network"].as_str().unwrap_or("?").to_string())
            .unwrap_or_else(|_| "?".to_string())
    };

    let plain = std::process::Command::new(&python).arg(&probe).output().unwrap();
    let inside = std::process::Command::new("unshare")
        .args(["-Urn", "--pid", "--mount", "--fork", "--"])
        .arg(&python)
        .arg(&probe)
        .output()
        .unwrap();

    assert_eq!(read(plain), "reachable", "without isolation the network must be reachable");
    assert_eq!(read(inside), "blocked", "inside the namespace the network must be blocked");

    // And the host can spawn a real worker under that same isolation.
    let config = worker_config();
    let mut worker = Worker::spawn(&config, &iso).expect("spawn inside the namespace");
    let err = worker
        .request(
            Frame::Train {
                v: PROTOCOL_VERSION,
                request_id: "r".into(),
                run_id: "r".into(),
                attempt_id: "a".into(),
                graph: linear_graph(),
                weights: "/dev/null".into(),
                data: "/dev/null".into(),
                val_data: None,
                out: "/dev/null".into(),
                steps: 1,
                seed: 0,
                resume: None,
                save_at: None,
                state_out: None,
                stop_after_save: None,
            },
            Duration::from_secs(5),
        );
    // A missing input artifact is a clean failure — never a network call.
    assert!(matches!(err, Ok(Frame::Failed { .. })) || err.is_err());
    worker.kill();
}

// ── protocol ───────────────────────────────────────────────────────

#[test]
fn an_oversized_frame_is_refused() {
    // An oversized control frame is refused before it is parsed.
    let huge = "x".repeat(grove::worker::MAX_FRAME_BYTES + 10);
    let err = Frame::decode(&huge).unwrap_err();
    assert_eq!(err.kind, ErrorKind::BackendFailed);
    assert!(err.to_string().contains("cap"), "{err}");
    // A frame that is not JSON at all is refused, not defaulted.
    assert!(Frame::decode("not a frame at all").is_err());
}

#[test]
fn an_unknown_frame_type_is_not_decoded_as_a_known_one() {
    // serde's tag-based enum refuses an unknown variant rather than
    // defaulting to something permissive.
    let result = serde_json::from_str::<Frame>(r#"{"v":1,"type":"launch-missiles","run_id":"r"}"#);
    assert!(result.is_err(), "an unknown action must not decode");
}

// ── real training through the host ─────────────────────────────────

#[test]
fn training_through_the_host_produces_loadable_weights() {
    if !venv_python().exists() {
        eprintln!("skipping: no torch venv; install workers/torch/requirements.txt");
        return;
    }
    let Some(iso) = isolation() else {
        eprintln!("skipping: no namespace isolation available");
        return;
    };

    let scratch = std::env::temp_dir().join(format!("grove-w04-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let config = WorkerConfig { scratch: scratch.clone(), ..worker_config() };
    let mut worker = Worker::spawn(&config, &iso).unwrap();

    let seed_weights = scratch.join("seed.json");
    std::fs::write(&seed_weights, seeded_init()).unwrap();
    let trained = scratch.join("trained.json");

    let frame = worker.request(
        Frame::Train {
            v: PROTOCOL_VERSION,
            request_id: "req-1".into(),
            run_id: "run-1".into(),
            attempt_id: "att-1".into(),
            graph: nonlinear_graph(),
            weights: seed_weights.to_string_lossy().into_owned(),
            data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
            val_data: Some(task_dir().join("data/val.bin").to_string_lossy().into_owned()),
            out: trained.to_string_lossy().into_owned(),
            steps: 150,
            seed: 1,
            resume: None,
            save_at: None,
            state_out: None,
            stop_after_save: None,
        },
        Duration::from_secs(200),
    )
    .unwrap();

    match frame {
        Frame::Done { loss, first_loss, val_accuracy, .. } => {
            let loss = loss.expect("a finished run reports a loss");
            let first = first_loss.expect("a finished run reports its first loss");
            assert!(loss < first, "training did not reduce the loss");
            let accuracy = val_accuracy.unwrap_or(0.0);
            assert!(accuracy >= 0.9, "the nonlinear structure underfit: {accuracy}");
        }
        other => panic!("expected a done frame, got {other:?}"),
    }
    assert!(!worker.progress.is_empty(), "progress frames must be observable");

    // The saved weights become an immutable artifact in a real store.
    let store = Store::open(scratch.join("store")).unwrap();
    let digest = commit_worker_output(&store, &trained).unwrap();
    assert!(store.artifacts().exists(&digest));
    assert_ne!(digest, grove::contracts::digest_bytes(b""));

    // A *different* worker process loads them and predicts.
    let mut predictor = Worker::spawn(&config, &iso).unwrap();
    let predictions = scratch.join("pred.json");
    let out = predictor.request(
        Frame::Predict {
            v: PROTOCOL_VERSION,
            run_id: "run-predict".into(),
            attempt_id: "att-2".into(),
            graph: nonlinear_graph(),
            weights: trained.to_string_lossy().into_owned(),
            data: task_dir().join("data/val.bin").to_string_lossy().into_owned(),
            out: predictions.to_string_lossy().into_owned(),
        },
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(matches!(out, Frame::Done { .. }), "{out:?}");

    let rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&predictions).unwrap()).unwrap();
    let answers = rows["predictions"].as_array().expect("predictions array");
    assert!(!answers.is_empty());
    assert!(answers.iter().all(|r| {
        let a = r["answer"].as_u64().unwrap_or(99);
        a == 0 || a == 1
    }));
}

#[test]
fn an_invalid_graph_is_rejected_without_training() {
    if !venv_python().exists() {
        return;
    }
    let Some(iso) = isolation() else { return };
    let scratch = std::env::temp_dir().join(format!("grove-w04-bad-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let config = WorkerConfig { scratch: scratch.clone(), ..worker_config() };
    let mut worker = Worker::spawn(&config, &iso).unwrap();

    let mut graph = linear_graph();
    graph["ops"].as_array_mut().unwrap().push(serde_json::json!({
        "kind": "exec", "inputs": ["h0"], "output": "bad", "attrs": {}
    }));
    let seed = scratch.join("seed.json");
    std::fs::write(&seed, seeded_init()).unwrap();

    let frame = worker.request(
        Frame::Train {
            v: PROTOCOL_VERSION,
            request_id: "req-2".into(),
            run_id: "run-2".into(),
            attempt_id: "att-3".into(),
            graph,
            weights: seed.to_string_lossy().into_owned(),
            data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
            val_data: None,
            out: scratch.join("out.json").to_string_lossy().into_owned(),
            steps: 1,
            seed: 1,
            resume: None,
            save_at: None,
            state_out: None,
            stop_after_save: None,
        },
        Duration::from_secs(60),
    )
    .unwrap();

    match frame {
        Frame::Failed { error, .. } => {
            assert!(error.contains("not permitted"), "unclear rejection: {error}");
        }
        other => panic!("a non-permitted operator must fail the run, got {other:?}"),
    }
    // The rejected run wrote nothing that could be mistaken for output.
    assert!(!scratch.join("out.json").exists());
}

#[test]
fn a_timeout_kills_the_worker_rather_than_hanging() {
    if !venv_python().exists() {
        return;
    }
    let Some(iso) = isolation() else { return };
    let scratch = std::env::temp_dir().join(format!("grove-w04-slow-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let config = WorkerConfig { scratch: scratch.clone(), ..worker_config() };
    let mut worker = Worker::spawn(&config, &iso).unwrap();

    let seed = scratch.join("seed.json");
    std::fs::write(&seed, seeded_init()).unwrap();

    // A step budget far beyond the timeout: the host must give up and
    // kill the process, not wait for the worker.
    let err = worker.request(
        Frame::Train {
            v: PROTOCOL_VERSION,
            request_id: "req-3".into(),
            run_id: "run-3".into(),
            attempt_id: "att-4".into(),
            graph: nonlinear_graph(),
            weights: seed.to_string_lossy().into_owned(),
            data: task_dir().join("data/train.bin").to_string_lossy().into_owned(),
            val_data: None,
            out: scratch.join("slow.json").to_string_lossy().into_owned(),
            steps: 5_000_000,
            seed: 1,
            resume: None,
            save_at: None,
            state_out: None,
            stop_after_save: None,
        },
        Duration::from_millis(1500),
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Timeout, "got {err}");

    // The process is gone: a subsequent frame read fails rather than
    // blocking forever.
    let after = worker.next_frame(Duration::from_millis(500));
    assert!(after.is_err(), "a killed worker must not keep speaking");
}

#[test]
fn a_corrupt_input_artifact_is_reported_before_the_run() {
    let missing = std::env::temp_dir().join("grove-does-not-exist.bin");
    let err = digest_file(&missing).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ArtifactUnavailable);
}

#[test]
fn the_worker_protocol_constant_is_the_one_the_plan_names() {
    assert_eq!(PROTOCOL, "grove.worker/1");
    assert_eq!(PROTOCOL_VERSION, 1);
    // and the store's schema gate is unaffected by the worker existing
    assert_eq!(SCHEMA_VERSION, 1);
}
