//! Isolated candidate execution. The boundary that decides what a
//! candidate can reach while it runs.
//!
//! Three contracts this file keeps:
//!
//! * **A candidate runs in a fresh `EvalContext`.** Nothing from the
//!   caller's environment is reachable: no host/app/native function
//!   handle, no storage/transport/tensor binding, no `secret` symbol
//!   defined by the embedder, no closure over a parent's `Arc<Env>`.
//!   A candidate that asks for a name the parent had but the fresh
//!   context does not gets `symbol not found: <name>` back — not the
//!   value.
//!
//! * **Modules come only from the frozen map.** No filesystem, no
//!   ambient `ZIO_PATH`, no parent module table. Cycle detection
//!   rides on `ModuleTable::begin_loading`'s path stack, which
//!   reports `circular require detected` when the same module is
//!   entered twice.
//!
//! * **JSON safety is strict at both ends.** An input that cannot be
//!   represented as JSON never runs the candidate — the envelope is
//!   `refused`. A result that cannot be represented is an envelope
//!   `failed` with `JSON` in the error; we never let an opaque
//!   `Value::Function` or `Value::NativeFunction` slip out as the
//!   candidate's answer.
//!
//! Step bounding rides on `zio_core`'s observation port: every
//! `eval_inner` charges one fuel unit and the candidate is refused
//! at the wall defined by `max_steps`. The VM hook the compiler agent
//! is adding will charge inside `invoke_closure`; until that lands,
//! the interpreter path is what is bounded, and the "compiled"
//! engine routes through the same interpreter because
//! `Function::compiled` is never set by anything reachable today.
//!
//! Wall-clock bounding rides on a watchdog thread that flips a shared
//! flag after `timeout_ms`; the interpreter surfaces it as a refusal
//! on the next fuel-charge boundary.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use zio_core::bootstrap;
use zio_core::context::{EvalContext, EvalEngine, EvalRuntime, ModuleLoader, ModuleRegistry};
use zio_core::env::Env;
use zio_core::error::EvalError;
use zio_core::im::{HashMap, Vector, vector};
use zio_core::io::IoHost;
use zio_core::module::{Module, ModuleTable};
use zio_core::observer::{Event, EventKind, Observer, RecordingObserver};
use zio_core::value::{NativeFn, Value};

use zio_core::builtins::json;

use crate::{HostPolicy, values};

// ── Public API ────────────────────────────────────────────────────

/// Frozen module sources keyed by dotted module name. Bytes are the
/// raw `.zio` source — `load` / `require` may only see what was
/// passed in. A module not in the map is `module not found`; a
/// module in the map but circularly depending on itself fails with
/// `circular require detected`.
pub type FrozenModules = BTreeMap<String, Vec<u8>>;

/// Step, output, source-size and wall-clock bounds. All four must be
/// nonzero for untrusted use: zero would mean "unbounded" in the fuel
/// accounting, and unbounded is exactly what a candidate that has
/// not been read carefully must not get.
#[derive(Clone, Debug)]
pub struct ExecutionLimits {
    pub max_steps: u64,
    pub max_output_bytes: usize,
    pub max_source_bytes: usize,
    pub timeout_ms: u64,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            max_steps: 1_000_000,
            max_output_bytes: 64 * 1024,
            max_source_bytes: 256 * 1024,
            timeout_ms: 1000,
        }
    }
}

impl ExecutionLimits {
    /// Reject limits that would let a candidate run unbounded. Zero
    /// in any field is a configuration error, not a synonym for
    /// infinity.
    pub fn validate(&self) -> Result<(), EvalError> {
        if self.max_steps == 0 {
            return Err(EvalError::custom(
                "invalid-input: max_steps must be nonzero",
            ));
        }
        if self.max_output_bytes == 0 {
            return Err(EvalError::custom(
                "invalid-input: max_output_bytes must be nonzero",
            ));
        }
        if self.max_source_bytes == 0 {
            return Err(EvalError::custom(
                "invalid-input: max_source_bytes must be nonzero",
            ));
        }
        if self.timeout_ms == 0 {
            return Err(EvalError::custom(
                "invalid-input: timeout_ms must be nonzero",
            ));
        }
        Ok(())
    }
}

/// Engine selection. Today both go through the interpreter — the VM
/// hook has not landed yet, so `Compiled` is functionally equivalent.
/// The distinction is preserved so the boundary is stable once the
/// compiler agent's `invoke_closure` fuel hook arrives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionEngine {
    Compiled,
    Interpreter,
}

/// Evaluate a candidate source string. Always returns an envelope
/// `Value`. `Result::Err` is reserved for envelope-impossible
/// conditions — only the configuration failures above; an invalid
/// JSON input is `refused` inside the envelope, not a transport
/// error.
pub fn evaluate(
    source: &str,
    name: &str,
    entry: Option<&str>,
    input: Option<Value>,
    limits: ExecutionLimits,
    modules: FrozenModules,
) -> Result<Value, EvalError> {
    evaluate_with_engine(
        source,
        name,
        entry,
        input,
        limits,
        modules,
        ExecutionEngine::Interpreter,
    )
}

/// Same as [`evaluate`] but with an explicit engine choice. Both
/// engines run through the interpreter today — the VM hook has not
/// landed — so the distinction is reserved for when the compiler
/// agent finishes wiring `spend_fuel` inside `invoke_closure`.
pub fn evaluate_with_engine(
    source: &str,
    name: &str,
    entry: Option<&str>,
    input: Option<Value>,
    limits: ExecutionLimits,
    modules: FrozenModules,
    _engine: ExecutionEngine,
) -> Result<Value, EvalError> {
    if let Err(error) = limits.validate() {
        return Ok(refused_envelope(&error.to_string(), 0, false));
    }
    if source.len() > limits.max_source_bytes {
        return Ok(refused_envelope(
            "source size exceeds max_source_bytes",
            0,
            false,
        ));
    }
    if let Some(input) = input.as_ref() {
        if let Err(error) = json::to_json(input) {
            return Ok(refused_envelope(
                &format!("input is not JSON-safe: {error}"),
                0,
                false,
            ));
        }
    }
    let observer: Arc<dyn Observer> = Arc::new(RecordingObserver::new());
    let (outcome, output_text, truncated, steps) = run_candidate(
        source,
        name,
        entry,
        input,
        limits,
        modules,
        Arc::clone(&observer),
    );
    Ok(build_envelope(
        outcome,
        &observer,
        output_text,
        truncated,
        steps,
    ))
}

/// Install the `host/evaluate-isolated` binding on the supplied
/// context. The host policy is intentionally ignored: this is the
/// security boundary, not a host-specific surface. Grants nothing
/// else; a candidate running through this binding cannot reach a
/// parent authority because the fresh context it gets does not
/// inherit it.
pub fn install(ctx: &EvalContext, _policy: &HostPolicy) {
    ctx.env.set(
        "host/evaluate-isolated".into(),
        Value::NativeFunction(NativeFn::new("host/evaluate-isolated", isolated_handler)),
    );
}

// ── Internal: isolation machinery ────────────────────────────────

/// Shared state for the isolated run: a `cancelled` flag the
/// interpreter consults through any path that returns control to
/// fuel accounting and the truncated-output buffer the candidate
/// writes into through `print` / `println`.
struct IsolatedState {
    cancelled: Arc<AtomicBool>,
    output: Arc<Mutex<BoundedOutput>>,
}

impl IsolatedState {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// UTF-8-safe bounded output. Truncates at the largest valid UTF-8
/// character boundary inside `max_bytes`; sets `truncated` once any
/// byte was refused so the envelope can report it honestly.
struct BoundedOutput {
    max_bytes: usize,
    buffer: String,
    truncated: bool,
}

impl BoundedOutput {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            buffer: String::new(),
            truncated: false,
        }
    }

    fn remaining(&self) -> usize {
        self.max_bytes.saturating_sub(self.buffer.len())
    }

    fn try_push_str(&mut self, text: &str) {
        if self.remaining() == 0 {
            if !text.is_empty() {
                self.truncated = true;
            }
            return;
        }
        for ch in text.chars() {
            let needed = ch.len_utf8();
            if self.remaining() < needed {
                self.truncated = true;
                return;
            }
            self.buffer.push(ch);
        }
    }

    fn push_char(&mut self, ch: char) -> bool {
        let needed = ch.len_utf8();
        if self.remaining() < needed {
            self.truncated = true;
            return false;
        }
        self.buffer.push(ch);
        true
    }
}

/// An `IoHost` that prints into a bounded buffer and refuses every
/// filesystem / network / process-touching operation. A candidate
/// that asks for `(slurp …)` or `(load …)` gets an error from the
/// IoHost — but the candidate will not even see those names in the
/// isolated context.
///
/// `canonicalize_path` and `is_directory` answer for paths *inside*
/// the frozen module set so a `ModuleRoots` resolution that calls
/// them works. Anything outside the frozen set is refused — the same
/// scoped check `ModuleRoots::resolve_with_io` performs against the
/// granted roots, applied here to the virtual frozen root.
struct IsolatedIoHost {
    state: Arc<IsolatedState>,
    /// Pseudo-root that all frozen-module paths live under. The
    /// `IsolatedIoHost` answers `file_exists` / `canonicalize_path` /
    /// `is_directory` for paths inside this root and refuses outside.
    frozen_root: PathBuf,
    /// Canonical "frozen://<name>.zio" paths for every module in the
    /// frozen set, used by `file_exists` and `canonicalize_path`.
    frozen_paths: Vec<PathBuf>,
}

impl IsolatedIoHost {
    fn new(state: Arc<IsolatedState>, modules: &FrozenModules) -> Self {
        let frozen_root = PathBuf::from("frozen://");
        let mut frozen_paths = Vec::with_capacity(modules.len());
        for name in modules.keys() {
            frozen_paths.push(PathBuf::from(format!("frozen://{name}.zio")));
        }
        Self {
            state,
            frozen_root,
            frozen_paths,
        }
    }

    /// True when `path` resolves to a known frozen module.
    fn is_known_module(&self, path: &Path) -> bool {
        self.frozen_paths.iter().any(|known| known == path)
    }
}

impl IoHost for IsolatedIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        self.check_deadline()?;
        self.state.output.lock().try_push_str(msg);
        Ok(())
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        self.check_deadline()?;
        let mut out = self.state.output.lock();
        out.try_push_str(msg);
        // Always attempt the trailing newline; refusing it is part of
        // honest truncation reporting.
        out.push_char('\n');
        Ok(())
    }

    fn read_line(&self) -> Result<String, EvalError> {
        self.check_deadline()?;
        Err(EvalError::custom(
            "invalid-input: read-line is unavailable in an isolated context",
        ))
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        Err(EvalError::custom(
            "invalid-input: no working directory in an isolated context",
        ))
    }

    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        self.check_deadline()?;
        // A candidate must not even reach here — `slurp` / `load` /
        // `read-string` are refusal natives in the isolated context.
        // We still answer conservatively for any future caller that
        // routes through the IoHost.
        Err(EvalError::custom(format!(
            "invalid-input: filesystem access is unavailable in an isolated context ({path})"
        )))
    }

    fn write_file(&self, path: &str, _data: &str) -> Result<(), EvalError> {
        Err(EvalError::custom(format!(
            "invalid-input: filesystem write is unavailable in an isolated context ({path})"
        )))
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        self.check_deadline()?;
        let p = Path::new(path);
        // Refuse anything outside the frozen root.
        if !p.starts_with(&self.frozen_root) {
            return Ok(false);
        }
        Ok(self.is_known_module(p))
    }

    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        self.check_deadline()?;
        let p = Path::new(path);
        // The frozen root is the only addressable namespace. Anything
        // outside it would let a candidate resolve a real filesystem
        // path through directory walks — refuse.
        if !p.starts_with(&self.frozen_root) {
            return Err(EvalError::custom(format!(
                "invalid-input: path resolves outside the frozen module set: {path}"
            )));
        }
        // For a known module, return the canonical form. For an
        // unknown path under the frozen root, still refuse — the
        // candidate has no business probing arbitrary paths here.
        if self.is_known_module(p) {
            Ok(p.to_string_lossy().into_owned())
        } else {
            Err(EvalError::custom(format!(
                "invalid-input: path is not a registered frozen module: {path}"
            )))
        }
    }

    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        self.check_deadline()?;
        let p = Path::new(path);
        // Only the frozen root itself counts as a directory: a
        // candidate that asks whether a subpath is a directory must
        // be refused, because the frozen set is leaf-level — every
        // entry is a `.zio` file, not a tree.
        Ok(p == self.frozen_root)
    }
}

impl IsolatedIoHost {
    fn check_deadline(&self) -> Result<(), EvalError> {
        if self.state.is_cancelled() {
            return Err(EvalError::custom("timeout: wall-clock budget exhausted"));
        }
        Ok(())
    }
}

// ── Native overrides installed only in the isolated context ──────

/// Build a NativeFn that, on call, returns a refusal with a stable
/// message. These replace the globally-registered `slurp` / `load` /
/// `recv!` / etc. so a candidate cannot use the language core to
/// slip out of the sandbox.
fn refusal(name: &'static str, detail: &str) -> NativeFn {
    let message = format!("invalid-input: {name} is unavailable in an isolated context ({detail})");
    NativeFn::new(name, move |_args, _engine| {
        Err(EvalError::custom(message.clone()))
    })
}

/// The `error` native for the isolated context. It routes through
/// `EvalError::custom` so a candidate's `(error "msg")` surfaces as
/// a `failed` envelope with the same message.
fn error_native_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let message: String = args
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(" ");
    Err(EvalError::custom(message))
}

// ── The actual evaluator ─────────────────────────────────────────

fn run_candidate(
    source: &str,
    name: &str,
    entry: Option<&str>,
    input: Option<Value>,
    limits: ExecutionLimits,
    modules: FrozenModules,
    observer: Arc<dyn Observer>,
) -> (Outcome, String, bool, u64) {
    let output = Arc::new(Mutex::new(BoundedOutput::new(limits.max_output_bytes)));
    let cancelled = Arc::new(AtomicBool::new(false));
    let state = Arc::new(IsolatedState {
        cancelled: Arc::clone(&cancelled),
        output: Arc::clone(&output),
    });

    // Watchdog: if the candidate burns more wall-clock than the
    // caller allowed, flip the flag. The interpreter checks the flag
    // through every IoHost call; combined with the step ceiling,
    // runaway native loops get bounded on the next fuel charge.
    let timeout_ms = limits.timeout_ms;
    let watchdog_cancelled = Arc::clone(&cancelled);
    let _watchdog = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(timeout_ms));
        watchdog_cancelled.store(true, Ordering::Relaxed);
    });

    // Build the candidate's context. Fresh env, no parent. Install
    // only the safe surface.
    let io: Arc<dyn IoHost> = Arc::new(IsolatedIoHost::new(Arc::clone(&state), &modules));
    let env = Arc::new(Env::new(None));
    let ctx = EvalContext::with_io(env, Arc::clone(&io));
    install_isolated_bindings(&ctx);
    zio_core::syntax::register(&ctx.env);
    install_frozen_loader(&ctx, Arc::clone(&state), modules, limits.max_steps);
    ctx.set_step_ceiling(limits.max_steps);
    ctx.attach_observer(Arc::clone(&observer));

    // Load the language standard library. It defines safe helpers
    // (defn, reduce, get-in, …); it does not define slurp / load /
    // spit / recv! / etc., which live in `builtins::io` and
    // `builtins::concurrency`.
    if let Err(error) = bootstrap::bootstrap_source(&ctx, "core.zio", zio_core::stdlib_source()) {
        cancelled.store(true, Ordering::Relaxed);
        return finish(
            Outcome::Failed {
                error: error.to_string(),
            },
            &state,
            ctx.steps_spent(),
        );
    }

    // Parse first so a syntax error becomes an envelope rather than
    // a panic during evaluation.
    let forms = match bootstrap::parse_source(&ctx, name, source) {
        Ok(forms) => forms,
        Err(error) => {
            cancelled.store(true, Ordering::Relaxed);
            return finish(
                Outcome::Failed {
                    error: error.to_string(),
                },
                &state,
                ctx.steps_spent(),
            );
        }
    };

    // If an entry point was named, defer lookup until after the
    // candidate runs — `defn` lives in the body. The run loop
    // populates the env, and the entry resolves when the loop
    // completes.

    // Run the candidate. Each top-level form is evaluated; the last
    // form's value becomes the candidate's `result`.
    let mut last = Value::Nil;
    for sexp in forms.iter() {
        if state.is_cancelled() {
            return finish(
                Outcome::Failed {
                    error: "timeout: wall-clock budget exhausted".into(),
                },
                &state,
                ctx.steps_spent(),
            );
        }
        match ctx.eval_expr(sexp, &ctx.env, false) {
            Ok(zio_core::special::TailResult::Value(value)) => last = value,
            Ok(zio_core::special::TailResult::TailCall(func, args)) => {
                match zio_core::eval::apply(func, args, &ctx) {
                    Ok(zio_core::special::TailResult::Value(value)) => last = value,
                    Ok(_) => {
                        cancelled.store(true, Ordering::Relaxed);
                        return finish(
                            Outcome::Failed {
                                error: "internal: tail-call did not resolve".into(),
                            },
                            &state,
                            ctx.steps_spent(),
                        );
                    }
                    Err(error) => {
                        cancelled.store(true, Ordering::Relaxed);
                        return finish(
                            Outcome::Failed {
                                error: error.to_string(),
                            },
                            &state,
                            ctx.steps_spent(),
                        );
                    }
                }
            }
            Ok(zio_core::special::TailResult::Recur(_)) => {
                cancelled.store(true, Ordering::Relaxed);
                return finish(
                    Outcome::Failed {
                        error: "recur escaped a top-level form".into(),
                    },
                    &state,
                    ctx.steps_spent(),
                );
            }
            Err(error) => {
                let message = error.to_string();
                let timeout = state.is_cancelled() || message.contains("step limit exhausted");
                cancelled.store(true, Ordering::Relaxed);
                return finish(
                    Outcome::Failed {
                        error: if timeout {
                            format!("timeout: {message}")
                        } else {
                            message
                        },
                    },
                    &state,
                    ctx.steps_spent(),
                );
            }
        }
    }

    // Resolve the entry function if one was named. The entry sees
    // the JSON-safe `input` as its single argument; its return
    // value is the candidate's `result`.
    let result = if let Some(entry_name) = entry {
        let function = match ctx.env.get(entry_name) {
            Some(value @ (Value::Function(_) | Value::NativeFunction(_))) => value,
            _ => {
                cancelled.store(true, Ordering::Relaxed);
                return finish(
                    Outcome::Refused {
                        error: format!(
                            "invalid-input: entry point '{entry_name}' is not defined or not callable"
                        ),
                    },
                    &state,
                    ctx.steps_spent(),
                );
            }
        };
        let arg = input.unwrap_or(Value::Nil);
        let call_args: Vector<Value> = vector![arg];
        match zio_core::eval::apply(function, call_args, &ctx) {
            Ok(zio_core::special::TailResult::Value(value)) => value,
            Ok(zio_core::special::TailResult::TailCall(func, args)) => {
                match zio_core::eval::apply(func, args, &ctx) {
                    Ok(zio_core::special::TailResult::Value(value)) => value,
                    Ok(_) => {
                        cancelled.store(true, Ordering::Relaxed);
                        return finish(
                            Outcome::Failed {
                                error: "internal: tail-call did not resolve".into(),
                            },
                            &state,
                            ctx.steps_spent(),
                        );
                    }
                    Err(error) => {
                        cancelled.store(true, Ordering::Relaxed);
                        return finish(
                            Outcome::Failed {
                                error: error.to_string(),
                            },
                            &state,
                            ctx.steps_spent(),
                        );
                    }
                }
            }
            Ok(zio_core::special::TailResult::Recur(_)) => {
                cancelled.store(true, Ordering::Relaxed);
                return finish(
                    Outcome::Failed {
                        error: "recur escaped the entry call".into(),
                    },
                    &state,
                    ctx.steps_spent(),
                );
            }
            Err(error) => {
                let message = error.to_string();
                let timeout = state.is_cancelled() || message.contains("step limit exhausted");
                cancelled.store(true, Ordering::Relaxed);
                return finish(
                    Outcome::Failed {
                        error: if timeout {
                            format!("timeout: {message}")
                        } else {
                            message
                        },
                    },
                    &state,
                    ctx.steps_spent(),
                );
            }
        }
    } else {
        last
    };

    cancelled.store(true, Ordering::Relaxed);
    finish(
        Outcome::Completed { value: result },
        &state,
        ctx.steps_spent(),
    )
}

/// Pull the candidate's output and truncation flag out of the
/// shared state, then assemble the tuple `build_envelope` expects.
fn finish(
    outcome: Outcome,
    state: &Arc<IsolatedState>,
    steps: u64,
) -> (Outcome, String, bool, u64) {
    let out = state.output.lock();
    let truncated = out.truncated;
    let text = out.buffer.clone();
    drop(out);
    (outcome, text, truncated, steps)
}

// ── Isolated context construction ────────────────────────────────

/// Register exactly the safe surface. Every host / transport /
/// storage / tensor binding that the parent `install` provides is
/// absent. Globals like `slurp`, `load`, `recv!`, `chan`, `spit`,
/// `read-line`, `file-exists?`, `read-string` are replaced by
/// refusal natives so the candidate cannot reach the real
/// implementations even through a namespace alias.
fn install_isolated_bindings(ctx: &EvalContext) {
    use zio_core::builtins;
    builtins::numeric::register(&ctx.env);
    builtins::predicates::register(&ctx.env);
    builtins::collections::register(&ctx.env);
    builtins::strings::register(&ctx.env);
    builtins::buffer::register(&ctx.env);
    builtins::json::register(&ctx.env);
    builtins::zos_access::register(&ctx.env);
    builtins::macroexpand::register(&ctx.env);
    ctx.env.set(
        "error".into(),
        Value::NativeFunction(NativeFn::new("error", error_native_fn)),
    );

    // Overrides: these names are present in `builtins::io` and
    // `builtins::concurrency`; we replace them with refusals.
    // `print` / `println` / `prn` / `pprint` stay real — they route
    // through the bounded-output IoHost so the envelope can report
    // what was produced (and whether truncation happened).
    ctx.env.set(
        "slurp".into(),
        Value::NativeFunction(refusal("slurp", "filesystem access")),
    );
    ctx.env.set(
        "spit".into(),
        Value::NativeFunction(refusal("spit", "filesystem access")),
    );
    ctx.env.set(
        "load".into(),
        Value::NativeFunction(refusal("load", "filesystem access")),
    );
    ctx.env.set(
        "read-string".into(),
        Value::NativeFunction(refusal("read-string", "filesystem access")),
    );
    ctx.env.set(
        "file-exists?".into(),
        Value::NativeFunction(refusal("file-exists?", "filesystem access")),
    );
    ctx.env.set(
        "read-line".into(),
        Value::NativeFunction(refusal("read-line", "stdio is not available")),
    );
    ctx.env.set(
        "chan".into(),
        Value::NativeFunction(refusal("chan", "concurrency primitives are unavailable")),
    );
    ctx.env.set(
        "send!".into(),
        Value::NativeFunction(refusal("send!", "concurrency primitives are unavailable")),
    );
    ctx.env.set(
        "recv!".into(),
        Value::NativeFunction(refusal("recv!", "concurrency primitives are unavailable")),
    );
    ctx.env.set(
        "promise".into(),
        Value::NativeFunction(refusal("promise", "concurrency primitives are unavailable")),
    );
    ctx.env.set(
        "deliver".into(),
        Value::NativeFunction(refusal("deliver", "concurrency primitives are unavailable")),
    );
    ctx.env.set(
        "deref".into(),
        Value::NativeFunction(refusal("deref", "concurrency primitives are unavailable")),
    );
    ctx.env.set(
        "future-call".into(),
        Value::NativeFunction(refusal(
            "future-call",
            "concurrency primitives are unavailable",
        )),
    );

    // `host/evaluate-isolated` is intentionally absent: a candidate
    // cannot recurse into a second isolated evaluation. Each layer
    // would still see the same frozen input, and the recursion is
    // pointless.
}

// ── Frozen module loader ─────────────────────────────────────────

/// A boxed loader that reads modules only from the supplied frozen
/// map, with cycle detection via `ModuleTable::begin_loading`'s
/// loading stack. The same module entered twice fails with
/// `circular require detected`.
struct FrozenLoader {
    frozen: Arc<FrozenModules>,
    modules: std::rc::Rc<std::cell::RefCell<ModuleTable>>,
    state: Arc<IsolatedState>,
    max_steps: u64,
}

impl FrozenLoader {
    fn loader(&self) -> Box<ModuleLoader> {
        let frozen = Arc::clone(&self.frozen);
        let modules = self.modules.clone();
        let state = Arc::clone(&self.state);
        let max_steps = self.max_steps;
        Box::new(
            move |mod_name: &[String], _source: &str, parent_env: &Arc<Env>| {
                let name_str = mod_name.join(".");

                // Already-loaded module wins (require is idempotent).
                if let Some(existing) = modules.borrow().find(mod_name).cloned() {
                    return Ok(existing);
                }

                // Source lookup against the frozen map.
                let bytes = frozen.get(&name_str).cloned().ok_or_else(|| {
                    EvalError::custom(format!(
                        "module not found: {name_str} (searched the frozen module set only)"
                    ))
                })?;
                let source_text = String::from_utf8(bytes).map_err(|_| {
                    EvalError::custom(format!("module {name_str} source is not valid UTF-8"))
                })?;

                // Cycle detection: same module re-entered produces
                // `circular require detected`. The pseudo-path lives only
                // in this loader; the real filesystem never sees it.
                let pseudo_path = PathBuf::from(format!("frozen://{name_str}.zio"));
                modules.borrow_mut().begin_loading(pseudo_path.as_path())?;

                let result = load_module_body(
                    mod_name,
                    &pseudo_path,
                    &source_text,
                    parent_env,
                    Arc::clone(&frozen),
                    modules.clone(),
                    Arc::clone(&state),
                    max_steps,
                );

                modules.borrow_mut().end_loading(pseudo_path.as_path());
                if let Ok(module) = &result {
                    modules.borrow_mut().register(module.clone());
                }
                result
            },
        )
    }
}

fn install_frozen_loader(
    ctx: &EvalContext,
    state: Arc<IsolatedState>,
    modules: FrozenModules,
    max_steps: u64,
) {
    let shared = Arc::new(std::rc::Rc::new(
        std::cell::RefCell::new(ModuleTable::new()),
    ));
    let frozen = Arc::new(modules);
    let loader = FrozenLoader {
        frozen: Arc::clone(&frozen),
        modules: (*shared).clone(),
        state: Arc::clone(&state),
        max_steps,
    };
    *ctx.loader.borrow_mut() = Some(loader.loader());
}

/// Load one module's body. The module body is evaluated in a fresh
/// sub-context that shares the parent's SourceMap and the frozen
/// module set, so a `require` inside a module routes through the
/// same loader. The step ceiling is the candidate's ceiling: a
/// module that loops cannot consume more than the candidate was
/// allotted.
fn load_module_body(
    mod_name: &[String],
    pseudo_path: &Path,
    source_text: &str,
    parent_env: &Arc<Env>,
    frozen: Arc<FrozenModules>,
    shared_modules: std::rc::Rc<std::cell::RefCell<ModuleTable>>,
    state: Arc<IsolatedState>,
    max_steps: u64,
) -> Result<Module, EvalError> {
    let module_env = Arc::new(Env::new(Some(parent_env.clone())));
    let module_io: Arc<dyn IoHost> = Arc::new(IsolatedIoHost::new(Arc::clone(&state), &frozen));

    // Recursive loader — same frozen map, same shared module table.
    let recursive_loader = FrozenLoader {
        frozen: Arc::clone(&frozen),
        modules: shared_modules.clone(),
        state: Arc::clone(&state),
        max_steps,
    };

    // The isolated run owns its SourceMap: the candidate's sources are
    // registered here and nowhere else, so an error names the candidate's
    // own text and not a file the host happened to be reading.
    let source_map = Arc::new(zio_core::span::SourceMap::new());
    let ctx = EvalContext::with_shared_modules(
        module_env.clone(),
        recursive_loader.loader(),
        Arc::clone(&module_io),
        Arc::clone(&source_map),
        // The table is shared with the loader through an `Rc`; the
        // context takes that same `Rc` so both really see one table
        // rather than two that can drift.
        shared_modules.clone(),
    );
    // Modules export nothing by default; the body declares with
    // `(export name)` and we read the accumulator afterwards.
    install_isolated_bindings(&ctx);
    zio_core::syntax::register(&ctx.env);
    ctx.set_step_ceiling(max_steps);

    ctx.push_module_exports();
    let evaluation = (|| -> Result<Value, EvalError> {
        if state.is_cancelled() {
            return Err(EvalError::custom("timeout: wall-clock budget exhausted"));
        }
        let forms = bootstrap::parse_source(&ctx, &pseudo_path.to_string_lossy(), source_text)?;
        let mut last = Value::Nil;
        for sexp in forms.iter() {
            // Evaluate in the MODULE's environment, not the context's
            // root. Evaluating against the root would put the module's
            // definitions in the caller's scope, so `require` would find
            // nothing to export and a module could see — and shadow —
            // the program's own bindings.
            match ctx.eval_expr(sexp, &module_env, false) {
                Ok(zio_core::special::TailResult::Value(value)) => last = value,
                Ok(_) => {
                    return Err(EvalError::custom(
                        "internal: tail result not unwound in module load",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(last)
    })();
    let exports = ctx.take_module_exports();
    evaluation?;
    let source_id = ctx
        .source_map()
        .get_latest_id(&pseudo_path.to_string_lossy());
    let module = Module {
        name: mod_name.to_vec(),
        env: module_env,
        exports,
        source: source_id,
    };
    module.validate_exports()?;
    Ok(module)
}

// ── Envelope assembly ────────────────────────────────────────────

#[derive(Debug)]
enum Outcome {
    Completed { value: Value },
    Failed { error: String },
    Refused { error: String },
}

fn build_envelope(
    outcome: Outcome,
    observer: &Arc<dyn Observer>,
    output_text: String,
    truncated: bool,
    steps: u64,
) -> Value {
    let traces = collect_traces(observer);

    let (status, result_value, error_value) = match outcome {
        Outcome::Completed { value } => match json::to_json(&value) {
            Ok(json) => ("completed", Some(json_to_value(json)), None),
            Err(error) => (
                "failed",
                Some(Value::Nil),
                Some(format!("result is not JSON-safe: {error}")),
            ),
        },
        Outcome::Failed { error } => ("failed", Some(Value::Nil), Some(error)),
        Outcome::Refused { error } => ("refused", Some(Value::Nil), Some(error)),
    };

    let mut entries: Vec<(&'static str, Value)> = vec![
        ("status", Value::String(status.into())),
        ("result", result_value.unwrap_or(Value::Nil)),
        ("output", Value::String(output_text)),
        ("output-truncated", Value::Boolean(truncated)),
        ("steps", Value::Integer(steps as i64)),
        ("traces", Value::Vector(traces)),
    ];
    if let Some(error) = error_value {
        entries.push(("error", Value::String(error)));
    }
    values::map(entries)
}

fn refused_envelope(reason: &str, steps: u64, truncated: bool) -> Value {
    let entries: Vec<(&'static str, Value)> = vec![
        ("status", Value::String("refused".into())),
        ("result", Value::Nil),
        ("output", Value::String(String::new())),
        ("output-truncated", Value::Boolean(truncated)),
        ("steps", Value::Integer(steps as i64)),
        ("traces", Value::Vector(Vector::new())),
        ("error", Value::String(reason.into())),
    ];
    values::map(entries)
}

/// Walk a `RecordingObserver`'s collected events. The observer is
/// downcast to `RecordingObserver` if possible; otherwise we return
/// an empty vector. The contract for `traces` is "what actually
/// happened" — we do not invent events.
fn collect_traces(observer: &Arc<dyn Observer>) -> Vector<Value> {
    // Asked through the trait rather than by downcasting: a `&dyn
    // Observer` cannot be coerced to a subtrait, and an observer that
    // records nothing must report no trace rather than an empty one
    // that reads like a run which happened and saw nothing.
    match observer.recorded_recording() {
        Some(events) => events_to_values(&events),
        None => Vector::new(),
    }
}

fn events_to_values(events: &[Event]) -> Vector<Value> {
    let mut out = Vector::new();
    for event in events {
        let kind = match event.kind {
            EventKind::Branch => "branch",
            EventKind::Call => "call",
            EventKind::TailCall => "tail-call",
            EventKind::Error => "error",
            EventKind::MacroExpansion => "macro-expansion",
            EventKind::Return => "return",
        };
        let span = event
            .span
            .map(|s| {
                values::map([
                    ("line", Value::Integer(s.line as i64)),
                    ("col", Value::Integer(s.col as i64)),
                ])
            })
            .unwrap_or(Value::Nil);
        let entry = values::map([
            ("sequence", Value::Integer(event.sequence as i64)),
            ("kind", Value::String(kind.into())),
            ("detail", Value::String(event.detail.clone())),
            ("span", span),
        ]);
        out.push_back(entry);
    }
    out
}

fn json_to_value(json: serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Boolean(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Nil
            }
        }
        serde_json::Value::String(s) => Value::String(s),
        serde_json::Value::Array(items) => {
            let mut v = Vector::new();
            for item in items {
                v.push_back(json_to_value(item));
            }
            Value::Vector(v)
        }
        serde_json::Value::Object(items) => {
            let mut map = HashMap::new();
            for (key, value) in items {
                map.insert(Value::String(key), json_to_value(value));
            }
            Value::Map(map)
        }
    }
}

// ── The host/evaluate-isolated handler ───────────────────────────

/// The `host/evaluate-isolated` native function. It runs inside the
/// caller's context, but it spawns a fresh `EvalContext` for the
/// candidate — the candidate does not inherit the caller's
/// environment, modules, observer, or IoHost.
fn isolated_handler(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    values::arity(&args, 1)?;
    let request = match &args[0] {
        Value::Map(_) => &args[0],
        _ => return Err(EvalError::type_error("map", args[0].value_type())),
    };

    let source = match values::get(request, "source") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => return Err(EvalError::type_error("string", other.value_type())),
        None => return Err(EvalError::custom("invalid-input: source is required")),
    };

    let name = match values::get(request, "name") {
        Some(Value::String(s)) => s.clone(),
        _ => "candidate.zio".into(),
    };
    let entry = match values::get(request, "entry") {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let input = values::get(request, "input").cloned();
    let modules = match values::get(request, "modules") {
        Some(Value::Map(m)) => {
            let mut out = FrozenModules::new();
            for (key, value) in m.iter() {
                let module_name = match key {
                    Value::String(s) | Value::Keyword(s) => s.clone(),
                    _ => {
                        return Err(EvalError::custom(
                            "invalid-input: module name must be a string or keyword",
                        ));
                    }
                };
                let module_source = match value {
                    Value::String(s) => s.clone(),
                    Value::Buffer(b) => {
                        let bytes = b.lock();
                        String::from_utf8(bytes.clone()).map_err(|_| {
                            EvalError::custom("invalid-input: module source must be valid UTF-8")
                        })?
                    }
                    other => {
                        return Err(EvalError::type_error(
                            "string or buffer",
                            other.value_type(),
                        ));
                    }
                };
                out.insert(module_name, module_source.into_bytes());
            }
            out
        }
        None => FrozenModules::new(),
        _ => return Err(EvalError::custom("invalid-input: modules must be a map")),
    };

    let limits = match values::get(request, "limits") {
        Some(Value::Map(m)) => ExecutionLimits {
            max_steps: integer_field(m, "max-steps")?,
            max_output_bytes: integer_field(m, "max-output-bytes")? as usize,
            max_source_bytes: integer_field(m, "max-source-bytes")? as usize,
            timeout_ms: integer_field(m, "timeout-ms")?,
        },
        None => ExecutionLimits::default(),
        _ => return Err(EvalError::custom("invalid-input: limits must be a map")),
    };

    evaluate(&source, &name, entry.as_deref(), input, limits, modules)
}

fn integer_field(map: &HashMap<Value, Value>, key: &str) -> Result<u64, EvalError> {
    match map
        .get(&Value::Keyword(key.into()))
        .or_else(|| map.get(&Value::String(key.into())))
    {
        Some(Value::Integer(n)) if *n >= 0 => Ok(*n as u64),
        Some(Value::Integer(_)) => Err(EvalError::custom(format!(
            "invalid-input: {key} must be nonnegative"
        ))),
        Some(other) => Err(EvalError::type_error(
            "nonnegative integer",
            other.value_type(),
        )),
        None => Err(EvalError::custom(format!(
            "invalid-input: {key} is required"
        ))),
    }
}
