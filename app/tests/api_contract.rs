//! W10 API contract: the product surface over real HTTP.
//!
//! A real server binds on loopback and real requests drive it. The
//! properties under test are the ones a local product cannot skip: role
//! separation on every mutating call (a reader cannot annotate, an
//! annotator cannot train, an operator cannot publish), idempotent
//! mutations, expected-version conflicts, and a refusal to expose an
//! unauthenticated non-loopback surface.

#![cfg(feature = "http")]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use grove::contracts::{ActorRole, ModelSnapshot, ParamRef, RunState, SCHEMA_VERSION};
use grove::store::Store;

fn tokens() -> HashMap<String, ActorRole> {
    let mut map = HashMap::new();
    map.insert("reader-tok".to_string(), ActorRole::Reader);
    map.insert("annotator-tok".to_string(), ActorRole::Annotator);
    map.insert("operator-tok".to_string(), ActorRole::Operator);
    map.insert("publisher-tok".to_string(), ActorRole::Publisher);
    map
}

/// Start a real server on a loopback port (on a helper thread) and return
/// its base URL plus the shared store.
/// One store per test: SQLite locks per file, so parallel tests must not
/// share a root.
fn start_server() -> (String, Arc<Store>) {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("grove-w10-{}-{nonce}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Arc::new(Store::open(&root).unwrap());
    let state = grove_app::api::ApiState::new(Arc::clone(&store), tokens());

    // bind INSIDE the runtime (a std listener cannot be handed to tokio)
    let (addr_tx, addr_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let _ = addr_tx.send(addr);
            let app = grove_app::api::router(state);
            let _ = axum::serve(listener, app).await;
        });
    });
    let addr = addr_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the server binds its port");
    (format!("http://{addr}"), store)
}

struct Reply {
    status: u16,
    body: serde_json::Value,
}

/// One real HTTP request through ureq.
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
    let result = match &body {
        // axum's Json extractor needs the content type; a POST without it
        // is a 415, not a business failure
        Some(value) => req
            .set("Content-Type", "application/json")
            .send_string(&value.to_string()),
        None => req.call(),
    };
    match result {
        Ok(response) => Reply {
            status: response.status(),
            body: response
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(serde_json::Value::Null),
        },
        Err(ureq::Error::Status(code, response)) => Reply {
            status: code,
            body: response
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(serde_json::Value::Null),
        },
        Err(e) => Reply {
            status: 0,
            body: serde_json::json!({ "transport": e.to_string() }),
        },
    }
}

/// A committed snapshot plus a recipe the API can use.
fn seed_world(store: &Store) -> (String, String) {
    let actor = grove::contracts::Actor::new("api-operator", ActorRole::Operator);
    let weights = store.artifacts().put(b"api-weights").unwrap();
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: "api-operator".to_string(),
        entrypoint: "predict".to_string(),
        params: vec![ParamRef {
            module: "fusion".into(),
            shape: vec![1],
            dtype: "float32".into(),
            artifact: weights,
        }],
        libraries: vec![],
        preprocessing_version: "geometry-sensor-xor@1.0.0".into(),
    };
    let snapshot_hex = store
        .commit_manifest("ModelSnapshot", "api-operator", &snapshot)
        .unwrap()
        .to_hex();

    store
        .put_recipe(
            &actor,
            &grove::contracts::Recipe {
                schema: SCHEMA_VERSION,
                id: "supervised@1".to_string(),
                task_id: "task-1".to_string(),
                consumes: vec![grove::contracts::SignalKind::HumanCorrection],
                trainable_modules: vec!["fusion".to_string()],
                budget_steps: 256,
            },
        )
        .unwrap();
    (snapshot_hex, "supervised@1".to_string())
}

// ── auth ───────────────────────────────────────────────────────────

#[test]
fn health_needs_no_token_but_everything_else_does() {
    let (base, _store) = start_server();

    let health = request(&base, "GET", "/api/health", None, None);
    assert_eq!(health.status, 200, "liveness is not a capability");
    assert_eq!(health.body["status"], "ok");

    let runs = request(&base, "GET", "/api/runs", None, None);
    assert_eq!(runs.status, 403, "an unauthenticated read is refused");
    assert_eq!(runs.body["class"], "capability-denied");

    let bad = request(&base, "GET", "/api/runs", Some("not-a-token"), None);
    assert_eq!(bad.status, 403);
    assert!(bad.body["detail"]
        .as_str()
        .unwrap_or_default()
        .contains("unknown token"));
}

// ── role separation ────────────────────────────────────────────────

#[test]
fn roles_do_not_bleed_into_each_other() {
    let (base, store) = start_server();
    let (snapshot, _recipe) = seed_world(&store);

    // a reader may observe...
    let observe = request(
        &base,
        "POST",
        "/api/observations",
        Some("reader-tok"),
        Some(serde_json::json!({
            "operation_id": "op-obs-1",
            "id": "obs-1",
            "task": "task-1",
            "source": "line-a",
            "modality_mask": [true, true],
            "content": "{\"image\": \"...\"}"
        })),
    );
    assert_eq!(observe.status, 201, "{:?}", observe.body);

    // ...but may not annotate
    let signal_by_reader = request(
        &base,
        "POST",
        "/api/signals",
        Some("reader-tok"),
        Some(serde_json::json!({
            "operation_id": "op-sig-1", "id": "sig-1",
            "kind": "human-correction", "observation_id": "obs-1",
            "content": "clear", "usage_permitted": true
        })),
    );
    assert_eq!(signal_by_reader.status, 403);
    let detail = signal_by_reader.body["detail"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        detail.contains("annotator"),
        "the refusal must name the missing grant: {detail}"
    );

    // an annotator may correct...
    let signal = request(
        &base,
        "POST",
        "/api/signals",
        Some("annotator-tok"),
        Some(serde_json::json!({
            "operation_id": "op-sig-2", "id": "sig-2",
            "kind": "human-correction", "observation_id": "obs-1",
            "target_field": "state", "content": "clear",
            "usage_permitted": true
        })),
    );
    assert_eq!(signal.status, 201, "{:?}", signal.body);

    // ...but may not start training
    let train_by_annotator = request(
        &base,
        "POST",
        "/api/learning/runs",
        Some("annotator-tok"),
        Some(serde_json::json!({
            "operation_id": "op-run-1",
            "base_snapshot": snapshot,
            "dataset_revision": "ds-1",
            "recipe": "supervised@1",
            "steps_budget": 100
        })),
    );
    assert_eq!(train_by_annotator.status, 403);
    assert!(train_by_annotator.body["detail"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase()
        .contains("operator"));

    // an operator may start a run, and the recipe caps what it may ask for
    let run = request(
        &base,
        "POST",
        "/api/learning/runs",
        Some("operator-tok"),
        Some(serde_json::json!({
            "operation_id": "op-run-2",
            "base_snapshot": snapshot,
            "dataset_revision": "ds-1",
            "recipe": "supervised@1",
            "steps_budget": 100
        })),
    );
    assert_eq!(run.status, 200, "{:?}", run.body);
    assert_eq!(
        run.body["steps_budget"], 100,
        "the recipe's 256-step budget is the ceiling; 100 fits"
    );

    // ...but may not publish
    let publish_by_operator = request(
        &base,
        "POST",
        "/api/publish",
        Some("operator-tok"),
        Some(serde_json::json!({
            "operation_id": "op-pub-1", "snapshot": snapshot,
            "protocol": "accept-v1"
        })),
    );
    assert_eq!(publish_by_operator.status, 403);
    assert!(publish_by_operator.body["detail"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase()
        .contains("publisher"));
}

// ── idempotency and conflicts ──────────────────────────────────────

#[test]
fn repeated_operations_are_idempotent() {
    let (base, store) = start_server();
    seed_world(&store);

    let body = serde_json::json!({
        "operation_id": "op-sig-idem", "id": "sig-idem",
        "kind": "human-correction", "observation_id": "obs-x",
        "content": "clear", "usage_permitted": true
    });

    let first = request(
        &base,
        "POST",
        "/api/signals",
        Some("annotator-tok"),
        Some(body.clone()),
    );
    assert_eq!(first.status, 201, "{:?}", first.body);
    assert_eq!(first.body["status"], "accepted");

    // the replay returns the FIRST receipt — no second sample
    let replay = request(
        &base,
        "POST",
        "/api/signals",
        Some("annotator-tok"),
        Some(body),
    );
    assert_eq!(replay.status, 201);
    assert_eq!(
        replay.body, first.body,
        "a replay returns the first receipt, not a second effect"
    );
}

#[test]
fn a_conflicting_publication_version_is_refused() {
    let (base, store) = start_server();
    let (snapshot, _) = seed_world(&store);

    // provision the protocol and an eligible evaluation so the only
    // possible failure left is the version check
    let operator_actor = grove::contracts::Actor::new("api-operator", ActorRole::Operator);
    let publisher_actor = grove::contracts::Actor::new("api-publisher", ActorRole::Publisher);
    let mut protocol = grove::evaluation::EvaluationProtocol::new(
        "accept-v1",
        "geometry-sensor-xor@1.0.0",
        "ds-1",
    );
    protocol.gates = vec![("accuracy".to_string(), 0.90)];
    store.put_protocol(&operator_actor, &protocol).unwrap();

    let digest = grove::contracts::ArtifactRef::parse_hex(&snapshot).unwrap();
    store
        .put_evaluation(
            &publisher_actor,
            &grove::contracts::EvaluationRecord {
                schema: SCHEMA_VERSION,
                id: "eval-1".to_string(),
                snapshot: digest,
                protocol_id: "accept-v1".to_string(),
                dataset_revision: "ds-1".to_string(),
                metrics: vec![("accuracy".to_string(), 0.95)],
                repeat_index: 0,
                device: "cpu".to_string(),
                completed_at_ms: 1,
            },
        )
        .unwrap();

    // first publish: version 1
    let first = request(
        &base,
        "POST",
        "/api/publish",
        Some("publisher-tok"),
        Some(serde_json::json!({
            "operation_id": "op-pub-2", "snapshot": snapshot,
            "protocol": "accept-v1", "expected_version": null
        })),
    );
    assert_eq!(first.status, 200, "{:?}", first.body);
    assert_eq!(first.body["version"], 1);

    // a writer that still believes the pointer is at version 0 loses
    let stale = request(
        &base,
        "POST",
        "/api/publish",
        Some("publisher-tok"),
        Some(serde_json::json!({
            "operation_id": "op-pub-3", "snapshot": snapshot,
            "protocol": "accept-v1", "expected_version": 0
        })),
    );
    assert_eq!(stale.status, 409, "{:?}", stale.body);
    assert_eq!(stale.body["class"], "conflict");
    // the active publication is untouched by the refused write
    assert_eq!(store.active_publication().unwrap().unwrap().0, 1);
}

// ── reads ──────────────────────────────────────────────────────────

#[test]
fn runs_events_and_evaluations_are_readable_with_correlated_ids() {
    let (base, store) = start_server();
    let (snapshot, _) = seed_world(&store);
    let actor = grove::contracts::Actor::new("api-operator", ActorRole::Operator);
    store
        .put_run(
            &actor,
            &grove::contracts::Run {
                schema: SCHEMA_VERSION,
                id: "run-visible".to_string(),
                task_id: "task-1".to_string(),
                base_snapshot: grove::contracts::ArtifactRef::parse_hex(&snapshot).unwrap(),
                dataset_revision: "ds-1".to_string(),
                recipe: "supervised@1".to_string(),
                state: RunState::Running,
                steps_consumed: 40,
                steps_budget: 100,
                resumed_from: None,
            },
        )
        .unwrap();

    let runs = request(&base, "GET", "/api/runs", Some("reader-tok"), None);
    assert_eq!(runs.status, 200);
    assert_eq!(runs.body[0]["id"], "run-visible");
    assert_eq!(runs.body[0]["steps_consumed"], 40);

    let events = request(&base, "GET", "/api/events", Some("reader-tok"), None);
    assert_eq!(events.status, 200);
    let text = events.body.to_string();
    assert!(text.contains("run-visible"), "{text}");
    // no token ever appears in the event stream
    assert!(!text.contains("reader-tok"), "events must not leak tokens: {text}");

    let evaluations = request(
        &base,
        "GET",
        &format!("/api/evaluations/{snapshot}"),
        Some("reader-tok"),
        None,
    );
    assert_eq!(evaluations.status, 200);
    assert!(evaluations.body["evaluations"].is_array());
}

// ── bind policy ────────────────────────────────────────────────────

#[test]
fn a_non_loopback_bind_without_tokens_is_refused() {
    let refused: std::net::SocketAddr = "0.0.0.0:8787".parse().unwrap();
    let err = grove_app::api::check_bind(refused, 0).unwrap_err();
    assert_eq!(err.kind, grove::contracts::ErrorKind::CapabilityDenied);
    assert!(err.to_string().contains("without tokens"), "{err}");

    // loopback without tokens is a legitimate single-user local mode
    let loopback: std::net::SocketAddr = "127.0.0.1:8787".parse().unwrap();
    grove_app::api::check_bind(loopback, 0).unwrap();

    // non-loopback WITH tokens is allowed (authenticated, deliberate)
    grove_app::api::check_bind(refused, 4).unwrap();
}
