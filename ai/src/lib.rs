//! zio-ai — host-side AI capability protocols for zio (ADR-016).
//!
//! The crate is a capability socket, not intelligence: it defines the
//! [`LlmHost`] / [`EmbedHost`] traits, a deterministic mock (record/replay,
//! see [`mock`]), an optional OpenAI-compatible HTTP implementation (the
//! `http` feature, see [`http`]), and [`install`] — the external attach
//! point that registers the `llm-complete` / `embed` native bindings into
//! an `EvalContext`.
//!
//! core stays untouched (ADR-009): zio-ai depends on zio-core, never the
//! reverse. Both bindings are always registered; without a host installed
//! a call fails with the stable `capability-denied:` prefix (anchored by
//! contract tests), which is why `install` takes `Option<Arc<dyn _>>`.

use std::fmt;
use std::sync::Arc;

use zio_core::context::{EvalContext, EvalEngine};
use zio_core::error::EvalError;
use zio_core::im::Vector;
use zio_core::value::{NativeFn, Value};

pub mod mock;

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

/// Host-provided text completion capability (ADR-016).
///
/// The host is an untrusted external process by construction: its output
/// only ever re-enters zio through the reader → whitelist → eval gates,
/// so no host can smuggle authority into the evaluator.
pub trait LlmHost {
    fn complete(&self, prompt: &str, opts: &LlmOptions) -> Result<String, HostError>;
}

/// Host-provided text embedding capability (ADR-016).
///
/// Vectors are advisory data (semantic bridge for natural-language tasks);
/// nothing in the learning loop depends on them.
pub trait EmbedHost {
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

/// Attach AI hosts to an evaluation context by registering the
/// `llm-complete` / `embed` native bindings.
///
/// This is the only wiring zio-ai performs: hosts are captured by the
/// binding closures, so `EvalContext` gains no fields and zio-core gains
/// no dependency. Both bindings are always registered — pass `None` when
/// a capability is not provisioned and calls fail with the stable
/// `capability-denied:` prefix instead of a missing-symbol error.
pub fn install(
    ctx: &EvalContext,
    llm: Option<Arc<dyn LlmHost>>,
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
    host: &Option<Arc<dyn LlmHost>>,
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
    let response = host
        .complete(&prompt, &opts)
        .map_err(|e| EvalError::custom(format!("llm-complete: {e}")))?;
    Ok(Value::String(response))
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
