//! ZIO execution harness (ZIO-008).
//! See docs/superpowers/specs/2026-10-16-zio-execution-harness-contract.md.
//!
//! Slice 1 delivered the typed session surface: session/run identity, the
//! capability set, compile/eval envelopes, and the cancel flag. Slice 2
//! lands the real `eval` — output capture through [`BudgetedIoHost`],
//! capability gates through [`CapabilityEnforcingIoHost`], and the
//! fuel + wall-clock budgets — plus `run_image` (contract §14) and the
//! typed `CapabilityDenied`/`TimedOut` errors. Process-group kill
//! isolation on non-cancellable calls and observer `events[]` wiring
//! remain for Slice 3.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::error::EvalError;
use crate::io::{BufferIoHost, IoHost};
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
/// The set is fixed at session construction; the harness never grants a
/// bit itself. Slice 1 declared the surface; Slice 2 enforces it through
/// [`CapabilityEnforcingIoHost`].
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

/// Harness-level event on a run envelope (contract §6/§7). Observer
/// wiring of the evaluator event stream is a Slice 3 item; Slice 2 emits
/// the `OutputTruncated` warning from the budget wrapper.
#[derive(Debug, Clone)]
pub struct ObserverEvent {
    pub kind: String,
    pub span: Option<Span>,
}

/// Budgets the launcher attaches to a session (contract §8). Every item
/// is opt-in: `None` means "use the default" or "do not check".
#[derive(Clone, Debug, Default)]
pub struct Budgets {
    /// Fuel cap in evaluator nodes. `None` = use the eval default.
    pub fuel: Option<u64>,
    /// Wall-clock cap. `None` = unbounded (do not check).
    pub max_duration: Option<Duration>,
    /// Per-stream output cap. `None` = unbounded.
    pub max_output_bytes: Option<usize>,
}

/// Session configuration.
#[derive(Clone, Debug, Default)]
pub struct SessionDefaults {
    /// Fuel cap. None = use eval's step_limit default.
    pub fuel: Option<u64>,
    /// Wall-clock cap for a run. `None` = unbounded (do not check).
    pub max_duration: Option<Duration>,
    /// Per-stream output cap (contract §7). `None` = unbounded.
    pub max_output_bytes: Option<usize>,
}

impl SessionDefaults {
    /// Construct with explicit budgets.
    pub fn with_budgets(budgets: Budgets) -> Self {
        Self {
            fuel: budgets.fuel,
            max_duration: budgets.max_duration,
            max_output_bytes: budgets.max_output_bytes,
        }
    }
}

/// Session state (contract §2).
///
/// Sessions hold almost no state: the immutable capability set, budget
/// defaults, the cancel flag, and a poison marker. Each run executes in a
/// fresh `language_context`, so no user-visible state leaks between runs.
pub struct Session {
    pub id: SessionId,
    capabilities: CapabilitySet,
    defaults: SessionDefaults,
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
            defaults,
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
    /// The flag a concurrent caller can set to cancel the in-flight run.
    /// The evaluator's fuel checkpoints poll this in a later slice; the
    /// run boundary checks it now (contract §9).
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

    /// Evaluate source in a fresh `language_context`, returning the last
    /// value (contract §6). `print`/`println` are captured into
    /// `RunResult::output` and capped at `max_output_bytes` (contract
    /// §7); exceeding the cap surfaces an `OutputTruncated` event, never
    /// a run failure. The fuel ceiling is armed on the run's context,
    /// and a `max_duration` is armed as a mid-run deadline on the same
    /// fuel checkpoint — a non-terminating program is interrupted while
    /// it runs instead of after `eval_source` returns, and the run
    /// boundary converts the abort into the typed `TimedOut` error
    /// (contract §8). Both clocks ignore context bootstrap. A native
    /// call that never re-enters the evaluator cannot be preempted;
    /// process-group isolation for those is a Slice 3 item.
    pub fn eval(&self, name: &str, source: &str) -> RunResult {
        let run_id = RunId::new();
        // Consume a pending cancel at the boundary: a run that has not
        // started is cancelled cleanly (contract §9, matrix row 1).
        if self.cancel_flag.swap(false, Ordering::SeqCst) {
            return RunResult::err(run_id, EvalError::cancelled(0));
        }

        // Substrate, budget wrapper, capability gate — the gate is
        // outermost so a denied call never touches the capture, and the
        // budget wraps the substrate so every print is accounted.
        let inner: Arc<dyn IoHost> = Arc::new(BufferIoHost::new());
        let budgeted = BudgetedIoHost::new(inner, self.defaults.max_output_bytes);
        let probe = budgeted.clone();
        let gated = CapabilityEnforcingIoHost::new(Arc::new(budgeted), self.capabilities.clone());
        let io: Arc<dyn IoHost> = Arc::new(gated);

        let mut ctx = match crate::bootstrap::language_context_with_io(
            crate::bootstrap::ModuleRoots::empty(),
            io,
        ) {
            Ok(c) => c,
            Err(e) => return RunResult::err(run_id, e),
        };
        if let Some(fuel) = self.defaults.fuel {
            ctx.set_step_ceiling(fuel);
        }
        // The wall-clock clock starts at the run, not at setup: the
        // context bootstrap above runs trusted builtin code only, and a
        // budget that is eaten by setup would flake on slow machines.
        let start = Instant::now();
        if let Some(limit) = self.defaults.max_duration {
            // Arm the deadline on the fuel checkpoint right after the
            // clock starts, so `deadline.0 >= start`: when the
            // checkpoint refuses work the boundary check below is
            // guaranteed fired too.
            ctx.observation.set_deadline(limit);
        }

        let result = crate::bootstrap::eval_source(&ctx, name, source);
        let fuel_spent = ctx.steps_spent();
        let elapsed = start.elapsed();

        // Outcome priority: a cancel that landed mid-run wins, then the
        // wall-clock budget, then the program's own result.
        if self.cancel_flag.swap(false, Ordering::SeqCst) {
            return RunResult::err(run_id, EvalError::cancelled(fuel_spent));
        }
        if matches!(self.defaults.max_duration, Some(limit) if elapsed > limit) {
            return RunResult::err(run_id, EvalError::timed_out(elapsed, fuel_spent));
        }
        let output = probe.into_output_lines();
        let mut events = Vec::new();
        if probe.truncated() {
            events.push(ObserverEvent {
                kind: "OutputTruncated".into(),
                span: None,
            });
        }
        match result {
            Ok(value) => RunResult {
                value: Some(value),
                error: None,
                output,
                events,
                fuel_spent,
                run_id,
            },
            Err(error) => RunResult {
                value: None,
                error: Some(error),
                output,
                events,
                fuel_spent,
                run_id,
            },
        }
    }

    /// Run a `.zio-image` (contract §14): parse the wire image, restore
    /// it into a fresh context whose only authority is the session's
    /// granted capabilities, bind each root by name, and eval the first
    /// root as the entry point.
    pub fn run_image(&self, image_path: &Path) -> RunResult {
        let run_id = RunId::new();
        let bytes = match std::fs::read(image_path) {
            Ok(b) => b,
            Err(e) => {
                return RunResult::err(
                    run_id,
                    EvalError::io_failure(format!("cannot read {}: {e}", image_path.display())),
                );
            }
        };
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                return RunResult::err(run_id, EvalError::invalid_form("image is not UTF-8"));
            }
        };
        let image = match crate::image::wire::Image::parse(&text) {
            Ok(i) => i,
            Err(e) => return RunResult::err(run_id, EvalError::load_error(e.to_string())),
        };

        // Restore does not inherit authority: the fresh context gets the
        // session's grants and nothing else (contract §4/§14).
        let inner: Arc<dyn IoHost> = Arc::new(BufferIoHost::new());
        let gated = CapabilityEnforcingIoHost::new(inner, self.capabilities.clone());
        let io: Arc<dyn IoHost> = Arc::new(gated);
        let ctx = match crate::bootstrap::language_context_with_io(
            crate::bootstrap::ModuleRoots::empty(),
            io,
        ) {
            Ok(c) => c,
            Err(e) => return RunResult::err(run_id, e),
        };
        if let Some(fuel) = self.defaults.fuel {
            ctx.set_step_ceiling(fuel);
        }

        let roots = match crate::image::restore::restore_image(
            &ctx,
            &image,
            &crate::image::ImageLimits::default(),
        ) {
            Ok(r) => r,
            Err(e) => return RunResult::err(run_id, EvalError::load_error(e.to_string())),
        };

        // Bind each root by name; eval the first root as the entry
        // (root-names contract, same as the CLI image runner).
        for (i, value) in roots.iter().enumerate() {
            let name = image
                .root_names
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("root-{i}"));
            ctx.env.set(name, value.clone());
        }
        let entry = image
            .root_names
            .first()
            .cloned()
            .unwrap_or_else(|| "root-0".into());
        let result =
            crate::bootstrap::eval_source(&ctx, image_path.to_string_lossy().as_ref(), &entry);
        let fuel_spent = ctx.steps_spent();
        match result {
            Ok(value) => RunResult {
                value: Some(value),
                error: None,
                output: Vec::new(),
                events: Vec::new(),
                fuel_spent,
                run_id,
            },
            Err(error) => RunResult {
                value: None,
                error: Some(error),
                output: Vec::new(),
                events: Vec::new(),
                fuel_spent,
                run_id,
            },
        }
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

/// IoHost that captures `print`/`println` output into an in-memory
/// buffer with an optional byte cap (contract §7). Truncation is reported
/// as the `OutputTruncated` event at the run boundary — not as a run
/// failure; other methods delegate to the substrate host. Holding the
/// buffer behind an `Arc` lets the caller drain it after this host has
/// been moved into the wrapper stack.
#[derive(Clone)]
pub struct BudgetedIoHost {
    inner: Arc<dyn IoHost>,
    cap: Option<usize>,
    capture: Arc<Mutex<Capture>>,
}

#[derive(Default)]
struct Capture {
    bytes: usize,
    truncated: bool,
    lines: Vec<String>,
    pending: String,
}

impl BudgetedIoHost {
    pub fn new(inner: Arc<dyn IoHost>, cap: Option<usize>) -> Self {
        BudgetedIoHost {
            inner,
            cap,
            capture: Arc::new(Mutex::new(Capture::default())),
        }
    }

    /// Drain the captured output lines into the result envelope. A final
    /// `print` without a newline is flushed as its own line.
    pub fn into_output_lines(&self) -> Vec<String> {
        let mut capture = self.capture.lock();
        let pending = std::mem::take(&mut capture.pending);
        if !pending.is_empty() {
            capture.lines.push(pending);
        }
        std::mem::take(&mut capture.lines)
    }

    /// The byte cap was reached: output after the truncation point was
    /// dropped. Distinct from a budget-exhaustion failure (contract §7).
    pub fn truncated(&self) -> bool {
        self.capture.lock().truncated
    }

    /// Record one message, keeping at most the cap's worth of bytes.
    /// Truncation is at message granularity: the message that would
    /// cross the cap is dropped whole and the stream is marked
    /// truncated, so the retained bytes never exceed the cap and no
    /// mid-UTF-8 splitting is needed.
    fn record(&self, msg: &str) {
        let mut capture = self.capture.lock();
        if capture.truncated {
            return;
        }
        let Some(budget) = self.cap else {
            capture.bytes += msg.len();
            capture.pending.push_str(msg);
            return;
        };
        if capture.bytes + msg.len() > budget {
            capture.truncated = true;
            return;
        }
        capture.bytes += msg.len();
        capture.pending.push_str(msg);
    }
}

impl IoHost for BudgetedIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        self.record(msg);
        Ok(())
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        self.record(msg);
        let mut capture = self.capture.lock();
        let pending = std::mem::take(&mut capture.pending);
        capture.lines.push(pending);
        Ok(())
    }

    fn read_line(&self) -> Result<String, EvalError> {
        self.inner.read_line()
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        self.inner.current_dir()
    }

    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        self.inner.read_file(path)
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        self.inner.write_file(path, data)
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        self.inner.file_exists(path)
    }

    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        self.inner.canonicalize_path(path)
    }

    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        self.inner.is_directory(path)
    }
}

/// IoHost that gates file operations on the session's `CapabilitySet`
/// (contract §12). A call to a gated method without the bit returns a
/// typed `EvalError::CapabilityDenied` and never touches the inner host.
///
/// The substrate enforces what the `IoHost` surface can express. The
/// remaining bits (`Network`, `ProcessSpawn`, `JsEngine`) gate calls
/// that are not `IoHost` methods — loom/JS engines check the same bit at
/// their own registration boundary — so for those the gate is honored at
/// the type level: a capability the launcher did not grant is never
/// wired into the context these hosts feed.
pub struct CapabilityEnforcingIoHost {
    inner: Arc<dyn IoHost>,
    capabilities: CapabilitySet,
}

impl CapabilityEnforcingIoHost {
    pub fn new(inner: Arc<dyn IoHost>, capabilities: CapabilitySet) -> Self {
        CapabilityEnforcingIoHost {
            inner,
            capabilities,
        }
    }
}

impl IoHost for CapabilityEnforcingIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        self.inner.print(msg)
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        self.inner.println(msg)
    }

    fn read_line(&self) -> Result<String, EvalError> {
        self.inner.read_line()
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        self.inner.current_dir()
    }

    /// Gated by `Capability::FileRead` (contract §12).
    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        if !self.capabilities.contains(Capability::FileRead) {
            return Err(EvalError::capability_denied("FileRead"));
        }
        self.inner.read_file(path)
    }

    /// Gated by `Capability::FileWrite` (contract §12).
    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        if !self.capabilities.contains(Capability::FileWrite) {
            return Err(EvalError::capability_denied("FileWrite"));
        }
        self.inner.write_file(path, data)
    }

    // Lookup calls that carry no authority pass through: the substrate
    // host decides what it can answer (contract §12).
    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        self.inner.file_exists(path)
    }

    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        self.inner.canonicalize_path(path)
    }

    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        self.inner.is_directory(path)
    }
}

fn diag_from(error: EvalError, span: Option<Span>) -> Diagnostic {
    Diagnostic {
        message: error.to_string(),
        span,
    }
}
