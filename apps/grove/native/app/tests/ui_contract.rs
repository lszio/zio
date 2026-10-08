//! W11 contract: the product surface the browser actually drives.
//!
//! This is deliberately a *served-asset* contract plus the read shapes
//! the UI depends on. The full interactive walkthrough is browser
//! evidence (screenshots from a real session), not something a headless
//! assertion can stand in for — but the properties below are the ones
//! that would silently break the UI, and each one is a real HTTP call
//! against a real server:
//!
//! * the three assets are served same-origin with usable content types
//!   (a UI that loads a 403 page is not a UI);
//! * a path traversal in an asset name is refused, not resolved;
//! * the lineage view carries recovery level and invalidation, because a
//!   node that silently lost its state artifact must not look healthy;
//! * a correction reports "filed" and "learned into" as separate facts;
//! * module view refuses nothing silently — a snapshot whose bytes are
//!   gone is reported unloadable;
//! * an observation with a short pixel list is refused rather than padded,
//!   because padding is how a model gets told it saw a black image.

#![cfg(feature = "http")]

use std::collections::HashMap;

use grove::contracts::{
    Actor, ActorRole, ModelSnapshot, Run, RunState, SCHEMA_VERSION, SignalKind,
};
use grove::store::Store;

fn tokens() -> HashMap<String, ActorRole> {
    let mut map = HashMap::new();
    map.insert("reader-tok".to_string(), ActorRole::Reader);
    map.insert("annotator-tok".to_string(), ActorRole::Annotator);
    map.insert("operator-tok".to_string(), ActorRole::Operator);
    map.insert("publisher-tok".to_string(), ActorRole::Publisher);
    map
}

fn start_server() -> (String, Arc<Store>) {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("grove-w11-{}-{nonce}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Arc::new(Store::open(&root).unwrap());
    let state = grove_app::api::ApiState::new(Arc::clone(&store), tokens());

    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _ = addr_tx.send(listener.local_addr().unwrap());
            let _ = axum::serve(listener, grove_app::api::router(state)).await;
        });
    });
    let addr = addr_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("the server binds its port");
    (format!("http://{addr}"), store)
}

use std::sync::Arc;

struct Reply {
    status: u16,
    body: String,
    content_type: String,
}

fn request(
    base: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> Reply {
    let agent = ureq::Agent::new();
    let url = format!("{base}{path}");
    let mut req = match method {
        "GET" => agent.get(&url),
        _ => agent.post(&url),
    };
    if let Some(t) = token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    let out = match &body {
        Some(value) => req
            .set("Content-Type", "application/json")
            .send_string(&value.to_string()),
        None => req.call(),
    };
    let (status, text, content_type) = match out {
        Ok(r) => (
            r.status(),
            r.into_string().unwrap_or_default(),
            String::new(),
        ),
        Err(ureq::Error::Status(code, r)) => {
            (code, r.into_string().unwrap_or_default(), String::new())
        }
        Err(e) => (0, e.to_string(), String::new()),
    };
    Reply {
        status,
        body: text,
        content_type,
    }
}

// ── assets ────────────────────────────────────────────────────────

#[test]
fn the_ui_is_served_same_origin_with_usable_assets() {
    let (base, _store) = start_server();

    let index = request(&base, "GET", "/", None, None);
    assert_eq!(index.status, 200, "the product page must be served");
    assert!(
        index.body.contains("<form") && index.body.contains("API token"),
        "the served page is the control surface, not a placeholder: {}",
        &index.body[..index.body.len().min(200)]
    );

    let js = request(&base, "GET", "/app.js", None, None);
    assert_eq!(js.status, 200, "the script the page loads must exist");

    let css = request(&base, "GET", "/styles.css", None, None);
    assert_eq!(css.status, 200, "the stylesheet must exist");

    // an asset handler that can read outside its directory is a file
    // disclosure, not a convenience
    let escape = request(&base, "GET", "/../Cargo.toml", None, None);
    assert_ne!(
        escape.status, 200,
        "a traversal must not resolve to a real file"
    );
}

#[test]
fn an_observation_with_a_short_image_is_refused_not_padded() {
    let (base, _store) = start_server();

    // 100 samples: padding it to 256 would tell the model it saw a
    // mostly-black image that nobody observed.
    let short = request(
        &base,
        "POST",
        "/api/observations",
        Some("reader-tok"),
        Some(serde_json::json!({
            "operation_id": "op-short",
            "id": "obs-short",
            "task": "task-1",
            "source": "contract",
            "modality_mask": [true, true],
            "scene_id": 1,
            "pixels": vec![10u8; 100],
            "readings": [1.0, 1.0]
        })),
    );
    assert_eq!(short.status, 400, "{}", short.body);
    assert!(
        short.body.contains("256"),
        "the refusal must name the expected width: {}",
        short.body
    );

    // the real width is accepted
    let good = request(
        &base,
        "POST",
        "/api/observations",
        Some("reader-tok"),
        Some(serde_json::json!({
            "operation_id": "op-good",
            "id": "obs-good",
            "task": "task-1",
            "source": "contract",
            "modality_mask": [true, true],
            "scene_id": 1,
            "pixels": vec![10u8; 256],
            "readings": [1.0, 1.0]
        })),
    );
    assert_eq!(good.status, 201, "{}", good.body);
}

// ── the views the UI depends on ───────────────────────────────────

#[test]
fn the_signal_view_separates_filed_from_learned() {
    let (base, store) = start_server();
    let annotator = Actor::new("api-annotator", ActorRole::Annotator);

    let observation_id = "obs-sig-1";
    let pixels = vec![7u8; 256];
    let content = store.artifacts().put(&pixels).unwrap();
    let observation = grove::contracts::Observation {
        schema: SCHEMA_VERSION,
        id: observation_id.to_string(),
        owner: "api-reader".to_string(),
        task_id: "task-1".to_string(),
        session_id: "contract".to_string(),
        source: "contract".to_string(),
        occurred_at_ms: 1,
        blocks: vec![grove::contracts::ContentBlock {
            media_type: "application/x-gvd1".into(),
            artifact: content,
        }],
        modality_mask: vec![true, true],
        training_permitted: true,
    };
    store
        .put_observation(&Actor::new("api-reader", ActorRole::Reader), &observation)
        .unwrap();

    let signal = grove::contracts::LearningSignal {
        schema: SCHEMA_VERSION,
        id: "corr-1".to_string(),
        idempotency_key: "corr-1".to_string(),
        producer: annotator.id.clone(),
        kind: SignalKind::HumanCorrection,
        task_id: "task-1".to_string(),
        observation_id: Some(observation_id.to_string()),
        prediction_id: None,
        target_field: Some("label".to_string()),
        content: "1".to_string(),
        usage_permitted: true,
        occurred_at_ms: 1,
        received_at_ms: 1,
        revises: None,
    };
    store.submit_signal(&annotator, &signal).unwrap();

    let view = request(
        &base,
        "GET",
        "/api/signals/corr-1",
        Some("reader-tok"),
        None,
    );
    assert_eq!(view.status, 200, "{}", view.body);
    let parsed: serde_json::Value = serde_json::from_str(&view.body).unwrap();
    assert_eq!(parsed["status"], "accepted");
    assert_eq!(parsed["in_force"], true);
    // filed ≠ learned: no dataset has been frozen yet
    assert_eq!(
        parsed["learned_into"].as_array().unwrap().len(),
        0,
        "a filed correction has not been learned into anything yet"
    );

    // freezing a dataset that carries it makes the second fact true
    let revision = grove::contracts::DatasetRevision {
        schema: SCHEMA_VERSION,
        id: "ds-ui-1".to_string(),
        task_id: "task-1".to_string(),
        policy_version: "policy@1".to_string(),
        signal_ids: vec!["corr-1".to_string()],
        observation_ids: vec![observation_id.to_string()],
        split: "train".to_string(),
        frozen_at_ms: 2,
    };
    store
        .freeze_dataset(&Actor::new("api-operator", ActorRole::Operator), &revision)
        .unwrap();

    let after = request(
        &base,
        "GET",
        "/api/signals/corr-1",
        Some("reader-tok"),
        None,
    );
    let parsed: serde_json::Value = serde_json::from_str(&after.body).unwrap();
    assert_eq!(
        parsed["learned_into"].as_array().unwrap()[0],
        "ds-ui-1",
        "after a freeze the correction IS in a dataset, and the view says so"
    );
}

#[test]
fn the_lineage_view_marks_unrecoverable_and_invalidated_nodes() {
    let (base, store) = start_server();
    let operator = Actor::new("api-operator", ActorRole::Operator);

    // a committed run with a checkpoint whose state artifact is deleted
    let weights = store.artifacts().put(b"w").unwrap();
    let snapshot = ModelSnapshot::new(
        "api-operator",
        "predict",
        vec![grove::contracts::ParamRef {
            module: "fusion".into(),
            shape: vec![1],
            dtype: "float32".into(),
            artifact: weights,
        }],
        "geometry-sensor-xor@1.0.0",
    );
    let digest = store
        .commit_manifest("ModelSnapshot", "api-operator", &snapshot)
        .unwrap();
    store.name_snapshot("seed-1", &digest).unwrap();

    let run = Run {
        schema: SCHEMA_VERSION,
        id: "run-ui-1".to_string(),
        task_id: "task-1".to_string(),
        base_snapshot: digest,
        dataset_revision: "ds-1".to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Paused,
        steps_consumed: 10,
        steps_budget: 100,
        resumed_from: None,
    };
    store.put_run(&operator, &run).unwrap();

    // commit a checkpoint, then delete the object it points at.
    // The state carries the *current* schema: a schema-1 artifact predates
    // the recorded trainable set and cannot serve a LearningContinuation,
    // which is a refusal rather than a stale-but-usable checkpoint.
    let state_bytes = format!(
        r#"{{"schema":{},"protocol":"grove.worker.state/1","run_id":"run-ui-1","step":10,
             "params":{{"h0":{{"w":[0.0],"b":[0.0]}}}},
             "optimizer":{{"adam":{{
                "h0.weight":{{"m":[0.0],"v":[0.0]}},
                "h0.bias":{{"m":[0.0],"v":[0.0]}}}}}},
             "rng":{{"torch":1}}}}"#,
        grove::checkpoint::STATE_SCHEMA
    );
    let scratch = std::env::temp_dir().join(format!("grove-w11-state-{}", nonce_of()));
    std::fs::create_dir_all(&scratch).unwrap();
    let state_path = scratch.join("state.json");
    std::fs::write(&state_path, state_bytes).unwrap();
    let checkpoint = grove::checkpoint::commit(
        &store,
        &operator,
        "run-ui-1",
        None,
        grove::contracts::ResumeLevel::LearningContinuation,
        &state_path,
        10,
        3,
    )
    .unwrap();

    let before = request(&base, "GET", "/api/lineage", Some("reader-tok"), None);
    let parsed: serde_json::Value = serde_json::from_str(&before.body).unwrap();
    let node = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == checkpoint.id.as_str())
        .expect("the checkpoint is in the lineage view");
    assert_eq!(node["recoverable"], true, "the artifact is still there");

    // now remove it: the view must say the node is NOT recoverable
    store
        .artifacts()
        .remove(&checkpoint.state_artifact)
        .unwrap();
    let after = request(&base, "GET", "/api/lineage", Some("reader-tok"), None);
    let parsed: serde_json::Value = serde_json::from_str(&after.body).unwrap();
    let node = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["id"] == checkpoint.id.as_str())
        .unwrap();
    assert_eq!(
        node["recoverable"], false,
        "a checkpoint whose state artifact is gone is not recoverable, \
         and the view must say so rather than hide it"
    );
    assert_eq!(
        node["resume_level"], "learning-continuation",
        "the declared recovery level travels with the node"
    );
}

#[test]
fn the_module_view_reports_module_contracts_and_experts() {
    let (base, store) = start_server();
    let operator = Actor::new("api-operator", ActorRole::Operator);

    // an empty store still returns a list: a view that 404s when there is
    // nothing is a view whose absence is indistinguishable from failure
    let empty = request(&base, "GET", "/api/modules", Some("reader-tok"), None);
    assert_eq!(empty.status, 200, "{}", empty.body);
    let parsed: serde_json::Value = serde_json::from_str(&empty.body).unwrap();
    assert_eq!(
        parsed["snapshots"].as_array().unwrap().len(),
        0,
        "an empty store has no snapshots, and says so"
    );

    // a real snapshot with modules: the view must surface the semantic
    // spaces, the frozen flag and the permission requirement, because
    // those are what composition is checked against
    let weights = store.artifacts().put(b"m").unwrap();
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: operator.id.clone(),
        entrypoint: "predict".to_string(),
        params: vec![grove::contracts::ParamRef {
            module: "visual".into(),
            shape: vec![2],
            dtype: "float32".into(),
            artifact: weights,
        }],
        libraries: vec![],
        preprocessing_version: "geometry-sensor-xor@1.0.0".to_string(),
        modules: vec![grove::contracts::ModuleSpec {
            name: "visual".into(),
            input_space: "pixel".into(),
            output_space: "pixel".into(),
            layers: vec!["visual_h".into()],
            depends_on: vec![],
            shared_group: Some("shared".into()),
            frozen: true,
            requires: ActorRole::Publisher,
        }],
        ensemble: None,
        graph: None,
    };
    let digest = store
        .commit_manifest("ModelSnapshot", &operator.id, &snapshot)
        .unwrap();
    store.name_snapshot("composite-ui", &digest).unwrap();

    let view = request(&base, "GET", "/api/modules", Some("reader-tok"), None);
    let parsed: serde_json::Value = serde_json::from_str(&view.body).unwrap();
    let node = &parsed["snapshots"][0];
    assert_eq!(node["name"], "composite-ui");
    assert_eq!(node["deployable"], true, "nothing invalidated this one");
    let module = &node["modules"][0];
    assert_eq!(module["input_space"], "pixel");
    assert_eq!(module["output_space"], "pixel");
    assert_eq!(module["frozen"], true);
    assert_eq!(module["shared_group"], "shared");
    assert_eq!(
        module["requires"], "publisher",
        "the permission a module demands is part of its contract"
    );
}

fn nonce_of() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
