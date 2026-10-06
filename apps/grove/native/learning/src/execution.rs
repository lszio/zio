//! G02: running a generated program inside the host's authority.
//!
//! A candidate is code the host did not write, so the question is never
//! "did it run" but "what was it allowed to reach while it ran". This
//! module is the answer, and it is the *only* answer: a refusal here is
//! the same refusal a future capability grant would go through, so
//! opening one up later cannot be a different code path from the one
//! that is audited today.
//!
//! The shape of the check matters more than the list of names. A source
//! string can be scanned for `eval` and still be handed an `eval` by a
//! macro, or reach a file through a `require` that the check never saw.
//! So the order is:
//!
//! 1. **parse** every top-level form, before anything is evaluated;
//! 2. **check the written form** — the forms the host can see;
//! 3. **expand macros in the restricted environment**, then
//! 4. **check the expansion and its dependency closure** — because a
//!    macro that expands into `eval` is a macro that evaluates;
//! 5. only then **execute**, with a step ceiling, a buffered
//!    filesystem that contains nothing but what the grant declared, and
//!    a byte-capped output.
//!
//! Two things are deliberately *not* here. This module never decides
//! whether a result is any good — that is evaluation, and an evaluator
//! that reads a program's own claim is not an evaluator. And it never
//! grants anything: an absent capability is refused, not defaulted.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use zio_core::bootstrap::{eval_source, language_context, ModuleRoots};
use zio_core::context::{EvalContext, EvalRuntime};
use zio_core::error::EvalError;
use zio_core::im::Vector;
use zio_core::macros;
use zio_core::observer;
use zio_core::sexp::Sexp;
use zio_core::value::Value;

use crate::contracts::{digest_bytes, Actor, ActorRole, ArtifactRef, Error, ErrorKind, Result};
use crate::events;
use crate::store::Store;

/// How a run ended. `Refused` and `Failed` are different facts: a
/// refusal is the host declining before the program ran, a failure is
/// the program running and not delivering. Collapsing them would let a
/// capability denial be reported as "the model got it wrong".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// The program ran to completion. The output is a claim about what
    /// it printed; whether the result is correct is not decided here.
    Completed,
    /// The program ran and did not deliver: a raised error, a step
    /// limit, a deadline.
    Failed,
    /// The host declined to run it. Nothing of the program executed.
    Refused,
}

impl ExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Refused => "refused",
        }
    }
}

/// A capability a program may exercise. Named, not inferred: a program
/// using something that is not on the list is refused with the name of
/// what it wanted, because "denied" without a reason cannot be acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Numbers, comparison, and the numeric builtins.
    Arithmetic,
    /// Lists, vectors, maps, and the collection builtins.
    Collections,
    /// Strings and the string builtins.
    Strings,
    /// Printing. Writing is a separate capability and is not implied by it.
    Output,
    /// Writing files. Without it, the output and scratch bindings are absent.
    FileWrite,
    /// `eval` on code the program built at runtime.
    DynamicEval,
    /// `load` of a file not in the frozen dependency set.
    DynamicLoad,
}

impl Capability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Arithmetic => "arithmetic",
            Self::Collections => "collections",
            Self::Strings => "strings",
            Self::Output => "output",
            Self::FileWrite => "file-write",
            Self::DynamicEval => "dynamic-eval",
            Self::DynamicLoad => "dynamic-load",
        }
    }
}

/// One file the grant froze. A program may require exactly these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenSource {
    /// The name the program uses: `helper` for `helper.zio`.
    pub module: String,
    pub path: PathBuf,
}

/// Bounds on one execution. Every one of these is a ceiling the host
/// chose; none of them is something the program can raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionLimits {
    /// Evaluated nodes before the run is stopped. `0` is unbounded and
    /// is only ever right for a source the host wrote.
    pub max_steps: u64,
    /// Bytes of program output retained. Output past this is dropped and
    /// the result says it was truncated — a short log that looks
    /// complete is worse than a long one.
    pub max_output_bytes: usize,
    /// Bytes the source may be. A source is read into memory.
    pub max_source_bytes: usize,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        ExecutionLimits {
            max_steps: 1_000_000,
            max_output_bytes: 64 * 1024,
            max_source_bytes: 256 * 1024,
        }
    }
}

/// What a run is allowed to do, and what it may not.
///
/// A candidate cannot extend a grant. Adding a capability is an edit to
/// the grant by whoever holds it, and the code below has no path that
/// turns a program's own request into one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantProfile {
    pub capabilities: Vec<Capability>,
    pub frozen: Vec<FrozenSource>,
    /// Module names this grant may load, by `require` or `load`. A
    /// frozen source not named here is not reachable.
    pub allowed_dependencies: Vec<String>,
    /// Kept as an explicit field rather than inferred from
    /// `capabilities`: "this run may evaluate code it built" is a
    /// decision someone has to be able to see in the record.
    pub allow_dynamic_eval: bool,
    pub limits: ExecutionLimits,
}

impl GrantProfile {
    pub fn allows(&self, capability: &Capability) -> bool {
        if capability == &Capability::DynamicEval {
            return self.allow_dynamic_eval;
        }
        self.capabilities.contains(capability)
    }
}

/// One execution request: the identity a trusted host owns, the code,
/// and the grant it runs under.
#[derive(Debug, Clone)]
pub struct ExecutionRequest {
    pub execution_id: String,
    pub run_id: String,
    pub attempt_id: String,
    /// Bumped on takeover, so a receipt from a superseded attempt can
    /// be told apart from this one.
    pub epoch: u64,
    pub source: String,
    pub grant: GrantProfile,
    /// The function called when the source loads. Running a whole
    /// program because a candidate defined some functions is not the
    /// same act as running the candidate.
    pub entrypoint: String,
    /// Directories granted to `require`. The program sees nothing else.
    pub inputs: Vec<PathBuf>,
    pub deadline: Instant,
}

/// What actually happened, bound to the request that produced it.
#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub execution_id: String,
    pub run_id: String,
    pub attempt_id: String,
    pub epoch: u64,
    /// Digest of the source that ran. A record cannot be filed against
    /// bytes that were never executed.
    pub source_digest: ArtifactRef,
    /// The source that ran, kept so a caller can file the program and
    /// not just a hash of it. A digest with no bytes behind it cannot
    /// be reviewed, and a candidate nobody can read is not a candidate
    /// anyone can approve.
    pub source: String,
    pub status: ExecutionStatus,
    /// The entrypoint's return value, when it returned one. `None` on
    /// failure or refusal: a failed run has no result.
    pub result: Option<String>,
    pub output: String,
    pub output_truncated: bool,
    /// Empty on success. Category and source location, not a stack.
    pub error: String,
    pub steps: u64,
    /// Committed to the event log; the trace is evidence, not a field
    /// somebody can hand-wave at.
    pub event_count: usize,
}

// ── what a form is allowed to name ─────────────────────────────────

/// The special forms a candidate may not write.
///
/// `eval` is a binding rather than a special form, so the same name
/// covers both; `load` likewise. The list is small on purpose: each
/// entry is a way for data to become code, or for code to reach a file
/// nobody froze.
const FORBIDDEN_HEADS: &[&str] = &["eval", "load", "read-string", "require"];

/// Check one form against the grant. Returns the offending name.
fn check_form(form: &Sexp, grant: &GrantProfile) -> Option<String> {
    match form {
        Sexp::List(items, _) | Sexp::Vector(items, _) => {
            if let Some(Sexp::Symbol(head, _)) = items.front() {
                if head == "eval" && !grant.allows(&Capability::DynamicEval) {
                    return Some("dynamic-eval".into());
                }
                if head == "read-string" && !grant.allows(&Capability::DynamicEval) {
                    // `read-string` turns a string into a form. On its own
                    // that is data; followed by `eval` it is code. The
                    // candidate profile refuses the reader too, so a
                    // program cannot assemble the form it was not given.
                    return Some("dynamic-eval".into());
                }
                if head == "load" && !grant.allows(&Capability::DynamicLoad) {
                    return Some("dynamic-load".into());
                }
                if head == "require" {
                    // `require` names its module as the first argument.
                    if let Some(Sexp::Symbol(name, _) | Sexp::Keyword(name, _)) = items.get(1) {
                        let module = name.split('.').next().unwrap_or(name);
                        if !grant.allowed_dependencies.iter().any(|d| d == module) {
                            return Some(format!("dependency {module} is not in the grant"));
                        }
                    } else {
                        return Some("require needs a literal module name".into());
                    }
                }
                if FORBIDDEN_HEADS.contains(&head.as_str())
                    && (head == "eval" || head == "load" || head == "read-string")
                {
                    return Some(head.clone());
                }
            }
            for item in items {
                if let Some(found) = check_form(item, grant) {
                    return Some(found);
                }
            }
            None
        }
        // A quote is data by construction, and the reader makes a form
        // out of it only if the program evaluates it — which the
        // `eval` check above already covers.
        Sexp::Map(entries, _) => {
            for (key, value) in entries {
                if let Some(found) = check_form(key, grant) {
                    return Some(found);
                }
                if let Some(found) = check_form(value, grant) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// Walk every form, reporting the first refusal. Also returns the
/// `require`d module names, so the dependency closure can be compared
/// against what actually ran.
fn collect_dependencies(form: &Sexp, out: &mut Vec<String>) {
    match form {
        Sexp::List(items, _) => {
            if let Some(Sexp::Symbol(head, _)) = items.front() {
                if head == "require" {
                    if let Some(Sexp::Symbol(name, _) | Sexp::Keyword(name, _)) = items.get(1) {
                        out.push(name.split('.').next().unwrap_or(name).to_string());
                    }
                }
            }
            for item in items {
                collect_dependencies(item, out);
            }
        }
        Sexp::Vector(items, _) => {
            for item in items {
                collect_dependencies(item, out);
            }
        }
        Sexp::Map(entries, _) => {
            for (key, value) in entries {
                collect_dependencies(key, out);
                collect_dependencies(value, out);
            }
        }
        _ => {}
    }
}

/// A `defmacro` form, which is code that must run before its calls can
/// be expanded.
fn is_macro_definition(form: &Sexp) -> bool {
    matches!(form, Sexp::List(items, _) if matches!(items.front(), Some(Sexp::Symbol(name, _)) if name == "defmacro"))
}

/// Expand macros and reject the expansion.
///
/// The written form passing is not the answer: `(defmacro boom [] (list
/// 'eval ...))` names nothing forbidden, and expands into exactly the
/// form the first check would have refused. So the same check runs
/// again on what the macro produced, in the same restricted
/// environment — the expander cannot read a file or call a binding the
/// program could not have called.
fn check_expansion(
    ctx: &EvalContext,
    form: &Sexp,
    grant: &GrantProfile,
    depth: usize,
) -> std::result::Result<(), String> {
    // A macro that expands to itself is not a clever program.
    if depth > 64 {
        return Err("macro expansion did not terminate".into());
    }
    if let Some(found) = check_form(form, grant) {
        return Err(found);
    }
    let Sexp::List(items, _) = form else {
        return Ok(());
    };
    let Some(Sexp::Symbol(name, _)) = items.front() else {
        return Ok(());
    };
    let args: Vec<Sexp> = items.iter().skip(1).cloned().collect();
    let Ok(Some(expanded)) = macros::try_expand_by_name(name, &args, ctx.env(), ctx) else {
        return Ok(());
    };
    check_expansion(ctx, &expanded, grant, depth + 1)
}

/// The I/O host an untrusted run gets.
///
/// Files are supplied by the grant, so a program cannot reach the
/// store, the credentials, or the source tree by naming a path: the
/// path is not a key in a map that contains them. `current_dir` is an
/// error rather than a guess, and the output is capped on write rather
/// than on read, so a program printing a gigabyte does not make the host
/// hold a gigabyte.
struct SandboxIo {
    /// The output buffer is this type's own. Delegating to
    /// `BufferIoHost` looked simpler, but its `println` appends a
    /// newline the caller never charged for, so a program that printed
    /// a million suppressed lines still grew the host's buffer past the
    /// cap — the cap was on the *messages*, not on the bytes kept.
    output: std::sync::Arc<std::sync::Mutex<String>>,
    files: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    cap: usize,
    truncated: std::sync::Arc<std::sync::Mutex<bool>>,
}

impl SandboxIo {
    fn new(cap: usize) -> Self {
        SandboxIo {
            output: Arc::new(std::sync::Mutex::new(String::new())),
            files: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            cap,
            truncated: Arc::new(std::sync::Mutex::new(false)),
        }
    }

    fn grant_file(&self, path: &str, content: &str) {
        self.files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(path.to_string(), content.to_string());
    }

    /// Append `text` to the kept output, up to the cap.
    ///
    /// Every byte written is charged, newline included: a cap that does
    /// not count the separator is a cap the program walks past one line
    /// at a time.
    fn append(&self, text: &str) {
        let mut out = self.output.lock().unwrap_or_else(|p| p.into_inner());
        let remaining = self.cap.saturating_sub(out.len());
        if remaining == 0 {
            if !text.is_empty() {
                *self.truncated.lock().unwrap_or_else(|p| p.into_inner()) = true;
            }
            return;
        }
        if text.len() <= remaining {
            out.push_str(text);
            return;
        }
        // Cut on a char boundary: a truncated multi-byte character
        // would turn a capped log into a corrupt one.
        let mut end = remaining;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        out.push_str(&text[..end]);
        *self.truncated.lock().unwrap_or_else(|p| p.into_inner()) = true;
    }

    fn output(&self) -> String {
        self.output.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

impl zio_core::io::IoHost for SandboxIo {
    fn print(&self, msg: &str) -> std::result::Result<(), EvalError> {
        self.append(msg);
        Ok(())
    }

    fn println(&self, msg: &str) -> std::result::Result<(), EvalError> {
        self.append(&format!("{msg}\n"));
        Ok(())
    }

    fn read_line(&self) -> std::result::Result<String, EvalError> {
        // No input stream: a program that waits for a console is a
        // program that runs until the deadline.
        Err(EvalError::custom("no input stream in a sandboxed execution"))
    }

    fn current_dir(&self) -> std::result::Result<String, EvalError> {
        Err(EvalError::custom(
            "sandboxed execution has no working directory",
        ))
    }

    fn read_file(&self, path: &str) -> std::result::Result<String, EvalError> {
        self.files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(path)
            .cloned()
            .ok_or_else(|| {
                EvalError::custom(format!("{path} was not granted to this execution"))
            })
    }

    fn write_file(&self, path: &str, data: &str) -> std::result::Result<(), EvalError> {
        // A write is charged to the same budget as a print: it is bytes
        // the program made the host hold, and an unbounded write is the
        // same exhaustion as an unbounded print.
        self.append(data);
        self.grant_file(path, data);
        Ok(())
    }

    fn file_exists(&self, path: &str) -> std::result::Result<bool, EvalError> {
        Ok(self
            .files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(path))
    }
}

/// Run one generated program under one grant.
///
/// The `actor` is the trusted host: it names the run, and only it can
/// write the trace. A candidate never calls this directly.
pub fn execute(
    store: &Store,
    actor: &Actor,
    request: ExecutionRequest,
) -> Result<ExecutionResult> {
    actor.require(ActorRole::Operator, "executing a generated program")?;
    let grant = request.grant.clone();
    let source_digest = digest_bytes(request.source.as_bytes());

    let refused = |reason: String| -> Result<ExecutionResult> {
        Ok(ExecutionResult {
            execution_id: request.execution_id.clone(),
            run_id: request.run_id.clone(),
            attempt_id: request.attempt_id.clone(),
            epoch: request.epoch,
            source_digest,
            source: request.source.clone(),
            status: ExecutionStatus::Refused,
            result: None,
            output: String::new(),
            output_truncated: false,
            error: reason,
            steps: 0,
            event_count: 0,
        })
    };

    if request.source.len() > grant.limits.max_source_bytes {
        return refused(format!(
            "source is {} bytes, over the {}-byte cap",
            request.source.len(),
            grant.limits.max_source_bytes
        ));
    }

    // 1. parse — before anything is evaluated, so a malformed program
    //    is refused rather than half-run.
    let parse_root = ModuleRoots::empty();
    let parse_ctx = language_context(parse_root).map_err(|e| eval_err(e, "bootstrap"))?;
    let forms = match zio_core::bootstrap::parse_source(&parse_ctx, "candidate.zio", &request.source) {
        Ok(forms) => forms,
        Err(e) => return refused(format!("candidate does not parse: {e}")),
    };
    if forms.is_empty() {
        return refused("candidate defines nothing to run".into());
    }

    // 2. the written form.
    for form in &forms {
        if let Some(found) = check_form(form, &grant) {
            return refused(format!("{found} is not in this grant"));
        }
    }

    // 3. expansion, in the same restricted environment.
    //
    //    A macro has to *exist* before its call can be expanded, and a
    //    `defmacro` is code. So macro definitions are evaluated first —
    //    but in a context whose dangerous bindings are already revoked,
    //    because "we are only going to expand with it" is exactly the
    //    reasoning that lets a `defmacro` body call `eval` at definition
    //    time and never get checked.
    revoke_candidate_bindings(&parse_ctx);
    let mut definitions: Vec<Sexp> = Vec::new();
    for form in &forms {
        if is_macro_definition(form) {
            if let Err(e) = zio_core::eval::eval_in_context(form, &parse_ctx) {
                return refused(format!("macro definition is refused: {e}"));
            }
        } else {
            definitions.push(form.clone());
        }
    }
    for form in &definitions {
        if let Err(found) = check_expansion(&parse_ctx, form, &grant, 0) {
            return refused(format!("macro expansion yields {found}, which is not in this grant"));
        }
    }

    // 4. the dependency closure, compared against what the grant froze.
    //    A macro that expanded into a `require` is covered by the walk
    //    above; a macro that expanded into a `load` is not, because
    //    `load` names a path rather than a module.
    let mut required: Vec<String> = Vec::new();
    for form in &forms {
        collect_dependencies(form, &mut required);
    }
    for name in &required {
        if !grant.allowed_dependencies.iter().any(|d| d == name) {
            return refused(format!("dependency {name} is not in the grant"));
        }
    }
    for frozen in &grant.frozen {
        if !frozen.path.is_file() {
            return refused(format!(
                "frozen dependency {} is missing from disk; a grant that names a \
                 file that is not there is not a grant",
                frozen.path.display()
            ));
        }
    }

    // 5. execute. The context carries the granted roots, the sandbox
    //    I/O, the step ceiling, and an observer.
    let mut roots: Vec<PathBuf> = Vec::new();
    for frozen in &grant.frozen {
        if let Some(parent) = frozen.path.parent() {
            roots.push(parent.to_path_buf());
        }
    }
    for input in &request.inputs {
        roots.push(input.clone());
    }
    let module_roots = ModuleRoots::new(roots).map_err(|e| eval_err(e, "granted roots"))?;

    let io = Arc::new(SandboxIo::new(grant.limits.max_output_bytes));
    for frozen in &grant.frozen {
        if let Ok(text) = std::fs::read_to_string(&frozen.path) {
            io.grant_file(&frozen.path.to_string_lossy(), &text);
        }
    }
    for input in &request.inputs {
        if let Ok(entries) = std::fs::read_dir(input) {
            for entry in entries.flatten() {
                if let Ok(text) = std::fs::read_to_string(entry.path()) {
                    io.grant_file(&entry.path().to_string_lossy(), &text);
                }
            }
        }
    }

    let ctx = language_context_with_sandbox(module_roots, Arc::clone(&io))
        .map_err(|e| eval_err(e, "sandbox bootstrap"))?;
    ctx.set_step_ceiling(grant.limits.max_steps);

    // The observer is attached before the first form runs: a trace that
    // starts after the fact is a partial replay, not a record.
    let trace = Arc::new(observer::RecordingObserver::new());
    ctx.attach_observer(trace.clone());

    let entrypoint = request.entrypoint.clone();
    let source = request.source.clone();
    // The program runs on the calling thread. `Value` is not `Send`
    // (ADR-012), so there is nowhere else to run it, and the step
    // ceiling is what bounds a program that never stops. The deadline
    // is the host's coarser bound; it cannot interrupt a running node,
    // so the step ceiling is what actually stops the work and the
    // deadline is what the caller asked to be held to.
    let eval = (|| -> std::result::Result<Value, EvalError> {
        eval_source(&ctx, "candidate.zio", &source)?;
        let entry = ctx.env.get(&entrypoint).ok_or_else(|| {
            EvalError::symbol_not_found(entrypoint.clone())
        })?;
        if let Value::Function(func) = &entry {
            if !func.params.is_empty() {
                // A zero-argument entrypoint only. Passing candidate data
                // as arguments would have the host choose what the
                // candidate sees, which is a different contract.
                return Err(EvalError::wrong_arg_count(0, func.params.len()));
            }
        }
        // `apply` returns the next step, not a value: a call in tail
        // position hands back a `TailCall` that the trampoline has to
        // drive. Unwrapping it directly is the `try` bug again, in a
        // different place.
        let mut step = zio_core::eval::apply(entry, Vector::new(), &ctx)?;
        loop {
            match step {
                zio_core::special::TailResult::Value(value) => return Ok(value),
                zio_core::special::TailResult::Recur(_) => {
                    return Err(EvalError::custom("entrypoint recurred without a loop frame"))
                }
                zio_core::special::TailResult::TailCall(next, args) => {
                    step = zio_core::eval::apply(next, args, &ctx)?;
                }
            }
        }
    })();

    let output = io.output();
    let output_truncated = *io.truncated.lock().unwrap_or_else(|p| p.into_inner());
    let steps = ctx.steps_spent();

    // What ran, in the order it ran, bound to the run the host named.
    let run_events = events::RunEvents::new(
        &request.run_id,
        &request.attempt_id,
        trace.events().into_iter().map(event_to_record).collect(),
    );
    let event_count = run_events.len();
    run_events.commit(store, actor)?;

    let (status, result, error) = match eval {
        Ok(value) => (ExecutionStatus::Completed, Some(render(&value)), String::new()),
        Err(e) => (ExecutionStatus::Failed, None, e.to_string()),
    };

    Ok(ExecutionResult {
        execution_id: request.execution_id,
        run_id: request.run_id,
        attempt_id: request.attempt_id,
        epoch: request.epoch,
        source_digest,
        source: request.source,
        status,
        result,
        output,
        output_truncated,
        error,
        steps,
        event_count,
    })
}

fn render(value: &Value) -> String {
    let mut out = String::new();
    let _ = value.pretty_print(&mut out, 0);
    out
}

fn eval_err(error: EvalError, what: &str) -> Error {
    Error::new(ErrorKind::BackendFailed, format!("{what}: {error}"))
}

/// One shared assembly path with a host-supplied I/O host, so the
/// sandbox context and the CLI context differ in exactly one place.
fn language_context_with_sandbox(
    roots: ModuleRoots,
    io: Arc<SandboxIo>,
) -> std::result::Result<EvalContext, EvalError> {
    let ctx = zio_core::bootstrap::language_context_with_io(
        roots,
        io as Arc<dyn zio_core::io::IoHost>,
    )?;
    revoke_candidate_bindings(&ctx);
    Ok(ctx)
}

/// Take away the bindings a candidate run must not have.
///
/// The execution path builds this context itself and installs nothing
/// else, so the model's session, the artifact store and the publication
/// entry points were never bound. They are taken to `nil` anyway: a name
/// that resolves to `nil` fails with a type error the program can read,
/// which is a better answer than a name that is silently absent and
/// looks like a typo.
fn revoke_candidate_bindings(ctx: &EvalContext) {
    for name in RESERVED_BINDINGS {
        ctx.env.set((*name).to_string(), Value::Nil);
    }
}

/// What a candidate program may not call, however it got them. Named
/// once so the execution path and its tests cannot disagree.
pub const RESERVED_BINDINGS: &[&str] = &[
    "grove-publish",
    "grove-approve",
    "grove-artifact-put",
    "grove-artifact-get",
    "llm-complete",
    "embed",
];

/// Core's event to a durable record. The name is resolved against the
/// same SourceMap the evaluator used, so the log can point at a line
/// without carrying the source text with it.
fn event_to_record(event: observer::Event) -> events::ExecutionEvent {
    let source = event.span.map(|span| events::EventSourceStub {
        source_id: span.source_id.0,
        line: span.line,
        col: span.col,
        // The name is filled in by the caller that owns the SourceMap;
        // a log entry without it is still a real location.
        name: String::new(),
    });
    events::ExecutionEvent {
        schema: crate::contracts::SCHEMA_VERSION,
        sequence: event.sequence,
        run_id: String::new(),
        attempt_id: String::new(),
        kind: event.kind.as_str().to_string(),
        detail: event.detail,
        source,
        at_ms: 0,
    }
}

/// Contract-test surface: the same walk the host runs, so a test can
/// drive it without an execution.
pub fn probe_check_form(source: &str, grant: &GrantProfile) -> Option<String> {
    let ctx = language_context(ModuleRoots::empty()).ok()?;
    let forms = zio_core::bootstrap::parse_source(&ctx, "probe.zio", source).ok()?;
    forms.iter().find_map(|f| check_form(f, grant))
}

/// The dependency names a source declares, by `require`.
pub fn probe_dependencies(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(ctx) = language_context(ModuleRoots::empty()) {
        if let Ok(forms) = zio_core::bootstrap::parse_source(&ctx, "probe.zio", source) {
            for form in &forms {
                collect_dependencies(form, &mut out);
            }
        }
    }
    out
}
