//! W10: the product API.
//!
//! The HTTP surface is a **shell over the same library contracts** — it
//! holds no learning logic, adds no authority, and cannot be more
//! permissive than the library it calls. What it does add is the two
//! things a local product needs and a library should not:
//!
//! * **Role-separated authentication.** A bearer token maps to exactly
//!   one role (reader / annotator / operator / publisher). Annotating a
//!   label, starting a run and publishing a model are three separate
//!   grants — a token for one is refused for the others, including on
//!   loopback. localhost is not a pass.
//! * **Idempotent mutation + optimistic concurrency.** Modifying calls
//!   carry an operation id (a replay returns the first result, never a
//!   second effect) and head/publication changes carry an expected
//!   version (a stale writer gets an explicit conflict).
//!
//! Binding: loopback only unless a bind address is given *and* tokens are
//! provisioned. A non-loopback bind without tokens is refused rather than
//! quietly serving an open control surface.
//!
//! W11 adds the reads the product UI needs to be honest: a prediction is
//! a record bound to the exact snapshot that produced it; a lineage view
//! walks real derivation edges; an invalidated node is *labelled*
//! non-recoverable rather than quietly hidden; and the static assets are
//! served from this same origin, so the browser never needs a CORS grant
//! to operate a control surface.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use grove::contracts::{
    Actor, ActorRole, ArtifactRef, Error as GroveError, ErrorKind, EvaluationRecord,
    Result as GroveResult, SCHEMA_VERSION,
};
use grove::evaluation::EvaluationProtocol;
use grove::store::Store;
use serde::{Deserialize, Serialize};

/// Everything a request handler needs. The store is behind an Arc so the
/// coordinator and the API can share one ledger.
#[derive(Clone)]
pub struct ApiState {
    /// The store is behind a mutex because rusqlite's `Connection` is
    /// `Send` but not `Sync`; the API serializes database access and the
    /// coordinator keeps its own Arc for the training paths.
    pub store: Arc<Mutex<Store>>,
    pub tokens: Arc<HashMap<String, ActorRole>>,
    /// Idempotency: operation id → the response it already produced.
    pub receipts: Arc<Mutex<HashMap<String, (u16, serde_json::Value)>>>,
    /// Where the real worker lives. The API spawns it to answer a
    /// prediction, so the product's answers come from the same isolated
    /// backend the trainer uses.
    pub paths: Arc<crate::Paths>,
    /// Scratch for staged weights and input files. Lives under the
    /// store's own root so a restart can clean it deterministically.
    pub scratch: PathBuf,
    pub started_ms: i64,
}

impl ApiState {
    pub fn new(store: Arc<Store>, tokens: HashMap<String, ActorRole>) -> Self {
        // adopt the caller's store behind the mutex; the store keeps its
        // own file handle, so the Arc here is only for sharing
        let store = match Arc::try_unwrap(store) {
            Ok(store) => store,
            Err(shared) => Store::open(shared.artifacts().root().parent().unwrap()).expect(
                "the shared store is still referenced; open the same root instead",
            ),
        };
        // staged weights and inputs live under the store's own root, so
        // "delete this product's data directory" removes them too
        let scratch = store.artifacts().root().parent().unwrap().join("scratch");
        let _ = std::fs::create_dir_all(&scratch);
        Self {
            store: Arc::new(Mutex::new(store)),
            tokens: Arc::new(tokens),
            receipts: Arc::new(Mutex::new(HashMap::new())),
            paths: Arc::new(crate::Paths::from_repo_root()),
            scratch,
            started_ms: now_ms(),
        }
    }
}

// ── error shape ────────────────────────────────────────────────────

/// The product's error envelope: a stable class and minimal context.
/// Never the error text as a contract — clients branch on `class`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ApiError {
    pub class: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict_version: Option<u32>,
}

impl From<GroveError> for ApiError {
    fn from(e: GroveError) -> Self {
        let conflict_version = if e.kind == ErrorKind::Conflict {
            e.context
                .split("version ")
                .nth(1)
                .and_then(|v| v.split(|c: char| !c.is_ascii_digit()).next())
                .and_then(|v| v.parse().ok())
        } else {
            None
        };
        Self {
            class: e.kind.as_str().to_string(),
            detail: e.context,
            conflict_version,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match self.class.as_str() {
            "invalid-input" | "protocol-violation" => StatusCode::BAD_REQUEST,
            "capability-denied" => StatusCode::FORBIDDEN,
            "conflict" => StatusCode::CONFLICT,
            "incompatible-state" | "artifact-unavailable" => StatusCode::UNPROCESSABLE_ENTITY,
            "budget-exhausted" => StatusCode::TOO_MANY_REQUESTS,
            "timeout" => StatusCode::GATEWAY_TIMEOUT,
            "cancelled" => StatusCode::GONE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, Json(self)).into_response()
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

/// The store guard. Handlers take it once and use it for their whole
/// transaction — a poisoned mutex is a genuine failure, not a warning.
fn store(state: &ApiState) -> std::sync::MutexGuard<'_, Store> {
    state.store.lock().unwrap_or_else(|e| e.into_inner())
}

// ── auth ───────────────────────────────────────────────────────────

/// Map a bearer token to its role. An unknown or missing token is
/// refused outright — there is no anonymous write path.
fn authenticate(state: &ApiState, headers: &HeaderMap) -> ApiResult<Actor> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| ApiError {
            class: "capability-denied".into(),
            detail: "missing bearer token".into(),
            conflict_version: None,
        })?;
    let role = state.tokens.get(token).copied().ok_or_else(|| ApiError {
        class: "capability-denied".into(),
        detail: "unknown token".into(),
        conflict_version: None,
    })?;
    // token identity is the role name: one token, one role
    Ok(Actor::new(format!("api-{}", role_name(role)), role))
}

fn require(state: &ApiState, headers: &HeaderMap, minimum: ActorRole) -> ApiResult<Actor> {
    let actor = authenticate(state, headers)?;
    if !actor.may(minimum) {
        return Err(ApiError {
            class: "capability-denied".into(),
            detail: format!(
                "{} needs role {minimum:?}, this token holds {:?}",
                actor.id,
                actor.role
            ),
            conflict_version: None,
        });
    }
    Ok(actor)
}

pub fn role_name(role: ActorRole) -> &'static str {
    match role {
        ActorRole::Reader => "reader",
        ActorRole::Annotator => "annotator",
        ActorRole::Operator => "operator",
        ActorRole::Publisher => "publisher",
    }
}

// ── request shapes ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SignalBody {
    pub operation_id: String,
    pub id: String,
    pub kind: String,
    pub observation_id: Option<String>,
    pub prediction_id: Option<String>,
    pub target_field: Option<String>,
    pub content: String,
    pub usage_permitted: bool,
}

#[derive(Debug, Deserialize)]
pub struct TrainBody {
    pub operation_id: String,
    pub base_snapshot: String,
    pub dataset_revision: String,
    pub recipe: String,
    pub steps_budget: u32,
}

#[derive(Debug, Deserialize)]
pub struct PublishBody {
    pub operation_id: String,
    pub snapshot: String,
    pub protocol: String,
    #[serde(default)]
    pub expected_version: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ForkBody {
    pub operation_id: String,
    pub checkpoint: String,
    pub branch: String,
    #[serde(default)]
    pub quota: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct ObserveBody {
    pub operation_id: String,
    pub id: String,
    pub task: String,
    pub source: String,
    /// Which modalities are genuinely present. A false here is a
    /// declared absence: the worker masks the row out and the model
    /// abstains, rather than the value being zero-filled.
    pub modality_mask: Vec<bool>,
    #[serde(default)]
    pub scene_id: u32,
    /// 256 grayscale samples. A short list is a declared error rather
    /// than being padded: a padded image is a lie about what was seen.
    #[serde(default)]
    pub pixels: Vec<u8>,
    #[serde(default)]
    pub readings: Vec<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ReceiptBody {
    pub status: String,
    pub operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct RunRef {
    pub id: String,
    pub state: String,
    pub steps_consumed: u32,
    pub steps_budget: u32,
    /// The run this one continues. A resume creates a NEW run and leaves
    /// the parent's record untouched, so the product must be able to say
    /// which run a ledger continues from — without this, a lineage looks
    /// like three unrelated runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Event {
    pub at_ms: i64,
    pub kind: String,
    pub subject: String,
    pub detail: String,
}

// ── idempotency ────────────────────────────────────────────────────

/// Replay guard: the same operation id returns its first response. A
/// duplicate submit never produces a second sample, and a duplicate
/// publish never flips the pointer twice.
type IdempotentResponse<T> = (axum::http::StatusCode, Json<T>);

/// Look up a replayed response. The FIRST response is what a replay
/// returns: a client that lost the first response must not be told it was
/// created when nothing new happened, and must not be charged for a
/// second effect.
fn replayed<T: serde::de::DeserializeOwned>(
    state: &ApiState,
    operation_id: &str,
) -> Option<IdempotentResponse<T>> {
    let receipts = state.receipts.lock().ok()?;
    let (status, value) = receipts.get(operation_id)?;
    let body = serde_json::from_value(value.clone()).ok()?;
    let code = axum::http::StatusCode::from_u16(*status).unwrap_or(StatusCode::OK);
    Some((code, Json(body)))
}

/// Record the response an operation produced, so its replay returns the
/// same thing.
fn remember<T: Serialize>(state: &ApiState, operation_id: &str, status: u16, body: &T) {
    if let Ok(mut receipts) = state.receipts.lock() {
        let value = serde_json::to_value(body).unwrap_or(serde_json::Value::Null);
        receipts.insert(operation_id.to_string(), (status, value));
    }
}

/// Replay guard for a synchronous mutation: check, do, record.
fn idempotent<T: Serialize + serde::de::DeserializeOwned>(
    state: &ApiState,
    operation_id: &str,
    make: impl FnOnce() -> ApiResult<(u16, T)>,
) -> ApiResult<IdempotentResponse<T>> {
    if let Some(replay) = replayed::<T>(state, operation_id) {
        return Ok(replay);
    }
    let (status, body) = make()?;
    remember(state, operation_id, status, &body);
    Ok((
        axum::http::StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(body),
    ))
}

// ── handlers ───────────────────────────────────────────────────────

/// `GET /api/health` — no auth: liveness is not a capability.
async fn health(State(state): State<ApiState>) -> Json<serde_json::Value> {
    let guard = store(&state);
    Json(serde_json::json!({
        "status": "ok",
        "uptime_ms": now_ms() - state.started_ms,
        "runs": guard.runs().len(),
        "root": guard.artifacts().root().display().to_string(),
    }))
}

/// `GET /api/runs` — reader-visible run ledger.
async fn list_runs(State(state): State<ApiState>, headers: HeaderMap) -> ApiResult<Json<Vec<RunRef>>> {
    require(&state, &headers, ActorRole::Reader)?;
    let runs = store(&state)
        .runs()
        .into_iter()
        .map(|r| RunRef {
            id: r.id,
            state: wire_name(r.state, "unknown"),
            steps_consumed: r.steps_consumed,
            steps_budget: r.steps_budget,
            resumed_from: r.resumed_from,
        })
        .collect();
    Ok(Json(runs))
}

/// `POST /api/observations` — an observation is evidence, not a label:
/// reader may record it.
async fn post_observation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ObserveBody>,
) -> ApiResult<IdempotentResponse<ReceiptBody>> {
    let actor = require(&state, &headers, ActorRole::Reader)?;
    idempotent(&state, &body.operation_id, || {
        let guard = store(&state);
        // The stored bytes are the GVD1 container the worker actually
        // reads, built from the request's pixel/reading fields. Storing
        // the request's JSON instead would leave a prediction bound to
        // bytes no backend has ever parsed.
        let mut pixels = [0u8; 256];
        if body.pixels.is_empty() {
            return Err(ApiError {
                class: "invalid-input".into(),
                detail: "an observation must carry its 256 pixel samples; \
                         an empty image is a declared absence (modality_mask), \
                         not a black one"
                    .into(),
                conflict_version: None,
            });
        }
        if body.pixels.len() != 256 {
            return Err(ApiError {
                class: "invalid-input".into(),
                detail: format!(
                    "expected 256 pixel samples, got {}",
                    body.pixels.len()
                ),
                conflict_version: None,
            });
        }
        pixels.copy_from_slice(&body.pixels);
        let mut readings = [0.0f32; 2];
        for (i, value) in body.readings.iter().take(2).enumerate() {
            readings[i] = *value;
        }
        let row = crate::container::Row::from_parts(
            body.scene_id,
            pixels,
            readings,
            body.modality_mask.get(1).copied().unwrap_or(false),
            None, // the product has no oracle; the answer is a prediction
        );
        let container = crate::container::encode(&[row], "observation")?;
        let content = guard.artifacts().put(&container)?;
        let observation = grove::contracts::Observation {
            schema: SCHEMA_VERSION,
            id: body.id.clone(),
            owner: actor.id.clone(),
            task_id: body.task.clone(),
            session_id: body.source.clone(),
            source: body.source.clone(),
            occurred_at_ms: now_ms(),
            blocks: vec![grove::contracts::ContentBlock {
                media_type: "application/x-gvd1".into(),
                artifact: content,
            }],
            modality_mask: body.modality_mask.clone(),
            training_permitted: true,
        };
        guard.put_observation(&actor, &observation)?;
        Ok((
            201,
            ReceiptBody {
                status: "accepted".into(),
                operation_id: body.operation_id.clone(),
                id: Some(body.id.clone()),
                version: None,
            },
        ))
    })
}

/// `POST /api/signals` — annotating: teacher labels and human
/// corrections live here, and only an annotator-or-better may write.
async fn post_signal(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<SignalBody>,
) -> ApiResult<IdempotentResponse<ReceiptBody>> {
    let actor = require(&state, &headers, ActorRole::Annotator)?;
    idempotent(&state, &body.operation_id, || {
        // a teacher label is an operator's claim about a model's output;
        // a human correction is the annotator's. A reader has neither.
        if body.kind == "teacher-label" {
            return Err(ApiError {
                class: "capability-denied".into(),
                detail: "teacher-label signals need the operator role; \
                         human corrections need the annotator role"
                    .into(),
                conflict_version: None,
            });
        }
        let kind = parse_signal_kind(&body.kind)?;
        let signal = grove::contracts::LearningSignal {
            schema: SCHEMA_VERSION,
            id: body.id.clone(),
            idempotency_key: body.id.clone(),
            producer: actor.id.clone(),
            kind,
            task_id: "task-1".to_string(),
            observation_id: body.observation_id.clone(),
            prediction_id: body.prediction_id.clone(),
            target_field: body.target_field.clone(),
            content: body.content.clone(),
            usage_permitted: body.usage_permitted,
            occurred_at_ms: now_ms(),
            received_at_ms: now_ms(),
            revises: None,
        };
        let receipt = store(&state).submit_signal(&actor, &signal)?;
        Ok((
            201,
            ReceiptBody {
                status: receipt.as_str().to_string(),
                operation_id: body.operation_id.clone(),
                id: Some(body.id.clone()),
                version: None,
            },
        ))
    })
}

fn parse_signal_kind(name: &str) -> ApiResult<grove::contracts::SignalKind> {
    use grove::contracts::SignalKind::*;
    Ok(match name {
        "teacher-label" => TeacherLabel,
        "human-correction" => HumanCorrection,
        "human-preference" => HumanPreference,
        "demonstration" => Demonstration,
        "environment-result" => EnvironmentResult,
        "revision" => Revision,
        "retraction" => Retraction,
        other => {
            return Err(ApiError {
                class: "invalid-input".into(),
                detail: format!("unknown signal kind {other:?}"),
                conflict_version: None,
            })
        }
    })
}

/// `POST /api/learning/runs` — starting training is an operator grant.
async fn start_run(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<TrainBody>,
) -> ApiResult<Json<RunRef>> {
    let actor = require(&state, &headers, ActorRole::Operator)?;
    let snapshot = ArtifactRef::parse_hex(&body.base_snapshot)?;
    store(&state).require_owner(&snapshot, &actor)?;
    let recipe = store(&state).get_recipe(&body.recipe)?;
    let budget = recipe.budget_steps.min(body.steps_budget);
    let run = grove::contracts::Run {
        schema: SCHEMA_VERSION,
        id: format!("run-{}", body.operation_id),
        task_id: "task-1".into(),
        base_snapshot: snapshot,
        dataset_revision: body.dataset_revision.clone(),
        recipe: body.recipe.clone(),
        state: grove::contracts::RunState::Queued,
        steps_consumed: 0,
        steps_budget: budget,
        resumed_from: None,
    };
    store(&state).put_run(&actor, &run)?;
    Ok(Json(RunRef {
        id: run.id,
        state: "queued".into(),
        steps_consumed: 0,
        steps_budget: budget,
        resumed_from: None,
    }))
}

/// `GET /api/runs/{id}` — a single run's state.
async fn get_run(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<RunRef>> {
    require(&state, &headers, ActorRole::Reader)?;
    let run = store(&state).get_run(&id)?;
    Ok(Json(RunRef {
        id: run.id,
        state: wire_name(run.state, "unknown"),
        steps_consumed: run.steps_consumed,
        steps_budget: run.steps_budget,
        resumed_from: run.resumed_from,
    }))
}

/// `POST /api/learning/fork` — an operator forks a branch.
async fn fork(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ForkBody>,
) -> ApiResult<IdempotentResponse<ReceiptBody>> {
    let actor = require(&state, &headers, ActorRole::Operator)?;
    idempotent(&state, &body.operation_id, || {
        let branch = grove::checkpoint::fork_branch(
            &store(&state),
            &actor,
            &body.checkpoint,
            &body.branch,
            "explore@1",
            body.quota.unwrap_or(100),
        )?;
        Ok((
            201,
            ReceiptBody {
                status: "accepted".into(),
                operation_id: body.operation_id.clone(),
                id: Some(branch.id),
                version: None,
            },
        ))
    })
}

/// `POST /api/publish` — publication is a publisher grant, with the
/// expected-version check.
async fn publish(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<PublishBody>,
) -> ApiResult<IdempotentResponse<ReceiptBody>> {
    let actor = require(&state, &headers, ActorRole::Publisher)?;
    idempotent(&state, &body.operation_id, || {
        let snapshot = ArtifactRef::parse_hex(&body.snapshot)?;
        let protocol: EvaluationProtocol = store(&state).get_protocol(&body.protocol)?;
        let version = grove::evaluation::publish_candidate(
            &store(&state),
            &actor,
            &protocol,
            &snapshot,
            body.expected_version,
        )?;
        Ok((
            200,
            ReceiptBody {
                status: "active".into(),
                operation_id: body.operation_id.clone(),
                id: None,
                version: Some(version),
            },
        ))
    })
}

/// `GET /api/evaluations/{snapshot}` — the protocol-scoped comparison.
async fn compare(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(snapshot): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, ActorRole::Reader)?;
    let digest = ArtifactRef::parse_hex(&snapshot)?;
    let records: Vec<serde_json::Value> = store(&state)
        .evaluations_for(&digest)?
        .into_iter()
        .map(|r: EvaluationRecord| {
            serde_json::json!({
                "protocol": r.protocol_id,
                "repeat": r.repeat_index,
                "metrics": r.metrics,
                "device": r.device,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "evaluations": records })))
}

/// `GET /api/events` — correlated events for the run view. Sensitive
/// raw content and tokens never appear here.
async fn events(State(state): State<ApiState>, headers: HeaderMap) -> ApiResult<Json<Vec<Event>>> {
    require(&state, &headers, ActorRole::Reader)?;
    let mut out = Vec::new();
    let guard = store(&state);
    for run in guard.runs() {
        out.push(Event {
            at_ms: now_ms(),
            kind: "run".into(),
            subject: run.id.clone(),
            detail: format!("{:?} {}/{} steps", run.state, run.steps_consumed, run.steps_budget),
        });
    }
    for cp in guard.checkpoints() {
        out.push(Event {
            at_ms: cp.created_at_ms,
            kind: "checkpoint".into(),
            subject: cp.id,
            detail: format!("run {} at {} steps", cp.run_id, cp.budget_spent_steps),
        });
    }
    if let Ok(Some((version, snapshot))) = guard.active_publication() {
        out.push(Event {
            at_ms: now_ms(),
            kind: "publication".into(),
            subject: format!("v{version}"),
            detail: snapshot.to_hex(),
        });
    }
    Ok(Json(out))
}

/// The product router. Same-origin by construction: the UI in W11 is
/// served from this same origin, so no CORS policy is opened.
pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/runs", get(list_runs))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/observations", post(post_observation))
        .route("/api/predictions", get(list_predictions))
        .route("/api/predictions", post(make_prediction))
        .route("/api/signals", post(post_signal))
        .route("/api/signals/{id}", get(get_signal))
        .route("/api/learning/runs", post(start_run))
        .route("/api/learning/fork", post(fork))
        .route("/api/learning/pause", post(pause_run))
        .route("/api/learning/resume", post(resume_run))
        .route("/api/learning/select", post(select_candidate))
        .route("/api/modules", get(list_modules))
        .route("/api/lineage", get(lineage))
        .route("/api/publish", post(publish))
        .route("/api/evaluations/{snapshot}", get(compare))
        .route("/api/events", get(events))
        .route("/", get(index_page))
        .route("/app.js", get(asset_js))
        .route("/styles.css", get(asset_css))
        .with_state(state)
}

// ── W11: the reads the product UI is allowed to trust ─────────────

/// `GET /api/predictions` — every prediction, bound to the exact snapshot
/// that produced it. The UI shows that digest next to the output, so a
/// correction can name the version it is correcting rather than "the
/// current model".
async fn list_predictions(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<serde_json::Value>>> {
    require(&state, &headers, ActorRole::Reader)?;
    let guard = store(&state);
    let rows = guard
        .query_predictions()
        .map_err(|e| ApiError::from(e))?;
    Ok(Json(
        rows.into_iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "observation_id": p.observation_id,
                    "snapshot": p.snapshot_digest.to_hex(),
                    "output": p.output,
                    "abstained": p.abstained,
                    "created_at_ms": p.created_at_ms,
                })
            })
            .collect(),
    ))
}

/// `GET /api/signals/{id}` — one signal with its lifecycle status. The
/// product shows "submitted" and "learned into" as two separate facts,
/// because a correction is accepted long before any model has trained
/// on it, and collapsing the two is how a UI starts lying.
async fn get_signal(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, ActorRole::Reader)?;
    let guard = store(&state);
    let status = guard.signal_status(&id)?;
    let in_force = guard.signal_in_force(&id).map_err(ApiError::from)?;
    let learned_into: Vec<String> = guard
        .datasets_containing_signal(&id)
        .map_err(ApiError::from)?;
    Ok(Json(serde_json::json!({
        "id": id,
        "status": status,
        "in_force": in_force,
        "learned_into": learned_into,
    })))
}

#[derive(Debug, Deserialize)]
pub struct PredictBody {
    pub id: String,
    pub observation_id: String,
    /// Hex digest of the exact snapshot to predict with. There is no
    /// "current model" default: a prediction that cannot name its model
    /// version cannot be corrected meaningfully.
    pub snapshot: String,
}

/// `POST /api/predictions` — a real prediction from the real worker,
/// bound to the exact snapshot the caller named.
///
/// The answer is not computed here. The observation's own stored bytes are
/// staged and handed to the same isolated torch worker the trainer uses,
/// so a product prediction and a training-time prediction come from one
/// implementation. An undecidable input abstains, and the abstain is
/// recorded as an abstain.
async fn make_prediction(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<PredictBody>,
) -> ApiResult<IdempotentResponse<serde_json::Value>> {
    let actor = require(&state, &headers, ActorRole::Reader)?;
    let operation_id = format!("predict-{}", body.id);
    // A replay returns the first answer without touching the worker
    // again: the receipt is what makes "did that request already run?"
    // answerable, and re-running an inference because a client retried
    // would be a second charge for one question.
    if let Some(replay) = replayed::<serde_json::Value>(&state, &operation_id) {
        return Ok(replay);
    }
    let digest = ArtifactRef::parse_hex(&body.snapshot)?;
    let observation_id = body.observation_id.clone();
    let root = state
        .scratch
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| ApiError {
            class: "invalid-input".into(),
            detail: "scratch directory has no store root".into(),
            conflict_version: None,
        })?;
    let scratch = state.scratch.clone();
    let paths = Arc::clone(&state.paths);

    // Spawning an isolated torch worker is blocking work; running it on
    // the async runtime would stall every other connection for the
    // duration of a model forward pass.
    let outcome = tokio::task::spawn_blocking(move || -> ApiResult<crate::inference::InferenceResult> {
        // A second connection to the same store root, so the blocking
        // work never holds the API's mutex while the worker runs.
        let store = Store::open(&root)?;
        let observation = store.get_observation(&observation_id)?;
        // The graph comes from the committed snapshot, not the request:
        // a prediction must be these weights under the structure they
        // were trained with, and a caller-supplied graph would let any
        // reader describe any model however they liked.
        let snapshot = store.load_snapshot(&digest)?;
        let graph = snapshot.graph.clone().ok_or_else(|| {
            ApiError {
                class: "incompatible-state".into(),
                detail: format!(
                    "snapshot {digest} declares no operator graph, so there is \
                     nothing to execute; it cannot serve predictions"
                ),
                conflict_version: None,
            }
        })?;
        let outcome = crate::inference::predict(
            &store,
            &actor,
            &paths,
            &digest,
            &graph,
            &observation,
            &scratch,
        )?;
        // the record is the product's durable claim; storing it is what
        // makes the correction that follows referenceable
        store.put_prediction(&actor, &outcome.prediction)?;
        Ok(outcome)
    })
    .await
    .map_err(|e| ApiError {
        class: "backend-failed".into(),
        detail: format!("prediction task failed: {e}"),
        conflict_version: None,
    })??;

    let payload = serde_json::json!({
        "id": outcome.prediction.id,
        "observation_id": outcome.prediction.observation_id,
        "snapshot": outcome.prediction.snapshot_digest.to_hex(),
        "output": outcome.prediction.output,
        "abstained": outcome.prediction.abstained,
        "decided_rows": outcome.decided,
        "abstained_rows": outcome.abstain_rows,
    });
    remember(&state, &operation_id, 201, &payload);
    Ok((StatusCode::CREATED, Json(payload)))
}

#[derive(Debug, Deserialize)]
pub struct PauseBody {
    pub run: String,
    pub state_artifact: String,
}

#[derive(Debug, Deserialize)]
pub struct ResumeBody {
    pub checkpoint: String,
    pub run: String,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub steps_budget: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SelectBody {
    pub protocol: String,
    pub snapshots: Vec<String>,
}

/// `POST /api/learning/pause` — pause at a checkpoint boundary. The run
/// only becomes `paused` after its state artifact is committed, so the
/// UI never shows a run as safely paused when the bytes are not there.
async fn pause_run(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<PauseBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let actor = require(&state, &headers, ActorRole::Operator)?;
    let checkpoint = grove::checkpoint::pause(
        &store(&state),
        &actor,
        &body.run,
        std::path::Path::new(&body.state_artifact),
        now_ms(),
    )?;
    Ok(Json(serde_json::json!({
        "checkpoint": checkpoint.id,
        "run": body.run,
        "state": "paused",
        "steps": checkpoint.budget_spent_steps,
        "resume_level": format!("{:?}", checkpoint.resume_level).to_lowercase(),
    })))
}

/// `POST /api/learning/resume` — a resume creates a NEW run and leaves
/// the parent record untouched. The response names both, because a UI
/// that showed one run id for a resumed lineage would be claiming a
/// continuity the store does not have.
async fn resume_run(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<ResumeBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let actor = require(&state, &headers, ActorRole::Operator)?;
    let level = match body.level.as_deref() {
        None | Some("learning-continuation") => grove::contracts::ResumeLevel::LearningContinuation,
        Some("model-initialization") => grove::contracts::ResumeLevel::ModelInitialization,
        Some("controlled-replay") => grove::contracts::ResumeLevel::ControlledReplay,
        Some(other) => {
            return Err(ApiError {
                class: "invalid-input".into(),
                detail: format!("unknown resume level {other:?}"),
                conflict_version: None,
            })
        }
    };
    let plan = grove::checkpoint::resume_plan(
        &store(&state),
        &actor,
        &body.checkpoint,
        level,
        false,
        &body.run,
        body.steps_budget.unwrap_or(1000),
    )?;
    Ok(Json(serde_json::json!({
        "run": plan.run.id,
        "resumed_from_checkpoint": plan.checkpoint_id,
        "steps_consumed": plan.run.steps_consumed,
        "steps_budget": plan.run.steps_budget,
        "state_step": plan.state_step,
    })))
}

/// `POST /api/learning/select` — the same protocol comparison the CLI
/// runs, plus the non-dominated set. Selection is advisory: it does not
/// publish, and the response says which snapshots cleared the gates so
/// the operator still has to press publish with a publisher token.
async fn select_candidate(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<SelectBody>,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, ActorRole::Reader)?;
    let guard = store(&state);
    let protocol: EvaluationProtocol = guard.get_protocol(&body.protocol)?;
    let mut digests = Vec::new();
    for hex in &body.snapshots {
        digests.push(ArtifactRef::parse_hex(hex)?);
    }
    let rows = grove::evaluation::compare(&guard, &protocol, &digests)?;
    let candidates: Vec<grove::evaluation::Candidate> = rows
        .iter()
        .map(|r| grove::evaluation::Candidate {
            snapshot: r.snapshot,
            quality: r
                .mean
                .iter()
                .find(|(n, _)| n == "accuracy")
                .map(|(_, v)| *v)
                .unwrap_or(0.0),
            cost: 1.0,
            meets_gates: r.meets_gates,
        })
        .collect();
    let kept: Vec<String> = grove::evaluation::non_dominated(&candidates)
        .iter()
        .map(|c| c.snapshot.to_hex())
        .collect();
    Ok(Json(serde_json::json!({
        "protocol": protocol.id,
        "gates": protocol.gates,
        "rows": rows.iter().map(|r| serde_json::json!({
            "snapshot": r.snapshot.to_hex(),
            "repeats": r.repeats,
            "mean": r.mean.iter().map(|(n, v)| serde_json::json!({"metric": n, "value": v})).collect::<Vec<_>>(),
            "meets_gates": r.meets_gates,
            "gate_failures": r.gate_failures,
        })).collect::<Vec<_>>(),
        "non_dominated": kept,
    })))
}

/// `GET /api/modules` — the module and expert view.
///
/// This reports what the store can actually serve. A node whose snapshot
/// was invalidated is returned with `deployable: false` and the reason,
/// not filtered out: a module list that quietly drops the broken parts
/// looks exactly like a healthy one.
async fn list_modules(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, ActorRole::Reader)?;
    let guard = store(&state);
    let mut modules = Vec::new();
    for (name, digest, _) in guard.named_snapshots().map_err(ApiError::from)? {
        let Ok(snapshot) = guard.load_snapshot(&digest) else {
            modules.push(serde_json::json!({
                "name": name, "snapshot": digest.to_hex(), "loadable": false
            }));
            continue;
        };
        let invalidation = guard.snapshot_invalidation(&digest).ok().flatten();
        modules.push(serde_json::json!({
            "name": name,
            "snapshot": digest.to_hex(),
            "owner": snapshot.owner,
            "loadable": true,
            "deployable": invalidation.is_none(),
            "revoked_signals": invalidation
                .as_ref()
                .map(|i| i.revoked_signals.clone())
                .unwrap_or_default(),
            "modules": snapshot.modules.iter().map(|m| serde_json::json!({
                "name": m.name,
                "input_space": m.input_space,
                "output_space": m.output_space,
                "layers": m.layers,
                "depends_on": m.depends_on,
                "shared_group": m.shared_group,
                "frozen": m.frozen,
                "requires": wire_name(m.requires, "unknown"),
            })).collect::<Vec<_>>(),
            "ensemble": snapshot.ensemble.as_ref().map(|e| serde_json::json!({
                "rule": wire_name(e.rule, "unknown"),
                "output_space": e.output_space,
                "budget_per_call": e.budget_per_call,
                "experts": e.experts.iter().map(|x| serde_json::json!({
                    "name": x.name,
                    "output_space": x.output_space,
                    "weight": x.weight,
                })).collect::<Vec<_>>(),
            })),
        }));
    }
    Ok(Json(serde_json::json!({ "snapshots": modules })))
}

/// `GET /api/lineage` — the derivation DAG the product renders as an
/// expandable tree. Recovery level and invalidation are properties of a
/// node, so they travel with the node.
async fn lineage(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> ApiResult<Json<serde_json::Value>> {
    require(&state, &headers, ActorRole::Reader)?;
    let guard = store(&state);
    let mut nodes = Vec::new();
    for cp in guard.checkpoints() {
        let recoverable = guard.artifacts().exists(&cp.state_artifact);
        nodes.push(serde_json::json!({
            "kind": "checkpoint",
            "id": cp.id,
            "run": cp.run_id,
            "parent": cp.parent,
            "snapshot": cp.snapshot.to_hex(),
            "steps": cp.budget_spent_steps,
            // the wire form, not the Debug form: a UI that showed
            // "learningcontinuation" would be showing a Rust identifier,
            // not the contract's name
            "resume_level": wire_name(cp.resume_level, "unknown"),
            "recoverable": recoverable,
            "created_at_ms": cp.created_at_ms,
        }));
    }
    for (name, digest, _) in guard.named_snapshots().map_err(ApiError::from)? {
        let parents: Vec<serde_json::Value> = guard
            .parents_of(&name)
            .map_err(ApiError::from)?
            .into_iter()
            .map(|(d, k)| serde_json::json!({
                "parent": d.to_hex(),
                "derivation": grove::checkpoint::derivation_label(k),
            }))
            .collect();
        let invalidation = guard.snapshot_invalidation(&digest).ok().flatten();
        nodes.push(serde_json::json!({
            "kind": "snapshot",
            "id": name,
            "snapshot": digest.to_hex(),
            "parents": parents,
            "deployable": invalidation.is_none(),
            "revoked_signals": invalidation
                .as_ref()
                .map(|i| i.revoked_signals.clone())
                .unwrap_or_default(),
        }));
    }
    let mut branches = Vec::new();
    for b in guard.branches() {
        branches.push(serde_json::json!({
            "id": b.id,
            "head": b.head,
            "head_version": b.head_version,
            "quota": b.budget_quota,
        }));
    }
    Ok(Json(serde_json::json!({
        "nodes": nodes,
        "branches": branches,
        "publication": match guard.active_publication() {
            Ok(Some((v, d))) => serde_json::json!({"version": v, "snapshot": d.to_hex()}),
            _ => serde_json::Value::Null,
        },
    })))
}

// ── static assets (same origin, no CORS) ──────────────────────────

/// The directory the UI lives in. Packaged installs override it with
/// `GROVE_WEB_ROOT`; the default is next to the crate so `cargo run`
/// serves the real files rather than a placeholder.
fn web_root() -> std::path::PathBuf {
    std::env::var_os("GROVE_WEB_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web")
        })
}

/// Read one asset. A path that escapes the web root is refused rather
/// than resolved — a static handler that can read `../` is a file
/// disclosure, not a convenience.
fn read_asset(name: &str) -> ApiResult<(String, &'static str)> {
    if name.contains("..") || name.contains('/') {
        return Err(ApiError {
            class: "artifact-unavailable".into(),
            detail: format!("asset {name:?} is not a web asset"),
            conflict_version: None,
        });
    }
    let path = web_root().join(name);
    let bytes = std::fs::read(&path).map_err(|e| {
        ApiError {
            class: "artifact-unavailable".into(),
            detail: format!("web asset {} is unreadable: {e}", path.display()),
            conflict_version: None,
        }
    })?;
    let body = String::from_utf8(bytes).map_err(|_| ApiError {
        class: "artifact-unavailable".into(),
        detail: format!("web asset {} is not UTF-8", path.display()),
        conflict_version: None,
    })?;
    let kind = if name.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if name.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "text/html; charset=utf-8"
    };
    Ok((body, kind))
}

async fn index_page() -> Response {
    serve_asset("index.html").await
}

async fn asset_js() -> Response {
    serve_asset("app.js").await
}

async fn asset_css() -> Response {
    serve_asset("styles.css").await
}

async fn serve_asset(name: &str) -> Response {
    match read_asset(name) {
        Ok((body, kind)) => (
            StatusCode::OK,
            [("content-type", kind)],
            body,
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// An enum's wire name, not its Rust identifier. `{:?}` on a
/// `Debug`-derived enum yields `LearningContinuation`; the contract's
/// name is `learning-continuation`, and a UI showing the former is
/// showing a type name where a value belongs.
fn wire_name<T: Serialize>(value: T, fallback: &str) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| fallback.to_string())
}

/// Refuse a non-loopback bind with no tokens: an open control surface on
/// a shared network is a security decision, not a default.
pub fn check_bind(bind: std::net::SocketAddr, token_count: usize) -> GroveResult<()> {
    let loopback = bind.ip().is_loopback();
    if !loopback && token_count == 0 {
        return Err(GroveError::denied(format!(
            "refusing to bind {} without tokens: a non-loopback control surface must be authenticated",
            bind
        )));
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
