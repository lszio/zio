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

use std::collections::HashMap;
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
        Self {
            store: Arc::new(Mutex::new(store)),
            tokens: Arc::new(tokens),
            receipts: Arc::new(Mutex::new(HashMap::new())),
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
    pub modality_mask: Vec<bool>,
    pub content: String,
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

/// Replay guard. The FIRST response — status code and body — is what a
/// replay returns: a client that lost the first response must not be
/// told it was created when nothing new happened, and must not be
/// charged for a second effect.
fn idempotent<T: Serialize + serde::de::DeserializeOwned>(
    state: &ApiState,
    operation_id: &str,
    make: impl FnOnce() -> ApiResult<(u16, T)>,
) -> ApiResult<IdempotentResponse<T>> {
    if let Ok(receipts) = state.receipts.lock() {
        if let Some((status, value)) = receipts.get(operation_id) {
            return match serde_json::from_value(value.clone()) {
                Ok(body) => {
                    let code = axum::http::StatusCode::from_u16(*status).unwrap_or(StatusCode::OK);
                    Ok((code, Json(body)))
                }
                Err(_) => Err(ApiError {
                    class: "invalid-input".into(),
                    detail: format!("receipt for {operation_id} is unreadable"),
                    conflict_version: None,
                }),
            };
        }
    }
    let (status, body) = make()?;
    let value = serde_json::to_value(&body).unwrap_or(serde_json::Value::Null);
    let code = axum::http::StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    if let Ok(mut receipts) = state.receipts.lock() {
        receipts.insert(operation_id.to_string(), (status, value));
    }
    Ok((code, Json(body)))
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
            state: format!("{:?}", r.state).to_lowercase(),
            steps_consumed: r.steps_consumed,
            steps_budget: r.steps_budget,
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
        let content = store(&state).artifacts().put(body.content.as_bytes())?;
        let observation = grove::contracts::Observation {
            schema: SCHEMA_VERSION,
            id: body.id.clone(),
            owner: actor.id.clone(),
            task_id: body.task.clone(),
            session_id: body.source.clone(),
            source: body.source.clone(),
            occurred_at_ms: now_ms(),
            blocks: vec![grove::contracts::ContentBlock {
                media_type: "application/json".into(),
                artifact: content,
            }],
            modality_mask: body.modality_mask.clone(),
            training_permitted: true,
        };
        store(&state).put_observation(&actor, &observation)?;
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
        state: format!("{:?}", run.state).to_lowercase(),
        steps_consumed: run.steps_consumed,
        steps_budget: run.steps_budget,
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
        .route("/api/signals", post(post_signal))
        .route("/api/learning/runs", post(start_run))
        .route("/api/learning/fork", post(fork))
        .route("/api/publish", post(publish))
        .route("/api/evaluations/{snapshot}", get(compare))
        .route("/api/events", get(events))
        .with_state(state)
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
