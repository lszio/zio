//! Trusted generic CPU tensor kernels over the shared bounded process lifecycle.
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Map, Value as Json, json};
use zio_core::context::EvalContext;
use zio_core::error::EvalError;
use zio_core::im::Vector;
use zio_core::value::{NativeFn, Value};

use crate::transport::{Process, ProcessConfig, ProcessRead};
use crate::{HostPolicy, values};

const HANDLE: &str = "host/tensor-backend";
const MAX_ID: u64 = 1 << 53;

fn invalid(message: impl AsRef<str>) -> EvalError {
    EvalError::custom(format!("invalid-input: {}", message.as_ref()))
}

fn limit(
    config: &Map<String, Json>,
    key: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, EvalError> {
    let value = config
        .get(key)
        .map_or(Some(default), Json::as_u64)
        .ok_or_else(|| invalid(format!("{key} must be an integer")))?;
    if value < minimum || value > maximum {
        return Err(invalid(format!("{key} must be in {minimum}..{maximum}")));
    }
    Ok(value)
}

fn config(input: &Value, policy: &HostPolicy) -> Result<(ProcessConfig, Duration), EvalError> {
    if !policy.tensor || !policy.process {
        return Err(EvalError::custom("capability-denied: tensor process"));
    }
    let backend = policy
        .trusted_tensor_backend
        .as_ref()
        .ok_or_else(|| EvalError::custom("capability-denied: no trusted tensor backend grant"))?
        .canonicalize()
        .map_err(|error| {
            EvalError::custom(format!("backend-failed: trusted tensor backend: {error}"))
        })?;
    let python = policy
        .trusted_tensor_python
        .as_ref()
        .ok_or_else(|| EvalError::custom("capability-denied: no trusted tensor Python grant"))?
        .canonicalize()
        .map_err(|error| {
            EvalError::custom(format!("backend-failed: trusted tensor Python: {error}"))
        })?;
    if !backend.is_file() || !python.is_file() {
        return Err(invalid("trusted tensor resources must be files"));
    }
    let cwd = backend
        .parent()
        .ok_or_else(|| invalid("trusted backend has no parent"))?;
    let input = values::to_json(input)?;
    let input = input
        .as_object()
        .ok_or_else(|| invalid("tensor config must be a map"))?;
    const KEYS: &[&str] = &[
        "timeout-ms",
        "write-timeout-ms",
        "call-timeout-ms",
        "max-frame-bytes",
        "max-output-bytes",
        "max-stderr-bytes",
        "max-address-space",
        "max-cpu-seconds",
        "max-processes",
        "max-elements",
        "max-tensors",
    ];
    if input.keys().any(|key| !KEYS.contains(&key.as_str())) {
        return Err(invalid(
            "tensor config accepts only resource limits; executable/argv/cwd/environment are launcher-owned",
        ));
    }
    let frame = limit(input, "max-frame-bytes", 1 << 20, 256, 16 << 20)?;
    let elements = limit(input, "max-elements", 8 << 20, 1, 1 << 28)?;
    let tensors = limit(input, "max-tensors", 4096, 1, 65536)?;
    let call_timeout = Duration::from_millis(limit(input, "call-timeout-ms", 30000, 1, 86400000)?);
    let mut generated = input.clone();
    generated.remove("max-elements");
    generated.remove("max-tensors");
    generated.remove("call-timeout-ms");
    generated.insert("executable".into(), json!(python));
    generated.insert("cwd".into(), json!(cwd));
    // -I ignores source-controlled Python paths and user startup modules. No arbitrary code flags.
    generated.insert(
        "argv".into(),
        json!([
            "-I",
            "-u",
            backend
                .to_str()
                .ok_or_else(|| invalid("backend path is not UTF-8"))?,
            "--max-frame-bytes",
            frame.to_string(),
            "--max-elements",
            elements.to_string(),
            "--max-tensors",
            tensors.to_string(),
        ]),
    );
    generated.insert("environment".into(), json!({}));
    generated.insert("max-frame-bytes".into(), json!(frame));
    Ok((
        ProcessConfig::from_json(Json::Object(generated), policy)?,
        call_timeout,
    ))
}

struct Backend {
    process: Process,
    next_id: u64,
    call_timeout: Duration,
}

impl Backend {
    fn call(&mut self, operation: &Value) -> Result<Value, EvalError> {
        let operation = values::to_json(operation)?;
        if !operation.is_object() || !operation.get("op").is_some_and(Json::is_string) {
            return Err(invalid("tensor operation must be a map with string op"));
        }
        if self.next_id >= MAX_ID {
            return Err(EvalError::custom(
                "resource-limit: tensor correlation counter exhausted",
            ));
        }
        let id = self.next_id;
        self.next_id += 1;
        self.process
            .write(&json!({"id": id, "operation": operation}))?;
        let frame = match self.process.read(self.call_timeout)? {
            ProcessRead::Frame(frame) => frame,
            ProcessRead::Timeout => {
                return Err(EvalError::custom(
                    "backend-timeout: tensor RPC deadline exceeded",
                ));
            }
            ProcessRead::Eof => {
                let exit = self.process.kill()?;
                return Err(EvalError::custom(format!(
                    "backend-failed: tensor EOF (code {:?}): {}",
                    exit.code,
                    String::from_utf8_lossy(&exit.stderr)
                )));
            }
        };
        let object = frame
            .as_object()
            .ok_or_else(|| EvalError::custom("protocol-error: tensor reply must be an object"))?;
        if object.get("id").and_then(Json::as_u64) != Some(id) {
            return Err(EvalError::custom(
                "protocol-error: tensor reply correlation mismatch",
            ));
        }
        match object.get("ok").and_then(Json::as_bool) {
            Some(true) if object.len() == 3 && object.contains_key("result") => {
                values::from_json(object["result"].clone())
            }
            Some(false) if object.len() == 3 && object.contains_key("error") => {
                let error = object["error"]
                    .as_object()
                    .ok_or_else(|| EvalError::custom("protocol-error: malformed tensor error"))?;
                let class = error
                    .get("class")
                    .and_then(Json::as_str)
                    .filter(|class| {
                        [
                            "invalid-input",
                            "shape-error",
                            "nonfinite",
                            "resource-limit",
                        ]
                        .contains(class)
                    })
                    .ok_or_else(|| {
                        EvalError::custom("protocol-error: invalid tensor error class")
                    })?;
                let message = error.get("message").and_then(Json::as_str).ok_or_else(|| {
                    EvalError::custom("protocol-error: tensor error message missing")
                })?;
                if error.len() != 2 || message.len() > 8192 {
                    return Err(EvalError::custom(
                        "protocol-error: malformed tensor error envelope",
                    ));
                }
                Err(EvalError::custom(format!("{class}: {message}")))
            }
            _ => Err(EvalError::custom(
                "protocol-error: invalid tensor reply envelope",
            )),
        }
    }
}

fn resource(backend: Backend) -> Value {
    let state = Rc::new(RefCell::new(Some(backend)));
    Value::NativeFunction(NativeFn::new(HANDLE, move |args, _| {
        if args.is_empty() {
            return Err(invalid("tensor handle requires operation"));
        }
        let action = values::string(&args[0])?;
        let mut state = state
            .try_borrow_mut()
            .map_err(|_| EvalError::custom("resource-busy: tensor RPC is already active"))?;
        match action {
            "close" => {
                values::arity(&args, 1)?;
                if let Some(mut backend) = state.take() {
                    backend.process.kill()?;
                }
                Ok(Value::Nil)
            }
            "call" => {
                values::arity(&args, 2)?;
                let backend = state
                    .as_mut()
                    .ok_or_else(|| EvalError::custom("resource-closed: tensor backend"))?;
                let result = backend.call(&args[1]);
                // Operation refusals preserve the channel. Transport/protocol failure cannot leave a stale reply.
                if result.as_ref().is_err_and(|error| {
                    let message = error.to_string();
                    ![
                        "invalid-input:",
                        "shape-error:",
                        "nonfinite:",
                        "resource-limit:",
                    ]
                    .iter()
                    .any(|prefix| message.starts_with(prefix))
                }) {
                    if let Some(mut backend) = state.take() {
                        let _ = backend.process.kill();
                    }
                }
                result
            }
            _ => Err(invalid("unknown tensor handle action")),
        }
    }))
}

pub fn install(ctx: &EvalContext, policy: &HostPolicy) {
    let policy = policy.clone();
    ctx.env.set(
        "host/tensor-start".into(),
        Value::NativeFunction(NativeFn::new("host/tensor-start", move |args, _| {
            values::arity(&args, 1)?;
            let (config, call_timeout) = config(&args[0], &policy)?;
            let process = Process::start_trusted_backend(&config, &policy)?;
            Ok(resource(Backend {
                process,
                next_id: 1,
                call_timeout,
            }))
        })),
    );
    ctx.env.set(
        "host/tensor-call".into(),
        Value::NativeFunction(NativeFn::new("host/tensor-call", |args, engine| {
            values::arity(&args, 2)?;
            values::handle(
                &args[0],
                HANDLE,
                Vector::from(vec![Value::String("call".into()), args[1].clone()]),
                engine,
            )
        })),
    );
    ctx.env.set(
        "host/tensor-close".into(),
        Value::NativeFunction(NativeFn::new("host/tensor-close", |args, engine| {
            values::arity(&args, 1)?;
            values::handle(
                &args[0],
                HANDLE,
                Vector::from(vec![Value::String("close".into())]),
                engine,
            )
        })),
    );
}
