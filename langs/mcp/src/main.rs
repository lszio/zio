//! Zio MCP basic service (ZIO-009): stdio JSON-RPC 2.0 over the harness
//! (contract: docs/superpowers/specs/2026-10-16-zio-execution-harness-contract.md §5–§15).
//!
//! Wire format: one JSON-RPC 2.0 request per stdin line, one response
//! line per request on stdout, flushed per response. stdout carries the
//! protocol only — the harness captures `print`/`println` into the
//! result envelope `output[]`, so no harness output leaks to real stdout
//! (contract §7/§13).
//!
//! Sessions live server-side, keyed by the harness-generated `SessionId`,
//! and survive transport reconnects (contract §2). Remote restart is
//! explicitly out of scope for this slice (contract §13; ZIO-012).
//!
//! Slice 1 caveats:
//! - line-delimited JSON, not Content-Length framing;
//! - synchronous, one request at a time (no async I/O);
//! - `ValueRef`: integers as decimal strings (bigint precision, contract
//!   §6 row 6); every other value is a to-string fallback;
//! - `CapabilitySet` in zio-core has no union constructor yet, so
//!   `create_session` grants a single capability or none and rejects a
//!   request naming several true flags (never silently under-grants).

use std::collections::HashMap;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value as Json, json};

use zio_core::error::EvalError;
use zio_core::harness::Budgets;
use zio_core::span::Span;
use zio_core::value::Value;
use zio_core::{
    Capability, CapabilitySet, Diagnostic, ObserverEvent, RunResult, Session, SessionDefaults,
};

// JSON-RPC 2.0 standard codes.
const CODE_PARSE_ERROR: i64 = -32700;
const CODE_INVALID_REQUEST: i64 = -32600;
const CODE_METHOD_NOT_FOUND: i64 = -32601;
const CODE_INVALID_PARAMS: i64 = -32602;
/// Server-level error for a session the server does not know. Harness
/// errors travel inside result envelopes; the typed harness-error codes
/// (-32001 CapabilityDenied, -32002 Cancelled, -32003 TimedOut, -32004
/// LoadError) are reserved for when a typed error surfaces at the
/// request level instead.
const CODE_SESSION_NOT_FOUND: i64 = -32000;

/// Server state (contract §2/§13): sessions keyed by harness-generated
/// `SessionId`. Wrapped in `Arc<Mutex<…>>` so a later async slice can
/// move session calls to worker threads; Slice 1 is single-threaded and
/// the mutex is uncontended.
#[derive(Default)]
struct Server {
    sessions: Mutex<HashMap<u64, Arc<Mutex<Session>>>>,
}

fn main() {
    let server = Server::default();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break; // EOF or fatal stdin error: nothing left to serve.
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let response = server.handle(line);
        if writeln!(out, "{response}").is_err() || out.flush().is_err() {
            break; // Client went away; stop writing to its pipe.
        }
    }
}

impl Server {
    /// Dispatch one JSON-RPC request line to its method; every request
    /// earns exactly one response line.
    fn handle(&self, line: &str) -> Json {
        let request: Json = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => return error_response(Json::Null, CODE_PARSE_ERROR, "parse error"),
        };
        let id = request.get("id").cloned().unwrap_or(Json::Null);
        let Some(method) = request.get("method").and_then(Json::as_str) else {
            return error_response(id, CODE_INVALID_REQUEST, "invalid request");
        };
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        match method {
            "create_session" => self.create_session(&params, id),
            "compile" => self.compile(&params, id),
            "eval" => self.eval(&params, id),
            "run" => self.run(&params, id),
            "cancel" => self.cancel(&params, id),
            "close_session" => self.close_session(&params, id),
            _ => error_response(id, CODE_METHOD_NOT_FOUND, "method not found"),
        }
    }

    /// Create a session with the requested capabilities and budgets
    /// (contract §2/§4/§8). The session id comes from the harness, and
    /// the server registers it by that id.
    fn create_session(&self, params: &Json, id: Json) -> Json {
        let capabilities = match parse_capabilities(params) {
            Ok(set) => set,
            Err(message) => return error_response(id, CODE_INVALID_PARAMS, message),
        };
        let budgets = match parse_budgets(params) {
            Ok(budgets) => budgets,
            Err(message) => return error_response(id, CODE_INVALID_PARAMS, message),
        };
        let session = Session::new(capabilities, SessionDefaults::with_budgets(budgets));
        let session_id = session.id.0;
        self.sessions
            .lock()
            .insert(session_id, Arc::new(Mutex::new(session)));
        result_response(id, json!({ "session_id": session_id }))
    }

    /// Compile source without executing it (contract §5).
    fn compile(&self, params: &Json, id: Json) -> Json {
        let session = match self.with_session(params, id.clone()) {
            Ok(session) => session,
            Err(response) => return response,
        };
        let Ok((name, source)) = name_and_source(params) else {
            return error_response(
                id,
                CODE_INVALID_PARAMS,
                "invalid params: name and source are strings",
            );
        };
        let result = session.lock().compile(&name, source);
        result_response(
            id,
            json!({
                "run_id": result.run_id.0,
                "module": if result.module.is_some() { json!({ "type": "compiled" }) } else { Json::Null },
                "exports": result.exports,
                "dependencies": result.dependencies,
                "warnings": result.warnings.iter().map(diag_json).collect::<Vec<_>>(),
                "errors": result.errors.iter().map(diag_json).collect::<Vec<_>>(),
            }),
        )
    }

    /// Evaluate source in a fresh context (contract §6). Harness errors
    /// are returned inside the result envelope, not as JSON-RPC errors:
    /// the response is a success carrying `error` (contract §10).
    fn eval(&self, params: &Json, id: Json) -> Json {
        let session = match self.with_session(params, id.clone()) {
            Ok(session) => session,
            Err(response) => return response,
        };
        let Ok((name, source)) = name_and_source(params) else {
            return error_response(
                id,
                CODE_INVALID_PARAMS,
                "invalid params: name and source are strings",
            );
        };
        let result = session.lock().eval(&name, source);
        result_response(id, run_envelope(&result))
    }

    /// Run a `.zio-image` (contract §14).
    fn run(&self, params: &Json, id: Json) -> Json {
        let session = match self.with_session(params, id.clone()) {
            Ok(session) => session,
            Err(response) => return response,
        };
        let Some(image_path) = params.get("image_path").and_then(Json::as_str) else {
            return error_response(
                id,
                CODE_INVALID_PARAMS,
                "invalid params: image_path is a string",
            );
        };
        let result = session.lock().run_image(Path::new(image_path));
        result_response(id, run_envelope(&result))
    }

    /// Acknowledge a cancel request; the flag is consumed by the next run
    /// boundary (contract §9).
    fn cancel(&self, params: &Json, id: Json) -> Json {
        let session = match self.with_session(params, id.clone()) {
            Ok(session) => session,
            Err(response) => return response,
        };
        session.lock().cancel();
        result_response(id, json!({ "ok": true }))
    }

    /// Remove the session from the map and release it (contract §2).
    fn close_session(&self, params: &Json, id: Json) -> Json {
        let Some(session_id) = params.get("session_id").and_then(Json::as_u64) else {
            return error_response(
                id,
                CODE_INVALID_PARAMS,
                "invalid params: session_id is a non-negative integer",
            );
        };
        match self.sessions.lock().remove(&session_id) {
            None => error_response(id, CODE_SESSION_NOT_FOUND, "session not found"),
            Some(session) => {
                // The map held the last reference; hand the session to
                // Session::close for explicit release.
                if let Ok(mutex) = Arc::try_unwrap(session) {
                    mutex.into_inner().close();
                }
                result_response(id, json!({ "ok": true }))
            }
        }
    }

    /// Look up the session named by `params.session_id`.
    fn with_session(&self, params: &Json, id: Json) -> Result<Arc<Mutex<Session>>, Json> {
        let Some(session_id) = params.get("session_id").and_then(Json::as_u64) else {
            return Err(error_response(
                id,
                CODE_INVALID_PARAMS,
                "invalid params: session_id is a non-negative integer",
            ));
        };
        match self.sessions.lock().get(&session_id).cloned() {
            Some(session) => Ok(session),
            None => Err(error_response(
                id,
                CODE_SESSION_NOT_FOUND,
                "session not found",
            )),
        }
    }
}

/// The `name` + `source` pair shared by `compile` and `eval`.
fn name_and_source(params: &Json) -> Result<(String, &str), ()> {
    let name = params
        .get("name")
        .and_then(Json::as_str)
        .unwrap_or("eval")
        .to_string();
    let source = params.get("source").and_then(Json::as_str).ok_or(())?;
    Ok((name, source))
}

/// Capability flags → `CapabilitySet` (contract §4). The harness API can
/// express zero or one bit — zio-core `CapabilitySet` has no union
/// constructor yet — so a request naming several true flags is rejected
/// rather than silently under-granted; the set is exactly what was
/// asked, or the request fails.
fn parse_capabilities(params: &Json) -> Result<CapabilitySet, String> {
    let Some(capabilities) = params.get("capabilities") else {
        return Ok(CapabilitySet::empty());
    };
    let object = capabilities
        .as_object()
        .ok_or_else(|| "capabilities must be an object".to_string())?;
    let mut granted = Vec::new();
    for (name, bit) in [
        ("FileRead", Capability::FileRead),
        ("FileWrite", Capability::FileWrite),
        ("Network", Capability::Network),
        ("ProcessSpawn", Capability::ProcessSpawn),
        ("JsEngine", Capability::JsEngine),
    ] {
        if object.get(name).and_then(Json::as_bool).unwrap_or(false) {
            granted.push(bit);
        }
    }
    match granted.as_slice() {
        [] => Ok(CapabilitySet::empty()),
        [bit] => Ok(CapabilitySet::with(*bit)),
        _ => Err(
            "multiple capabilities requested, but zio-core CapabilitySet has no union \
             constructor yet; grant a single capability"
                .to_string(),
        ),
    }
}

/// Budget fields → `Budgets` (contract §8). Every item is opt-in.
fn parse_budgets(params: &Json) -> Result<Budgets, String> {
    let Some(budgets) = params.get("budgets") else {
        return Ok(Budgets::default());
    };
    let object = budgets
        .as_object()
        .ok_or_else(|| "budgets must be an object".to_string())?;
    let fuel = opt_u64(object, "fuel")?;
    let max_duration = opt_u64(object, "max_duration_ms")?.map(Duration::from_millis);
    let max_output_bytes = opt_u64(object, "max_output_bytes")?.map(|bytes| bytes as usize);
    Ok(Budgets {
        fuel,
        max_duration,
        max_output_bytes,
    })
}

fn opt_u64(object: &serde_json::Map<String, Json>, key: &str) -> Result<Option<u64>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("budgets.{key} must be a non-negative integer")),
    }
}

// ── Wire serializers ──────────────────────────────────────────────

/// `RunResult` → the contract §6 envelope. Harness errors ride inside
/// the envelope (`error`), so a failed run is still a successful
/// JSON-RPC response.
fn run_envelope(result: &RunResult) -> Json {
    let value = match &result.value {
        Some(value) => value_json(value),
        None => Json::Null,
    };
    let error = match &result.error {
        Some(error) => err_json(error),
        None => Json::Null,
    };
    json!({
        "run_id": result.run_id.0,
        "value": value,
        "error": error,
        "output": result.output,
        "events": result.events.iter().map(event_json).collect::<Vec<_>>(),
        "fuel_spent": result.fuel_spent,
    })
}

/// `ValueRef` wire form (Slice 1). Integers travel as decimal strings so
/// bigint precision survives the wire (contract §6 row 6, §16 open
/// question); every other value is a to-string fallback.
fn value_json(value: &Value) -> Json {
    match value {
        Value::Integer(i) => json!({ "kind": "integer", "value": i.to_string() }),
        Value::String(s) => json!({ "kind": "string", "value": s }),
        Value::Nil => json!({ "kind": "nil", "value": Json::Null }),
        Value::Boolean(b) => json!({ "kind": "boolean", "value": b }),
        Value::Float(f) => json!({ "kind": "float", "value": f }),
        Value::Symbol(s) => json!({ "kind": "symbol", "value": s }),
        Value::Keyword(k) => json!({ "kind": "keyword", "value": k }),
        Value::List(_) => json!({ "kind": "list", "value": value.to_string() }),
        Value::Vector(_) => json!({ "kind": "vector", "value": value.to_string() }),
        Value::Map(_) => json!({ "kind": "map", "value": value.to_string() }),
        Value::Function(_) => json!({ "kind": "function", "value": value.to_string() }),
        Value::NativeFunction(_) => json!({ "kind": "native", "value": value.to_string() }),
        Value::Object(_) => json!({ "kind": "z_os_object", "value": value.to_string() }),
        // Char/macro/buffer/future/channel have no listed kind; the
        // to-string fallback stands in until the full Value codec.
        _ => json!({ "kind": "atom", "value": value.to_string() }),
    }
}

/// `ErrRef` wire form. The typed harness errors carry structured fields;
/// every other `EvalError` is the `Eval` kind with a to-string message
/// (the location text is embedded in the message; structured span
/// extraction for every variant is a later slice).
fn err_json(error: &EvalError) -> Json {
    match error {
        EvalError::CapabilityDenied { capability, span } => json!({
            "kind": "CapabilityDenied",
            "message": format!("capability denied: {capability}"),
            "span": span_json(span.span),
            "capability": capability,
            "fuel_spent": Json::Null,
            "elapsed_ms": Json::Null,
        }),
        EvalError::Cancelled { fuel_spent, span } => json!({
            "kind": "Cancelled",
            "message": "cancelled",
            "span": span_json(span.span),
            "fuel_spent": fuel_spent,
            "elapsed_ms": Json::Null,
        }),
        EvalError::TimedOut {
            elapsed_ms,
            fuel_spent,
            span,
        } => json!({
            "kind": "TimedOut",
            "message": "timed out",
            "span": span_json(span.span),
            "fuel_spent": fuel_spent,
            "elapsed_ms": elapsed_ms,
        }),
        EvalError::LoadError(message, span) => json!({
            "kind": "LoadError",
            "message": message,
            "span": span_json(span.span),
            "fuel_spent": Json::Null,
            "elapsed_ms": Json::Null,
        }),
        EvalError::Custom(message, span) => json!({
            "kind": "Custom",
            "message": message,
            "span": span_json(span.span),
            "fuel_spent": Json::Null,
            "elapsed_ms": Json::Null,
        }),
        other => json!({
            "kind": "Eval",
            "message": other.to_string(),
            "span": Json::Null,
            "fuel_spent": Json::Null,
            "elapsed_ms": Json::Null,
        }),
    }
}

/// `Diagnostic` → wire form (compile envelope, contract §5).
fn diag_json(diagnostic: &Diagnostic) -> Json {
    json!({
        "message": diagnostic.message,
        "span": span_json(diagnostic.span),
    })
}

/// Observer event → wire form (contract §6/§7).
fn event_json(event: &ObserverEvent) -> Json {
    json!({
        "kind": event.kind,
        "span": span_json(event.span),
    })
}

/// `Option<Span>` → structured location or null.
fn span_json(span: Option<Span>) -> Json {
    match span {
        Some(span) => json!({
            "start": span.start.0,
            "end": span.end.0,
            "line": span.line,
            "col": span.col,
        }),
        None => Json::Null,
    }
}

// ── JSON-RPC envelopes ────────────────────────────────────────────

fn result_response(id: Json, result: Json) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Json, code: i64, message: impl Into<String>) -> Json {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() },
    })
}
