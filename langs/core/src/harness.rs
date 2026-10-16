//! ZIO execution harness — minimal scaffold (ZIO-008 Slice 1).
//! See docs/superpowers/specs/2026-10-16-zio-execution-harness-contract.md.
//!
//! Slice 1 delivers the typed session surface: session/run identity, the
//! capability set (declared, not enforced), compile/eval envelopes, and the
//! cancel flag. Fuel budgeting, output capture, capability enforcement, and
//! image restore land in later slices.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::context::EvalRuntime;
use crate::error::EvalError;
use crate::span::Span;
use crate::value::Value;

/// Unique session id (process-wide counter; contract §2).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct SessionId(pub u64);

/// Unique run id within a session (contract §3). Every compile/eval/run
/// call produces one, and the result envelope returns it unchanged.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct RunId(pub u64);

/// Capability bits the launcher grants to a session (contract §4).
///
/// Slice 1 declares the type and the launcher-facing surface; the harness
/// itself never grants capabilities and does not enforce them yet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CapabilitySet {
    bits: u64,
}

impl CapabilitySet {
    pub fn empty() -> Self {
        Self { bits: 0 }
    }
    pub fn with(bit: Capability) -> Self {
        Self {
            bits: 1 << bit as u64,
        }
    }
    pub fn contains(&self, bit: Capability) -> bool {
        self.bits & (1 << bit as u64) != 0
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum Capability {
    FileRead = 0,
    FileWrite = 1,
    Network = 2,
    ProcessSpawn = 3,
    JsEngine = 4,
}

/// Compile result envelope (contract §5). Compilation is a static phase:
/// no user code runs, so there is no output stream on this envelope.
#[derive(Debug)]
pub struct CompileResult {
    pub module: Option<Value>,
    pub exports: Vec<String>,
    pub dependencies: Vec<String>,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
    pub run_id: RunId,
}

/// Run result envelope (contract §6). `value` and `error` are mutually
/// exclusive; when both are present, `error` wins.
#[derive(Debug)]
pub struct RunResult {
    pub value: Option<Value>,
    pub error: Option<EvalError>,
    pub output: Vec<String>,
    pub events: Vec<ObserverEvent>,
    pub fuel_spent: u64,
    pub run_id: RunId,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Debug, Clone)]
pub struct ObserverEvent {
    pub kind: String,
    pub span: Option<Span>,
}

/// Session configuration.
#[derive(Clone, Debug, Default)]
pub struct SessionDefaults {
    /// Fuel cap. None = use eval's step_limit default.
    pub fuel: Option<u64>,
}

/// Session state (contract §2).
///
/// Sessions hold almost no state: the immutable capability set, budget
/// defaults, the cancel flag, and a poison marker. Each run executes in a
/// fresh `language_context`, so no user-visible state leaks between runs.
pub struct Session {
    pub id: SessionId,
    capabilities: CapabilitySet,
    /// Budget defaults held per contract §2; fuel wiring lands in Slice 2.
    _defaults: SessionDefaults,
    cancel_flag: Arc<AtomicBool>,
    poisoned: bool,
    /// Module registry (Slice 1: empty; reserved for later).
    _modules: Mutex<Vec<Value>>,
}

impl Session {
    /// Create a new session. Capabilities are granted by the launcher and
    /// are immutable for the session's lifetime (contract §4).
    pub fn new(capabilities: CapabilitySet, defaults: SessionDefaults) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: SessionId(NEXT.fetch_add(1, Ordering::Relaxed)),
            capabilities,
            _defaults: defaults,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            poisoned: false,
            _modules: Mutex::new(Vec::new()),
        }
    }

    pub fn capabilities(&self) -> &CapabilitySet {
        &self.capabilities
    }
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
    /// The flag the observer will poll at fuel checkpoints in a later
    /// slice. Slice 1 only checks it at run boundaries.
    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }

    /// Compile source without executing it (contract §5).
    pub fn compile(&self, name: &str, source: &str) -> CompileResult {
        let run_id = RunId::new();
        let ctx = match crate::bootstrap::language_context(crate::bootstrap::ModuleRoots::empty()) {
            Ok(c) => c,
            Err(e) => return CompileResult::err(run_id, vec![diag_from(e, None)]),
        };
        match crate::bytecode::compile_source(&ctx, name, source) {
            Ok(value) => CompileResult {
                module: Some(value),
                // The compiler emits no export/dependency tables yet;
                // module metadata wiring lands with the module registry.
                exports: Vec::new(),
                dependencies: Vec::new(),
                warnings: Vec::new(),
                errors: Vec::new(),
                run_id,
            },
            Err(e) => CompileResult::err(run_id, vec![diag_from(e, None)]),
        }
    }

    /// Evaluate source in a fresh context, returning the last value
    /// (contract §6). Output capture is a later slice: `output` stays
    /// empty and the CLI still prints directly.
    pub fn eval(&self, name: &str, source: &str) -> RunResult {
        self.run_with(name, source, |ctx, name, src| {
            crate::bootstrap::eval_source(ctx, name, src)
        })
    }

    /// Run a `.zio-image` (contract §14). Image restore lands in Slice 2.
    pub fn run_image(&self, _image_path: &Path) -> RunResult {
        RunResult::err(
            RunId::new(),
            EvalError::custom("image run not yet implemented (ZIO-008 Slice 1)"),
        )
    }

    /// Cancel a pending run. Consumed by the next `eval` boundary.
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::SeqCst);
    }

    /// Close the session, releasing resources.
    pub fn close(self) {
        // Slice 1 holds no resources beyond self-drop.
        drop(self);
    }

    fn run_with(
        &self,
        name: &str,
        source: &str,
        runner: impl FnOnce(&crate::context::EvalContext, &str, &str) -> Result<Value, EvalError>,
    ) -> RunResult {
        let run_id = RunId::new();
        // Consume a pending cancel at the boundary: a run that has not
        // started is cancelled cleanly (contract §9, matrix row 1).
        if self.cancel_flag.swap(false, Ordering::SeqCst) {
            return RunResult::err(run_id, EvalError::cancelled(0));
        }

        let ctx = match crate::bootstrap::language_context(crate::bootstrap::ModuleRoots::empty()) {
            Ok(c) => c,
            Err(e) => return RunResult::err(run_id, e),
        };
        let result = runner(&ctx, name, source);
        let fuel_spent = ctx.observation().fuel_spent();
        let output = Vec::new();

        let cancel_requested = self.cancel_flag.swap(false, Ordering::SeqCst);
        if cancel_requested {
            return RunResult::err(run_id, EvalError::cancelled(fuel_spent));
        }

        match result {
            Ok(value) => RunResult {
                value: Some(value),
                error: None,
                output,
                events: Vec::new(),
                fuel_spent,
                run_id,
            },
            Err(error) => RunResult {
                value: None,
                error: Some(error),
                output,
                events: Vec::new(),
                fuel_spent,
                run_id,
            },
        }
    }
}

impl RunId {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl CompileResult {
    fn err(run_id: RunId, errors: Vec<Diagnostic>) -> Self {
        CompileResult {
            module: None,
            exports: Vec::new(),
            dependencies: Vec::new(),
            warnings: Vec::new(),
            errors,
            run_id,
        }
    }
}

impl RunResult {
    fn err(run_id: RunId, error: EvalError) -> Self {
        RunResult {
            value: None,
            error: Some(error),
            output: Vec::new(),
            events: Vec::new(),
            fuel_spent: 0,
            run_id,
        }
    }
}

fn diag_from(error: EvalError, span: Option<Span>) -> Diagnostic {
    Diagnostic {
        message: error.to_string(),
        span,
    }
}
