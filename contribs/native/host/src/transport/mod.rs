//! Delegated computation: HTTP, framed stdio, and jailed processes.
//!
//! Everything here is a *mechanism*. Nothing here decides what a request
//! means, who may make it, or what the answer was — that is application
//! policy, and it lives in the program that calls these.
//!
//! The split that matters: a process, a socket and a pipe are all
//! capable of outliving the call that started them, so none of them is
//! touched from inside an evaluation. A [`Process`] owns native file
//! descriptors and a reader thread; a [`Server`] owns a listener and a
//! queue; both hand plain data back to the evaluator. A `Value` never
//! crosses a thread boundary, because a `Value` can close over an
//! environment that must not be reachable from the other side of a
//! process.

pub(crate) mod backend;
pub mod framing;
pub mod http;
pub mod isolation;
pub mod process;
pub(crate) mod stdio;

pub use backend::backend;
pub use http::{HttpConfig, HttpRequest, HttpResponse, Server};
pub use isolation::ReadOnlyMount;
pub use process::{Process, ProcessConfig, ProcessExit, ProcessRead};

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value as Json;
use zio_core::context::{EvalContext, EvalEngine};
use zio_core::error::EvalError;
use zio_core::im::Vector;
use zio_core::value::{NativeFn, Value};

use crate::{HostPolicy, values};

const PROCESS_HANDLE: &str = "host/process-handle";
const HTTP_HANDLE: &str = "host/http-handle";

/// Convert an evaluator response map while preserving buffer-valued bodies.
fn http_response_from_value(value: &Value) -> Result<HttpResponse, EvalError> {
    let status = values::get(value, "status")
        .ok_or_else(|| invalid("HTTP response requires integer status"))?;
    let status = u16::try_from(values::integer(status)?)
        .map_err(|_| invalid("HTTP status out of range"))?;
    let headers = values::get(value, "headers")
        .ok_or_else(|| invalid("HTTP response headers must be an object"))?;
    let headers = values::to_json(headers)?;
    let headers = headers
        .as_object()
        .ok_or_else(|| invalid("HTTP response headers must be an object"))?;
    let mut resolved = BTreeMap::new();
    for (key, value) in headers {
        let value = value
            .as_str()
            .ok_or_else(|| invalid("HTTP response header values must be strings"))?;
        resolved.insert(key.clone(), value.to_owned());
    }
    let body = match values::get(value, "body") {
        Some(body) => values::with_bytes(body, |bytes| bytes.to_vec())?,
        None => Vec::new(),
    };
    Ok(HttpResponse {
        status,
        headers: resolved,
        body,
    })
}

fn invalid(message: impl AsRef<str>) -> EvalError {
    EvalError::custom(format!("invalid-input: {}", message.as_ref()))
}

fn capability(message: impl AsRef<str>) -> EvalError {
    EvalError::custom(format!("capability-denied: {}", message.as_ref()))
}

fn non_negative_ms(value: i64) -> Result<u64, EvalError> {
    if value < 0 {
        Err(invalid("timeout-ms must be non-negative"))?;
    }
    u64::try_from(value).map_err(|_| invalid("timeout-ms exceeded platform range"))
}

fn bind(
    ctx: &EvalContext,
    name: &'static str,
    function: impl Fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError> + 'static,
) {
    ctx.env.set(
        name.into(),
        Value::NativeFunction(NativeFn::new(name, function)),
    );
}

fn buffer(bytes: Vec<u8>) -> Value {
    Value::Buffer(std::sync::Arc::new(parking_lot::Mutex::new(bytes)))
}

fn opt_i64(value: Option<i32>) -> Value {
    match value {
        Some(number) => Value::Integer(number as i64),
        None => Value::Nil,
    }
}

fn keyword_map(entries: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    values::map(entries)
}

fn json_to_value(value: Json) -> Result<Value, EvalError> {
    values::from_json(value)
}


fn exit_map(exit: &ProcessExit) -> Value {
    keyword_map([
        ("success", Value::Boolean(exit.success)),
        ("code", opt_i64(exit.code)),
        ("signal", opt_i64(exit.signal)),
        ("stderr", buffer(exit.stderr.clone())),
    ])
}

fn headers_map(headers: &BTreeMap<String, String>) -> Value {
    let mut value_map = zio_core::im::HashMap::new();
    for (key, val) in headers {
        value_map.insert(Value::String(key.clone()), Value::String(val.clone()));
    }
    Value::Map(value_map)
}

fn request_envelope(request: HttpRequest) -> Value {
    keyword_map([
        ("id", Value::Integer(request.id)),
        ("method", Value::String(request.method)),
        ("path", Value::String(request.path)),
        ("query", Value::String(request.query)),
        ("headers", headers_map(&request.headers)),
        ("body", buffer(request.body)),
    ])
}

fn response_envelope(response: HttpResponse) -> Value {
    keyword_map([
        ("status", Value::Integer(response.status as i64)),
        ("headers", headers_map(&response.headers)),
        ("body", buffer(response.body)),
    ])
}

fn stdio_envelope(frame: stdio::Frame, timeout_label: &str) -> Value {
    match frame {
        stdio::Frame::Value(json) => {
            let value = json_to_value(json).unwrap_or(Value::Nil);
            keyword_map([("status", Value::String("frame".into())), ("frame", value)])
        }
        stdio::Frame::Timeout => keyword_map([("status", Value::String(timeout_label.into()))]),
        stdio::Frame::Eof => keyword_map([("status", Value::String("eof".into()))]),
    }
}

fn process_envelope(read: ProcessRead, timeout_label: &str) -> Value {
    match read {
        ProcessRead::Frame(json) => {
            let value = json_to_value(json).unwrap_or(Value::Nil);
            keyword_map([("status", Value::String("frame".into())), ("frame", value)])
        }
        ProcessRead::Timeout => keyword_map([("status", Value::String(timeout_label.into()))]),
        ProcessRead::Eof => keyword_map([("status", Value::String("eof".into()))]),
    }
}

fn process_handle(process: Process) -> Value {
    let state = Rc::new(RefCell::new(Some(process)));
    Value::NativeFunction(NativeFn::new(PROCESS_HANDLE, move |args, _| {
        if args.is_empty() {
            return Err(invalid("process handle requires operation"));
        }
        let op = values::string(&args[0])?;
        let mut slot = state
            .try_borrow_mut()
            .map_err(|_| EvalError::custom("resource-busy: process RPC is already active"))?;
        let process = slot
            .as_mut()
            .ok_or_else(|| EvalError::custom("resource-closed: process"))?;
        match op {
            "read" => {
                values::arity(&args, 2)?;
                let timeout = Duration::from_millis(non_negative_ms(values::integer(&args[1])?)?);
                let outcome = process.read(timeout)?;
                Ok(process_envelope(outcome, "timeout"))
            }
            "readable" => {
                values::arity(&args, 1)?;
                Ok(Value::Boolean(process.readable()?))
            }
            "write" => {
                values::arity(&args, 2)?;
                let json = values::to_json(&args[1])?;
                process.write(&json)?;
                Ok(Value::Nil)
            }
            "wait" => {
                values::arity(&args, 2)?;
                let timeout = Duration::from_millis(non_negative_ms(values::integer(&args[1])?)?);
                Ok(match process.wait(timeout)? {
                    Some(exit) => exit_map(&exit),
                    None => Value::Nil,
                })
            }
            "kill" => {
                values::arity(&args, 1)?;
                Ok(exit_map(&process.kill()?))
            }
            other => Err(invalid(format!("unknown process handle action: {other}"))),
        }
    }))
}

fn http_handle(server: Server) -> Value {
    let state = Rc::new(RefCell::new(Some(server)));
    Value::NativeFunction(NativeFn::new(HTTP_HANDLE, move |args, _| {
        if args.is_empty() {
            return Err(invalid("http handle requires operation"));
        }
        let op = values::string(&args[0])?;
        let mut slot = state
            .try_borrow_mut()
            .map_err(|_| EvalError::custom("resource-busy: http RPC is already active"))?;
        let server = slot
            .as_mut()
            .ok_or_else(|| EvalError::custom("resource-closed: http server"))?;
        match op {
            "next" => {
                values::arity(&args, 2)?;
                let timeout = Duration::from_millis(non_negative_ms(values::integer(&args[1])?)?);
                Ok(match server.next(timeout)? {
                    Some(request) => request_envelope(request),
                    None => Value::Nil,
                })
            }
            "readable" => {
                values::arity(&args, 1)?;
                Ok(Value::Boolean(server.readable()?))
            }
            "reply" => {
                values::arity(&args, 3)?;
                let id = values::integer(&args[1])?;
                let response = http_response_from_value(&args[2])?;
                server.reply(id, response)?;
                Ok(Value::Nil)
            }
            "close" => {
                values::arity(&args, 1)?;
                server.close();
                Ok(Value::Nil)
            }
            other => Err(invalid(format!("unknown http handle action: {other}"))),
        }
    }))
}

/// Dispatch to the handle, forwarding the operation + its own args.
/// Caller has already verified the public arity.
fn call_handle(
    handle: &Value,
    op: &'static str,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
    kind: &str,
) -> Result<Value, EvalError> {
    match handle {
        Value::NativeFunction(function) if function.name() == kind => {
            let mut next = Vector::new();
            next.push_back(Value::String(op.into()));
            for arg in args {
                next.push_back(arg);
            }
            function.call(next, engine)
        }
        _ => Err(invalid(format!("expected opaque {kind} resource"))),
    }
}

/// Bind every generic host/* transport primitive. The installer reads
/// `HostPolicy` at call time, so flipping the private flags after install
/// changes what the eval side can do without re-binding.
pub fn install(ctx: &EvalContext, policy: &HostPolicy) {
    // Shared so every `move` closure can read the same policy without
    // each needing its own copy. `Arc` rather than `Rc` because the
    // transport owns reader and socket threads, and the policy is read
    // from call time to decide a capability — a policy that could not
    // cross that boundary would be a policy only the main thread saw.
    let policy = Arc::new(policy.clone());

    // Process — gated by policy.process. Always goes through the jail;
    // start_trusted_backend is pub(crate) and only callable by tensor.
    {
        let policy_1 = Arc::clone(&policy);
        bind(ctx, "host/process-start", move |args, _| {
            if !policy_1.process {
                return Err(capability("process"));
            }
            values::arity(&args, 1)?;
            let json = values::to_json(&args[0])?;
            let config = ProcessConfig::from_json(json, &policy_1)?;
            let process = Process::start(&config, &policy_1)?;
            Ok(process_handle(process))
        });
    }
    {
        let policy_2 = Arc::clone(&policy);
        bind(ctx, "host/process-read", move |args, engine| {
            if !policy_2.process {
                return Err(capability("process"));
            }
            values::arity(&args, 2)?;
            let rest = Vector::from(vec![args[1].clone()]);
            call_handle(&args[0], "read", rest, engine, PROCESS_HANDLE)
        });
        let policy_3 = Arc::clone(&policy);
        bind(ctx, "host/process-readable", move |args, engine| {
            if !policy_3.process {
                return Err(capability("process"));
            }
            values::arity(&args, 1)?;
            call_handle(&args[0], "readable", Vector::new(), engine, PROCESS_HANDLE)
        });
        let policy_4 = Arc::clone(&policy);
        bind(ctx, "host/process-write", move |args, engine| {
            if !policy_4.process {
                return Err(capability("process"));
            }
            values::arity(&args, 2)?;
            let rest = Vector::from(vec![args[1].clone()]);
            call_handle(&args[0], "write", rest, engine, PROCESS_HANDLE)
        });
        let policy_5 = Arc::clone(&policy);
        bind(ctx, "host/process-wait", move |args, engine| {
            if !policy_5.process {
                return Err(capability("process"));
            }
            values::arity(&args, 2)?;
            let rest = Vector::from(vec![args[1].clone()]);
            call_handle(&args[0], "wait", rest, engine, PROCESS_HANDLE)
        });
        let policy_6 = Arc::clone(&policy);
        bind(ctx, "host/process-kill", move |args, engine| {
            if !policy_6.process {
                return Err(capability("process"));
            }
            values::arity(&args, 1)?;
            call_handle(&args[0], "kill", Vector::new(), engine, PROCESS_HANDLE)
        });
    }

    // HTTP — request is gated by policy_6.network; the listener is a long-lived
    // socket that the policy gates at start time.
    {
        let policy_7 = Arc::clone(&policy);
        bind(ctx, "host/http-request", move |args, _| {
            if !policy_7.network {
                return Err(capability("network"));
            }
            values::arity(&args, 1)?;
            let json = values::to_json(&args[0])?;
            let body = json.get("body").cloned().unwrap_or(Json::Null);
            let bytes = match body {
                Json::String(text) => text.into_bytes(),
                Json::Array(items) => {
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        let number = item
                            .as_u64()
                            .ok_or_else(|| invalid("HTTP body bytes must be integers"))?;
                        out.push(
                            u8::try_from(number)
                                .map_err(|_| invalid("HTTP body byte out of range"))?,
                        );
                    }
                    out
                }
                Json::Null => Vec::new(),
                _ => {
                    return Err(invalid(
                        "HTTP request body must be a string, byte sequence or omitted",
                    ));
                }
            };
            let response = http::request(&json, &bytes)?;
            Ok(response_envelope(response))
        });
        let policy_8 = Arc::clone(&policy);
        bind(ctx, "host/http-listen", move |args, _| {
            if !policy_8.network {
                return Err(capability("network"));
            }
            values::arity(&args, 1)?;
            let json = values::to_json(&args[0])?;
            let config = HttpConfig::from_json(&json)?;
            let server = Server::listen(config)?;
            Ok(http_handle(server))
        });
    }

    // The server half. `http-listen` returns an opaque handle, and these
    // three are how a program drives it. They are bound separately
    // rather than folded into the handle's own arity so the call sites
    // read as what they do: take the next request, answer it, close
    // the exchange.
    {
        let policy_10 = Arc::clone(&policy);
        bind(ctx, "host/http-next", move |args, engine| {
            if !policy_10.network {
                return Err(capability("network"));
            }
            values::arity(&args, 2)?;
            let timeout = Duration::from_millis(non_negative_ms(values::integer(&args[1])?)?);
            call_handle(
                &args[0],
                "next",
                Vector::from(vec![Value::Integer(timeout.as_millis() as i64)]),
                engine,
                HTTP_HANDLE,
            )
        });
    }
    {
        let policy_11 = Arc::clone(&policy);
        bind(ctx, "host/http-readable", move |args, engine| {
            if !policy_11.network {
                return Err(capability("network"));
            }
            values::arity(&args, 1)?;
            call_handle(
                &args[0],
                "readable",
                Vector::new(),
                engine,
                HTTP_HANDLE,
            )
        });
    }
    {
        let policy_12 = Arc::clone(&policy);
        bind(ctx, "host/http-reply", move |args, engine| {
            if !policy_12.network {
                return Err(capability("network"));
            }
            values::arity(&args, 3)?;
            call_handle(
                &args[0],
                "reply",
                Vector::from(vec![args[1].clone(), args[2].clone()]),
                engine,
                HTTP_HANDLE,
            )
        });
    }
    {
        let policy_13 = Arc::clone(&policy);
        bind(ctx, "host/http-close", move |args, engine| {
            if !policy_13.network {
                return Err(capability("network"));
            }
            values::arity(&args, 1)?;
            call_handle(
                &args[0],
                "close",
                Vector::new(),
                engine,
                HTTP_HANDLE,
            )
        });
    }

    // Host stdio + JSON-RPC — gated by policy_8.stdio. NDJSON for worker
    // stdin/stdout, Content-Length for editor / JSON-RPC peers; the
    // framing helper is shared so the wire format is identical.
    {
        let policy_9 = Arc::clone(&policy);
        bind(ctx, "host/stdio-read", move |args, _| {
            if !policy_9.stdio {
                return Err(capability("stdio"));
            }
            values::arity(&args, 1)?;
            let timeout = Duration::from_millis(non_negative_ms(values::integer(&args[0])?)?);
            Ok(stdio_envelope(stdio::read_ndjson(timeout)?, "timeout"))
        });
        let policy_10 = Arc::clone(&policy);
        bind(ctx, "host/stdio-write", move |args, _| {
            if !policy_10.stdio {
                return Err(capability("stdio"));
            }
            values::arity(&args, 1)?;
            let json = values::to_json(&args[0])?;
            stdio::write_ndjson(&json)?;
            Ok(Value::Nil)
        });
        let policy_11 = Arc::clone(&policy);
        bind(ctx, "host/rpc-read", move |args, _| {
            if !policy_11.stdio {
                return Err(capability("stdio"));
            }
            values::arity(&args, 1)?;
            let timeout = Duration::from_millis(non_negative_ms(values::integer(&args[0])?)?);
            Ok(stdio_envelope(stdio::read_rpc(timeout)?, "timeout"))
        });
        let policy_12 = Arc::clone(&policy);
        bind(ctx, "host/rpc-write", move |args, _| {
            if !policy_12.stdio {
                return Err(capability("stdio"));
            }
            values::arity(&args, 1)?;
            let json = values::to_json(&args[0])?;
            stdio::write_rpc(&json)?;
            Ok(Value::Nil)
        });
    }
}
