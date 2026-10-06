//! Native model/session transport adapter for Zio applications.
//!
//! Loom is a capability socket, not intelligence and not a product. It
//! owns the *contract* a model provider has to speak
//! ([`harness`]: messages, tool calls, sessions, budgets, cancellation)
//! and the transports that carry it: a deterministic mock
//! ([`mock`]), an OpenAI-compatible HTTP client behind the `http` feature
//! ([`http`]), and the teacher capability protocol ([`teacher`]).
//!
//! It does not own evaluation, approval, or publication: those belong to
//! the application that embeds it, which is why [`harness::Session`] takes
//! its policy by injection rather than deciding any of it.
//!
//! Neither zio-core nor the language CLI depends on this adapter.
//! Applications explicitly call `install` to attach `llm-complete` / `embed`;
//! an unprovisioned attached capability fails with `capability-denied:`.
//! This native adapter is not the Zio Loom library under `libs/loom/`.

use std::fmt;
use std::sync::Arc;

use zio_core::context::{EvalContext, EvalEngine};
use zio_core::error::EvalError;
use zio_core::im::Vector;
use zio_core::value::{NativeFn, Value};

pub mod harness;
pub mod mock;

/// The harness types a consumer names in its own signatures, re-exported
/// so the common path is one import. `LlmHost` / `EmbedHost` are
/// *defined* here, so they need no re-export; `ModelHost` and the
/// message types live in [`harness`] and are surfaced here because a
/// consumer implementing a provider should not have to know that.
pub use harness::{
    Budget, ChatMessage, ChatRequest, ChatResponse, ModelHost, Options, Session, Tools,
    ToolCall, Usage,
};

pub mod teacher;

#[cfg(feature = "http")]
pub mod http;

// ── Host protocols ──────────────────────────────────────────────

/// Options for a completion call. `Default` yields provider defaults.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
}

/// A text-completion provider.
///
/// This is the *narrow* port: one prompt in, one string out. It exists
/// for embedders that genuinely only need completion, and
/// [`harness::ModelHost`] is implemented for every provider in this crate
/// so the two never diverge. The Zio `llm-complete` binding speaks this,
/// backed by a session, so the narrow port is a view over the structured
/// contract rather than a second transport.
pub trait LlmHost: Send + Sync {
    fn complete(&self, prompt: &str, opts: &LlmOptions) -> Result<String, HostError>;
}

/// Host-provided text embedding capability (ADR-016).
///
/// Vectors are advisory data (semantic bridge for natural-language tasks);
/// nothing in the learning loop depends on them.
pub trait EmbedHost: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, HostError>;
}

// ── Errors ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostErrorKind {
    /// The call exceeded its time budget.
    Timeout,
    /// The remote endpoint answered with a non-success status.
    Http,
    /// The answer arrived but could not be interpreted.
    Protocol,
    /// The transport failed before an answer (connection, DNS, TLS).
    Transport,
    /// The host itself is misconfigured (bad URL, missing model, ...).
    Config,
    /// A replay mock has no recording for the requested call — fail-fast
    /// by contract; contract tests must never touch the network.
    ReplayMiss,
    /// Local I/O failure (reading or writing recordings).
    Io,
    /// The call would exceed a declared budget. The provider was not
    /// called: an over-budget call spends nothing.
    Budget,
    /// The session was cancelled. No further provider call is made, so a
    /// cancellation cannot leave a side effect behind.
    Cancelled,
    /// The provider did not report a cost and none is known. The
    /// reservation stays held; an unknown cost is not zero.
    UnknownCost,
}

impl HostErrorKind {
    fn label(self) -> &'static str {
        match self {
            HostErrorKind::Timeout => "timeout",
            HostErrorKind::Http => "http",
            HostErrorKind::Protocol => "protocol",
            HostErrorKind::Transport => "transport",
            HostErrorKind::Config => "config",
            HostErrorKind::ReplayMiss => "replay-miss",
            HostErrorKind::Io => "io",
            HostErrorKind::Budget => "budget",
            HostErrorKind::Cancelled => "cancelled",
            HostErrorKind::UnknownCost => "unknown-cost",
        }
    }
}

#[derive(Debug)]
pub struct HostError {
    pub kind: HostErrorKind,
    pub message: String,
}

impl HostError {
    pub fn new(kind: HostErrorKind, message: impl Into<String>) -> Self {
        HostError { kind, message: message.into() }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.label(), self.message)
    }
}

impl std::error::Error for HostError {}

/// FNV-1a 64-bit — a tiny, fully deterministic hash for replay keys.
/// std's DefaultHasher makes no cross-version stability promise; the
/// replay contract does.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// ── External attach (ADR-016) ───────────────────────────────────

/// Attach a model host and an embedding host to an evaluation context by
/// registering the `llm-complete` / `embed` native bindings.
///
/// This is the only wiring Loom performs: hosts are captured by the
/// binding closures, so `EvalContext` gains no fields and zio-core gains
/// no dependency. Both bindings are always registered — pass `None` when
/// a capability is not provisioned and calls fail with the stable
/// `capability-denied:` prefix instead of a missing-symbol error.
///
/// `llm-complete` is an *extension* binding Loom provides, not a language
/// primitive: core has no knowledge of it, and it is registered here from
/// outside.
pub fn install(
    ctx: &EvalContext,
    llm: Option<Arc<dyn ModelHost>>,
    embed: Option<Arc<dyn EmbedHost>>,
) {
    ctx.env.set(
        "llm-complete".into(),
        Value::NativeFunction(NativeFn::new(
            "llm-complete",
            move |args: Vector<Value>, _engine: &dyn EvalEngine| {
                llm_complete_fn(&llm, &args)
            },
        )),
    );
    ctx.env.set(
        "embed".into(),
        Value::NativeFunction(NativeFn::new(
            "embed",
            move |args: Vector<Value>, _engine: &dyn EvalEngine| embed_fn(&embed, &args),
        )),
    );
}

fn llm_complete_fn(
    host: &Option<Arc<dyn ModelHost>>,
    args: &Vector<Value>,
) -> Result<Value, EvalError> {
    let Some(host) = host else {
        return Err(EvalError::custom(
            "capability-denied: llm host not installed (llm-complete)",
        ));
    };
    let prompt = match args.get(0) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(EvalError::custom(format!(
                "llm-complete: prompt must be a string, got {}",
                value_kind(other)
            )));
        }
        None => return Err(EvalError::custom("llm-complete: expected a prompt string")),
    };
    let opts = parse_llm_options(args)?;
    // The text binding is a view over a session: one user turn, no
    // tools, answered by the same structured contract every other
    // transport uses. There is no second, text-only path.
    let session = harness::Session::new(Arc::clone(host));
    let message = session
        .send_with(
            &prompt,
            &harness::Tools::none(),
            harness::Options {
                temperature: opts.temperature,
                stop: opts.stop.clone(),
            },
        )
        .map_err(|e| EvalError::custom(format!("llm-complete: {e}")))?;
    match message.content {
        serde_json::Value::String(text) => Ok(Value::String(text)),
        other => Ok(Value::String(other.to_string())),
    }
}

/// Adapt a text-only provider onto the structured contract, so an
/// embedder that has one cannot also end up with a second transport.
pub struct SessionLlmHost {
    inner: Arc<dyn LlmHost>,
}

impl SessionLlmHost {
    pub fn new(inner: Arc<dyn LlmHost>) -> Self {
        SessionLlmHost { inner }
    }
}

impl harness::ModelHost for SessionLlmHost {
    fn respond(
        &self,
        request: &harness::ChatRequest,
        budget: &harness::Budget,
    ) -> Result<harness::ChatResponse, HostError> {
        let last = request.messages.last().cloned().unwrap_or(harness::ChatMessage {
            role: "user".into(),
            content: serde_json::Value::String(String::new()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        });
        let prompt = last.content.as_str().unwrap_or_default().to_string();
        let text = self
            .inner
            .complete(&prompt, &LlmOptions::default())
            .map_err(|e| {
                // A failure spends nothing, so the reservation goes back.
                let _ = budget;
                e
            })?;
        Ok(harness::ChatResponse {
            request_id: request.request_id.clone(),
            message: harness::ChatMessage {
                role: "assistant".into(),
                content: serde_json::Value::String(text),
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            usage: harness::Usage::default(),
            finish_reason: "stop".into(),
        })
    }
}

/// `(llm-complete prompt & :temperature x :max-tokens n :stop s)`
fn parse_llm_options(args: &Vector<Value>) -> Result<LlmOptions, EvalError> {
    let mut opts = LlmOptions::default();
    let mut i = 1;
    while i < args.len() {
        let key = match args.get(i) {
            Some(Value::Keyword(k)) => k.clone(),
            _ => {
                return Err(EvalError::custom(
                    "llm-complete: options must be :keyword value pairs",
                ));
            }
        };
        let value = args
            .get(i + 1)
            .ok_or_else(|| EvalError::custom(format!("llm-complete: missing value for :{key}")))?;
        match key.as_str() {
            "temperature" => opts.temperature = Some(as_f64(value, ":temperature")?),
            "max-tokens" => opts.max_tokens = Some(as_u32(value, ":max-tokens")?),
            "stop" => opts.stop.push(as_string(value, ":stop")?),
            other => {
                return Err(EvalError::custom(format!(
                    "llm-complete: unknown option :{other}"
                )));
            }
        }
        i += 2;
    }
    Ok(opts)
}

fn embed_fn(
    host: &Option<Arc<dyn EmbedHost>>,
    args: &Vector<Value>,
) -> Result<Value, EvalError> {
    let Some(host) = host else {
        return Err(EvalError::custom(
            "capability-denied: embed host not installed (embed)",
        ));
    };
    let texts: Vec<String> = match args.get(0) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Vector(vs)) => vs
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.clone()),
                other => Err(EvalError::custom(format!(
                    "embed: texts must be strings, got {}",
                    value_kind(other)
                ))),
            })
            .collect::<Result<Vec<String>, EvalError>>()?,
        _ => {
            return Err(EvalError::custom(
                "embed: expected a string or a vector of strings",
            ));
        }
    };
    if texts.is_empty() {
        return Err(EvalError::custom("embed: expected at least one text"));
    }
    let vectors = host
        .embed(&texts)
        .map_err(|e| EvalError::custom(format!("embed: {e}")))?;
    if vectors.len() != texts.len() {
        return Err(EvalError::custom(format!(
            "embed: host returned {} vectors for {} texts",
            vectors.len(),
            texts.len()
        )));
    }
    let rows: Vector<Value> = vectors
        .iter()
        .map(|row| Value::Vector(row.iter().copied().map(Value::Float).collect()))
        .collect();
    Ok(Value::Vector(rows))
}

// ── Small value helpers ─────────────────────────────────────────

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::List(_) => "list",
        Value::Vector(_) => "vector",
        Value::Map(_) => "map",
        Value::Function(_) | Value::NativeFunction(_) | Value::Macro(_) => "function",
        Value::Object(_) => "object",
        _ => "value",
    }
}

fn as_f64(value: &Value, what: &str) -> Result<f64, EvalError> {
    match value {
        Value::Integer(i) => Ok(*i as f64),
        Value::Float(f) => Ok(*f),
        other => Err(EvalError::custom(format!(
            "llm-complete: {what} must be a number, got {}",
            value_kind(other)
        ))),
    }
}

fn as_u32(value: &Value, what: &str) -> Result<u32, EvalError> {
    match value {
        Value::Integer(i) if *i >= 0 && *i <= u32::MAX as i64 => Ok(*i as u32),
        other => Err(EvalError::custom(format!(
            "llm-complete: {what} must be a non-negative integer, got {}",
            value_kind(other)
        ))),
    }
}

fn as_string(value: &Value, what: &str) -> Result<String, EvalError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        other => Err(EvalError::custom(format!(
            "llm-complete: {what} must be a string, got {}",
            value_kind(other)
        ))),
    }
}
