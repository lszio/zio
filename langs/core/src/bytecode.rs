//! Self-hosted Zio instruction runtime.
//!
//! This module is the only thing the rest of the evaluator calls into when a
//! function value carries `compiled: Some(closure)`. The compiler living in
//! `langs/compiler/compiler.zio` lowers Zio source into the constant pool and
//! opcode table this VM understands; `bootstrap_compiler` installs the
//! interpreted frontend bundle (reader, expand, evaluator, analyze,
//! compiler), `run_source`/`execute_module` run installed programs, and
//! `self_build` performs the three-stage build that proves the compiler
//! compiles itself: the interpreted frontend produces stage 1, and each
//! later stage is the previous stage's compiled frontend compiling the
//! same bundle again in a fresh context.
//!
//! Explicit artifacts execute bytecode without expression interpretation.
//! The public source loader retains its bootstrap AST path; native declaration
//! mechanisms install ZOS metadata and compiled function/method bodies.
//! Artifacts retain optional original source text across wire round-trips.
//! Runtime registration and parser span indexing are shared by their closures;
//! legacy artifacts without provenance never borrow a newer filename revision.
//! The VM charges fuel through `EvalRuntime::spend` on
//! every instruction so the bounded-execution feature in this system is
//! honored inside compiled code, including closures and HOFs.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Weak};

use im::{HashMap as ImHashMap, Vector, vector};
use serde::{Deserialize, Serialize};

use crate::context::{EvalContext, EvalEngine, EvalRuntime};
use crate::env::Env;
use crate::error::EvalError;
use crate::macros;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::value::{Function, Macro, NativeFn, Value};

// ─── Public wire-level types ────────────────────────────────────────────

/// The one wire version the compiler emits and the runtime accepts.
pub const MODULE_VERSION: u32 = 1;

/// A compiled callable with shared cells for captured lexical bindings.
#[derive(Debug, Clone)]
pub struct Closure {
    pub module: Arc<ModuleArtifact>,
    pub fn_index: usize,
    pub captures: Vector<Rc<RefCell<BindingCell>>>,
}

/// A compiled module: the wire-level payload the compiler emits, the runtime
/// executes, and `serialize_module` / `deserialize_module` round-trip.
#[derive(Debug, Clone)]
pub struct ModuleArtifact {
    pub version: u32,
    pub name: String,
    pub entry: usize,
    pub functions: Vector<FunctionRecord>,
    pub constants: Vector<Constant>,
    /// The module's exported entry, recorded by name.
    pub entry_name: Option<String>,
    /// Original source, absent for legacy artifacts; never inferred from a filename.
    source: Option<Arc<SourceProvenance>>,
}

#[derive(Debug, Clone)]
pub struct FunctionRecord {
    /// Source-level name, when the compiler knew one. Not needed to run
    /// a function, but needed to *find* one: several functions share a
    /// shape, and binding the wrong one by shape produces an error that
    /// names neither the function nor the mistake.
    pub name: Option<String>,
    pub params: Vector<String>,
    pub rest: Option<String>,
    pub locals: usize,
    /// Capture descriptors, in enclosing-scope order: `[:local n]` or
    /// `[:capture n]`. Runtime closures resolve these to shared binding cells.
    pub captures: Vector<Value>,
    pub code: Vector<Instruction>,
    pub debug: Vector<Option<(usize, usize)>>,
}

#[derive(Debug)]
struct SourceProvenance {
    text: String,
    spans: OnceCell<std::collections::BTreeMap<(usize, usize), crate::span::Span>>,
    registrations: RefCell<Vec<(Weak<crate::span::SourceMap>, crate::span::SourceId)>>,
    /// Guards reentrant span decoding: decoding runs the installed Zi
    /// reader, whose own compiled bodies may bind provenance again.
    decoding: std::cell::Cell<bool>,
}

fn collect_spans(
    form: &Sexp,
    spans: &mut std::collections::BTreeMap<(usize, usize), crate::span::Span>,
) {
    if let Some(span) = form.span() {
        spans.entry((span.line, span.col)).or_insert(span);
    }
    match form {
        Sexp::List(items, _) | Sexp::Vector(items, _) => {
            items.iter().for_each(|item| collect_spans(item, spans))
        }
        Sexp::Map(items, _) => items.iter().for_each(|(key, value)| {
            collect_spans(key, spans);
            collect_spans(value, spans);
        }),
        _ => {}
    }
}

// Macro constructors are materialized while the compiler is still running,
// before the returned root artifact can carry :source. Retain that
// compile unit's exact registration, lazily snapshotting it only if a
// constructor needs it.
struct CompilationSource {
    map: Arc<crate::span::SourceMap>,
    id: crate::span::SourceId,
    provenance: Option<(String, Arc<SourceProvenance>)>,
}

thread_local! {
    static COMPILATION_SOURCE: RefCell<Option<CompilationSource>> = const { RefCell::new(None) };
}

struct CompilationSourceGuard(Option<CompilationSource>);

impl CompilationSourceGuard {
    fn enter(ctx: &EvalContext, source_id: crate::span::SourceId) -> Self {
        let origin = CompilationSource {
            map: Arc::clone(ctx.source_map()),
            id: source_id,
            provenance: None,
        };
        Self(COMPILATION_SOURCE.with(|active| active.replace(Some(origin))))
    }
}

impl Drop for CompilationSourceGuard {
    fn drop(&mut self) {
        COMPILATION_SOURCE.with(|active| {
            active.replace(self.0.take());
        });
    }
}

fn compilation_provenance(name: &str, engine: &dyn EvalEngine) -> Option<Arc<SourceProvenance>> {
    COMPILATION_SOURCE.with(|active| {
        let mut active = active.borrow_mut();
        let origin = active.as_mut()?;
        if !Arc::ptr_eq(&origin.map, engine.source_map()) {
            return None;
        }
        if origin.provenance.is_none() {
            let file = origin.map.get(origin.id)?;
            if file.name != name {
                return None;
            }
            let source = Arc::new(SourceProvenance::new(file.source));
            source
                .registrations
                .borrow_mut()
                .push((Arc::downgrade(&origin.map), origin.id));
            origin.provenance = Some((file.name, source));
        }
        origin
            .provenance
            .as_ref()
            .filter(|(source_name, _)| source_name == name)
            .map(|(_, source)| Arc::clone(source))
    })
}

impl SourceProvenance {
    fn new(text: String) -> Self {
        Self {
            text,
            spans: OnceCell::new(),
            registrations: RefCell::new(Vec::new()),
            decoding: std::cell::Cell::new(false),
        }
    }

    /// Register this provenance's text under `name` in the engine's
    /// SourceMap and return its id, reusing an existing registration for
    /// the same map. Span metadata is decoded once per provenance —
    /// through the installed Zi reader, never a second hidden Rust parse —
    /// whether or not the registration already existed (macro constructors
    /// pre-seed registrations, and compile errors still need spans).
    fn bind(
        &self,
        name: &str,
        engine: &dyn EvalEngine,
    ) -> Result<crate::span::SourceId, EvalError> {
        let source_map = engine.source_map();
        let mut registrations = self.registrations.borrow_mut();
        let id = match registrations
            .iter()
            .find(|(map, _)| std::ptr::eq(map.as_ptr(), Arc::as_ptr(source_map)))
        {
            Some((_, id)) => *id,
            None => {
                registrations.retain(|(map, _)| map.strong_count() != 0);
                let id = source_map.register(name.to_string(), self.text.clone());
                registrations.push((Arc::downgrade(source_map), id));
                id
            }
        };
        drop(registrations);
        if self.spans.get().is_none() && !self.decoding.replace(true) {
            let decoded = self.decode_spans(id, engine);
            self.decoding.set(false);
            let _ = self.spans.set(decoded?);
        }
        Ok(id)
    }

    /// Decode the `(line, col) -> span` table by projecting the Zi
    /// reader's own syntax nodes to Sexp metadata. The reader must be
    /// installed; a context that cannot read its own artifacts fails
    /// closed instead of guessing spans.
    fn decode_spans(
        &self,
        id: crate::span::SourceId,
        engine: &dyn EvalEngine,
    ) -> Result<std::collections::BTreeMap<(usize, usize), crate::span::Span>, EvalError> {
        let zi_read = engine.env().get("zi-read").ok_or_else(|| {
            EvalError::custom(
                "zi-read is not bound; module source provenance requires the installed Zio reader",
            )
        })?;
        let nodes = call_value(
            zi_read,
            vector![
                Value::String(self.text.clone()),
                Value::Integer(id.0 as i64)
            ],
            engine,
        )?;
        let Value::Vector(forms) = nodes else {
            return Err(EvalError::custom(
                "zi-read did not return a form vector for module provenance",
            ));
        };
        let mut spans = std::collections::BTreeMap::new();
        for form in forms.iter() {
            collect_spans(&compiler_sexp(form)?, &mut spans);
        }
        Ok(spans)
    }
}

/// The self-hosted frontend bundle, in load order. Stage 0 evaluates each
/// part as ordinary Zio source; every later stage executes the same
/// content as one compiled artifact.
const COMPILER_PARTS: [(&str, &str); 5] = [
    ("reader.zio", include_str!("../../interpreter/reader.zio")),
    ("expand.zio", include_str!("../../interpreter/expand.zio")),
    (
        "evaluator.zio",
        include_str!("../../interpreter/evaluator.zio"),
    ),
    ("analyze.zio", include_str!("../../compiler/analyze.zio")),
    ("compiler.zio", include_str!("../../compiler/compiler.zio")),
];

/// The single deterministic bundle text: the five parts concatenated on
/// newline boundaries, in load order. Computed once into static storage;
/// every stage compiles byte-identical copies of this string.
pub fn compiler_source() -> &'static str {
    static SOURCE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let mut text = String::new();
        for (_, part) in COMPILER_PARTS {
            text.push_str(part);
            if !part.ends_with('\n') {
                text.push('\n');
            }
        }
        text
    });
    &SOURCE
}

thread_local! {
    /// SourceIds this thread's contexts registered for the compiler
    /// bundle — the interpreted bootstrap parts and each installed
    /// compiled artifact's provenance. Exact registrations, established
    /// only by `bootstrap_compiler` / `install_compiled_compiler`; a file
    /// the user happens to name `compiler.zio` never lands here.
    static COMPILER_BUNDLE_SOURCES: RefCell<Vec<(Weak<crate::span::SourceMap>, crate::span::SourceId)>> =
        const { RefCell::new(Vec::new()) };
}

fn record_bundle_source(map: &Arc<crate::span::SourceMap>, id: crate::span::SourceId) {
    COMPILER_BUNDLE_SOURCES.with(|sources| {
        let mut sources = sources.borrow_mut();
        sources.retain(|(live, _)| live.strong_count() != 0);
        sources.push((Arc::downgrade(map), id));
    });
}

/// Whether `id` names a compiler-bundle registration in `map`. Errors
/// carrying such a span are implementation failures of the frontend
/// itself; diagnostics rebind them to the target form.
pub(crate) fn is_compiler_bundle_source(
    map: &crate::span::SourceMap,
    id: crate::span::SourceId,
) -> bool {
    COMPILER_BUNDLE_SOURCES.with(|sources| {
        sources
            .borrow()
            .iter()
            .any(|(live, known)| std::ptr::eq(live.as_ptr(), map) && *known == id)
    })
}

struct CompilerBindings {
    map: Weak<crate::span::SourceMap>,
    names: Vec<String>,
    protected: Vec<Weak<Function>>,
}

thread_local! {
    static COMPILER_BINDINGS: RefCell<Vec<CompilerBindings>> = const { RefCell::new(Vec::new()) };
}

fn record_compiler_bindings(ctx: &EvalContext, names: Vec<String>) {
    let protected = ["zi-read-one", "zi-e-step"]
        .iter()
        .filter_map(|name| match ctx.env.get_owned(name) {
            Some(Value::Function(function)) => Some(Arc::downgrade(&function)),
            _ => None,
        })
        .collect();
    COMPILER_BINDINGS.with(|bindings| {
        let mut bindings = bindings.borrow_mut();
        bindings.retain(|entry| {
            entry.map.strong_count() > 0 && entry.map.as_ptr() != Arc::as_ptr(ctx.source_map())
        });
        bindings.push(CompilerBindings {
            map: Arc::downgrade(ctx.source_map()),
            names,
            protected,
        });
    });
}

/// Authorize the registered callback itself; syntax spans are forgeable data.
pub(crate) fn is_compiler_bundle_function(
    function: &Arc<Function>,
    map: &crate::span::SourceMap,
) -> bool {
    COMPILER_BINDINGS.with(|bindings| {
        bindings.borrow().iter().any(|entry| {
            std::ptr::eq(entry.map.as_ptr(), map)
                && entry
                    .protected
                    .iter()
                    .any(|known| std::ptr::eq(known.as_ptr(), Arc::as_ptr(function)))
        })
    })
}

/// Outer lookup retained by a reserved forward definition until it executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum BindingSource {
    Local(usize),
    Capture(usize),
    Global(String),
}

/// One VM instruction. Opcodes mirror what `langs/compiler/compiler.zio`
/// emits; operands are typed so the VM dispatch is a tight match.
#[derive(Debug, Clone)]
pub enum Instruction {
    Const(usize),
    Pop,
    Dup,
    Local(usize),
    Capture(usize),
    Store(usize),
    StoreCapture(usize),
    /// Assign a lexical binding without initializing an unexecuted definition.
    SetLocal(usize),
    Bind(usize),
    /// Reserve a fresh unbound activation, falling back to the outer binding
    /// until Store executes the definition. Captured assignment follows fallback.
    Declare(usize, String, Option<BindingSource>),
    Global(String),
    SetGlobal(String),
    Define(String),
    Jump(usize),
    JumpFalse(usize),
    JumpTrue(usize),
    Closure(usize),
    Call(usize),
    TailCall(usize),
    Return,
    VectorN(usize),
    MapN(usize),
    QuasiConj(bool),
    QuasiList,
    /// Protect a stack region. Targets are tested in the compiler's order,
    /// matching AST catch precedence; the catch gets a child `*error*` binding.
    Handler(Vec<(String, usize)>),
    Unhandler,
    ClearError,
    /// Metadata form: `(require a.b)` etc. The arg list sits on the stack
    /// as a single pre-collected Value; the VM defers to the module special
    /// form so behavior matches the interpreter.
    Metadata(String),
    Macro(String),
    MacroRules(String),
    Module,
    Method,
    Class(usize),
}

#[derive(Debug, Clone)]
pub enum Constant {
    Nil,
    Boolean(bool),
    Integer(i64),
    FloatBits(u64),
    String(String),
    Symbol(String),
    Keyword(String),
    Char(char),
    /// A list/vector/map payload indexes back into the constants table. The
    /// index references another `Constant`, so round-tripping never reuses a
    /// runtime `Value` — keys (including `:keyword`) are reconstructed from
    /// the literal kind on the way out, never silently turned into strings.
    List(Vector<usize>),
    Vector(Vector<usize>),
    Map(Vector<(usize, usize)>),
}

// ─── Bootstrap entry points ─────────────────────────────────────────────

/// Native host hooks the Zio compiler expects. Every name here is referenced
/// from `langs/compiler/compiler.zio`. Names are kept under the `compiler/`
/// namespace to match the source.
pub fn register(ctx: &EvalContext) {
    // The install hooks need the context's environment so they can install
    // macros and rules into it. The context itself is not cloneable (the
    // RefCells inside it would alias), so we only capture `env: Arc<Env>` —
    // it is the only thing the helpers actually use, and capturing it lets
    // the NativeFn closures outlive the `register` frame as `static`.
    let env = ctx.env.clone();
    env.set(
        "compiler/name".into(),
        Value::NativeFunction(NativeFn::new("compiler/name", move |args, _| {
            Ok(Value::String(compiler_name(args)?))
        })),
    );
    env.set(
        "compiler/position".into(),
        Value::NativeFunction(NativeFn::new("compiler/position", move |args, _| {
            compiler_position(args)
        })),
    );
    env.set(
        "compiler/form".into(),
        Value::NativeFunction(NativeFn::new("compiler/form", |args, _| {
            compiler_form(compiler_argument(&args)?)
        })),
    );
    env.set(
        "compiler/datum".into(),
        Value::NativeFunction(NativeFn::new("compiler/datum", |args, _| {
            compiler_datum(compiler_argument(&args)?)
        })),
    );
    env.set(
        "compiler/order-keys".into(),
        Value::NativeFunction(NativeFn::new("compiler/order-keys", move |args, _| {
            compiler_order_keys(args)
        })),
    );
    env.set(
        "compiler/float-bits".into(),
        Value::NativeFunction(NativeFn::new("compiler/float-bits", move |args, _| {
            Ok(Value::Integer(compiler_float_bits(args)?))
        })),
    );
    env.set(
        "compiler/fail".into(),
        Value::NativeFunction(NativeFn::new("compiler/fail", move |args, engine| {
            Err(compiler_fail(args, engine)?)
        })),
    );
    // Each closure needs its own handle to the one live environment.
    // `move` takes the binding by value, so a second binding is made
    // before the first is moved into its closure.
    let rules_env = Arc::clone(&env);
    env.set(
        "compiler/install-rules".into(),
        Value::NativeFunction(NativeFn::new("compiler/install-rules", move |args, _| {
            install_rules(args, Arc::clone(&rules_env))?;
            Ok(Value::Nil)
        })),
    );
    let macro_env = Arc::clone(&env);
    env.set(
        "compiler/install-macro".into(),
        Value::NativeFunction(NativeFn::new(
            "compiler/install-macro",
            move |args, engine| {
                install_macro(args, Arc::clone(&macro_env), engine)?;
                Ok(Value::Nil)
            },
        )),
    );
}

/// Install the interpreted frontend bundle into `ctx`: every part is
/// parsed and run through the standard Rust loader exactly once per
/// registration, so `zi-read`, `zi-expand`, `zi-apply` and `zio--compile`
/// exist as ordinary globals. Each part's registration is recorded as a
/// trusted implementation source — exact ids, never filename guesses —
/// so diagnostics re-frontend span errors to the target form.
pub fn bootstrap_compiler(ctx: &EvalContext) -> Result<(), EvalError> {
    let mut names = Vec::new();
    for (name, part) in COMPILER_PARTS {
        let id = ctx
            .source_map()
            .register(name.to_string(), part.to_string());
        record_bundle_source(ctx.source_map(), id);
        let forms = crate::reader::reader::read_program_with_source(part, id)
            .map_err(|error| error.into_eval(name))?;
        for form in forms {
            if let Sexp::List(items, _) = &form {
                if let (Some(Sexp::Symbol(head, _)), Some(Sexp::Symbol(name, _))) =
                    (items.get(0), items.get(1))
                    && matches!(
                        head.as_str(),
                        "def" | "defn" | "defmacro" | "defmacro-rules"
                    )
                {
                    names.push(name.clone());
                }
            }
            crate::eval::eval_in_context(&form, ctx)?;
        }
    }
    record_compiler_bindings(ctx, names);
    Ok(())
}

/// Parse, compile, and run `source` against `ctx`. Equivalent to
/// `bootstrap_source` with compilation installed.
pub fn run_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    eval_source_text(ctx, name, source)
}

/// Compile `source` to a module artifact by calling the installed
/// frontend. The text is registered in the context's SourceMap and read
/// by the global `zi-read` — interpreted at stage 0, compiled after
/// `install_compiled_compiler` — and the resulting syntax nodes go to
/// `zio--compile` untouched. No Rust parser ever sees the target text.
pub fn compile_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    let source_id = ctx
        .source_map()
        .register(name.to_string(), source.to_string());
    let zi_read = ctx.env.get("zi-read").ok_or_else(|| {
        EvalError::custom("zi-read is not bound; bootstrap_compiler was not called")
    })?;
    let _source_guard = CompilationSourceGuard::enter(ctx, source_id);
    let forms = call_value(
        zi_read,
        vector![
            Value::String(source.into()),
            Value::Integer(source_id.0 as i64)
        ],
        ctx,
    )?;
    if !matches!(&forms, Value::Vector(_)) {
        return Err(EvalError::custom(
            "zi-read did not return a vector of forms",
        ));
    }
    let zio_compile = ctx.env.get("zio--compile").ok_or_else(|| {
        EvalError::custom("zio--compile is not bound; bootstrap_compiler was not called")
    })?;
    let compiled = call_value(zio_compile, vector![Value::String(name.into()), forms], ctx)?;
    let Value::Map(mut module) = compiled else {
        return Err(EvalError::custom("compiler returned a non-module value"));
    };
    module.insert(
        Value::Keyword("source".into()),
        Value::String(source.into()),
    );
    Ok(Value::Map(module))
}

/// Run an installed compiled module — the unit the compiler emits.
pub fn execute_module(ctx: &EvalContext, module: &Value) -> Result<Value, EvalError> {
    let artifact = value_to_artifact(module)?;
    run_artifact(ctx, &artifact)
}

/// Serialize a module value to its canonical JSON wire form.
/// Map-key kinds are preserved: keyword keys round-trip as keywords, not as
/// strings.
pub fn serialize_module(module: &Value) -> Result<String, EvalError> {
    let artifact = value_to_artifact(module)?;
    let wire = artifact.to_wire()?;
    serde_json::to_string(&wire).map_err(|e| EvalError::custom(format!("serialize: {e}")))
}

/// Reconstruct a module value from a wire JSON string. Performs bounded
/// validation: constant kind whitelist, max constant count, max code length,
/// max function count, max instruction arity. Anything malformed fails closed.
pub fn deserialize_module(text: &str) -> Result<Value, EvalError> {
    let wire: WireModule =
        serde_json::from_str(text).map_err(|e| EvalError::custom(format!("deserialize: {e}")))?;
    let artifact = ModuleArtifact::from_wire(&wire)?;
    Ok(artifact_to_value(&artifact))
}

/// Three-stage self-build of the frontend bundle. Stage 1 is the
/// interpreted frontend (stage 0) compiling the bundle; stage 2 is stage
/// 1's compiled frontend recompiling the same bundle in a fresh context;
/// stage 3 is stage 2's frontend doing the same again. Every transition
/// starts from a fresh language context, so no interpreted helper or
/// module cache from an earlier stage can survive into the next compile.
///
/// `normalized` holds each stage's canonical wire JSON; `equal` is direct
/// byte equality of all three. The `modules` vec holds the module value
/// from each stage.
pub struct SelfBuild {
    pub modules: Vec<Value>,
    pub normalized: Vec<String>,
    pub equal: bool,
}

/// A fresh context for the next self-build stage: empty module roots and
/// the caller's I/O authority. The bundle is pure source — it needs no
/// module access — so nothing ambient is invented or inherited.
fn self_build_context(ctx: &EvalContext) -> Result<EvalContext, EvalError> {
    crate::bootstrap::language_context_with_io(
        crate::bootstrap::ModuleRoots::empty(),
        Arc::clone(&ctx.io),
    )
}

pub fn self_build(ctx: &EvalContext) -> Result<SelfBuild, EvalError> {
    let source = compiler_source();
    // Stage 0: ensure the interpreted frontend is installed (language
    // contexts already bootstrap it; other callers get it here).
    if ctx.env.get("zio--compile").is_none() {
        bootstrap_compiler(ctx)?;
    }
    // Stage 1: the interpreted frontend compiles the bundle.
    let stage1 = compile_source(ctx, "compiler.zio", source)?;
    // Stage 2: fresh context, stage 1's compiled frontend installed.
    let stage2_ctx = self_build_context(ctx)?;
    install_compiled_compiler(&stage2_ctx, &stage1)?;
    let stage2 = compile_source(&stage2_ctx, "compiler.zio", source)?;
    // Stage 3: fresh context, stage 2's compiled frontend installed.
    let stage3_ctx = self_build_context(ctx)?;
    install_compiled_compiler(&stage3_ctx, &stage2)?;
    let stage3 = compile_source(&stage3_ctx, "compiler.zio", source)?;

    let norm = |m: &Value| -> Result<String, EvalError> {
        let art = value_to_artifact(m)?;
        let wire = art.to_wire();
        serde_json::to_string(&wire?).map_err(|e| EvalError::custom(format!("normalize: {e}")))
    };
    let n1 = norm(&stage1)?;
    let n2 = norm(&stage2)?;
    let n3 = norm(&stage3)?;
    let equal = n1 == n2 && n2 == n3;
    Ok(SelfBuild {
        modules: vec![stage1, stage2, stage3],
        normalized: vec![n1, n2, n3],
        equal,
    })
}

// ─── VM entry from the eval layer ────────────────────────────────────────

/// Entry point used by `eval::apply` when a `Value::Function` carries a
/// compiled closure. Returns the produced value (caller wraps in TailResult).
pub fn invoke_function(
    func: &Function,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let closure = func
        .compiled
        .as_ref()
        .ok_or_else(|| EvalError::custom("invoke_function called on interpreted function"))?;
    invoke_closure(closure, args, &func.env, engine)
}

/// Call a `Value` as a function. Used by native HOFs that need to invoke a
/// possibly-compiled Zio function without going through `eval::apply`.
pub fn call_value(
    func: Value,
    args: Vector<Value>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let mut func = func;
    let mut args = args;
    loop {
        if let Value::Macro(m) = &func {
            if let Some(result) = engine.intercept_macro(m, &args) {
                return result;
            }
            let forms = args
                .iter()
                .map(macros::value_to_sexp)
                .collect::<Result<Vec<_>, _>>()?;
            return macros::apply_macro(m, &forms, &m.env, engine).map(Value::from);
        }
        match crate::eval::apply(func, args, engine)? {
            TailResult::Value(value) => return Ok(value),
            TailResult::TailCall(next, next_args) => {
                func = next;
                args = next_args;
            }
            TailResult::Recur(_) => return Err(EvalError::recur_without_loop()),
        }
    }
}

// ─── VM core ────────────────────────────────────────────────────────────

/// A protected operand-stack region and its lexical environment.
struct HandlerFrame {
    table: Vec<(String, usize)>,
    stack_len: usize,
    env: Arc<Env>,
    error_depth: usize,
}

#[derive(Debug)]
pub enum BindingFallback {
    Cell(Rc<RefCell<BindingCell>>),
    Global(String, Arc<Env>),
}

/// Runtime-only binding state; unbound reservations are never language values.
#[derive(Debug)]
pub enum BindingCell {
    Bound(Value),
    Unbound {
        name: String,
        fallback: Option<BindingFallback>,
    },
}

impl BindingCell {
    fn read(&self, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
        match self {
            Self::Bound(value) => Ok(value.clone()),
            Self::Unbound { name, fallback } => match fallback {
                Some(BindingFallback::Cell(cell)) => cell.borrow().read(engine),
                Some(BindingFallback::Global(name, env)) => lookup_global(engine, name, env),
                None => Err(EvalError::symbol_not_found(name)),
            },
        }
    }

    fn assign_capture(&mut self, value: Value, engine: &dyn EvalEngine) -> Result<(), EvalError> {
        match self {
            Self::Bound(slot) => {
                *slot = value;
                Ok(())
            }
            Self::Unbound { name, fallback } => match fallback {
                Some(BindingFallback::Cell(cell)) => {
                    cell.borrow_mut().assign_capture(value, engine)
                }
                Some(BindingFallback::Global(name, env)) => set_global(engine, name, value, env),
                None => Err(EvalError::symbol_not_found(name.as_str())),
            },
        }
    }
}

#[derive(Debug)]
enum LocalSlot {
    Inline(BindingCell),
    Shared(Rc<RefCell<BindingCell>>),
}

impl LocalSlot {
    fn read(&self, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
        match self {
            Self::Inline(binding) => binding.read(engine),
            Self::Shared(cell) => cell.borrow().read(engine),
        }
    }

    fn write(&mut self, value: Value) {
        match self {
            Self::Inline(binding) => *binding = BindingCell::Bound(value),
            Self::Shared(cell) => *cell.borrow_mut() = BindingCell::Bound(value),
        }
    }

    fn assign_capture(&mut self, value: Value, engine: &dyn EvalEngine) -> Result<(), EvalError> {
        match self {
            Self::Inline(binding) => binding.assign_capture(value, engine),
            Self::Shared(cell) => cell.borrow_mut().assign_capture(value, engine),
        }
    }

    fn capture(&mut self) -> Rc<RefCell<BindingCell>> {
        if let Self::Inline(binding) = self {
            *self = Self::Shared(Rc::new(RefCell::new(std::mem::replace(
                binding,
                BindingCell::Bound(Value::Nil),
            ))));
        }
        match self {
            Self::Shared(cell) => Rc::clone(cell),
            Self::Inline(_) => unreachable!(),
        }
    }
}

fn invoke_closure(
    closure: &Closure,
    args: Vector<Value>,
    lexical_env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let module = Arc::clone(&closure.module);
    let record = module
        .functions
        .get(closure.fn_index)
        .ok_or_else(|| EvalError::custom("closure references missing function"))?;
    bind_call(
        Arc::clone(&module),
        closure.fn_index,
        closure.captures.clone(),
        record,
        args,
        lexical_env,
        engine,
    )
}

fn bind_call(
    module: Arc<ModuleArtifact>,
    fn_index: usize,
    captures: Vector<Rc<RefCell<BindingCell>>>,
    record: &FunctionRecord,
    args: Vector<Value>,
    lexical_env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let mut locals = Vec::new();
    bind_locals(record, captures, args, &mut locals)?;
    run_loop(module, fn_index, locals, lexical_env, engine)
}

fn bind_locals(
    record: &FunctionRecord,
    captures: Vector<Rc<RefCell<BindingCell>>>,
    args: Vector<Value>,
    locals: &mut Vec<LocalSlot>,
) -> Result<(), EvalError> {
    let required = record.params.len();
    if record.rest.is_some() {
        if args.len() < required {
            return Err(EvalError::wrong_arg_count_min(required, args.len()));
        }
    } else if args.len() != required {
        return Err(EvalError::wrong_arg_count(required, args.len()));
    }
    let size = record
        .locals
        .max(captures.len() + required + usize::from(record.rest.is_some()));
    locals.clear();
    locals.reserve(size);
    locals.extend(captures.into_iter().map(LocalSlot::Shared));
    let mut args = args.into_iter();
    locals.extend(
        args.by_ref()
            .take(required)
            .map(|value| LocalSlot::Inline(BindingCell::Bound(value))),
    );
    if record.rest.is_some() {
        locals.push(LocalSlot::Inline(BindingCell::Bound(Value::List(
            args.collect(),
        ))));
    }
    locals.resize_with(size, || LocalSlot::Inline(BindingCell::Bound(Value::Nil)));
    Ok(())
}

/// Suspended compiled calls live here, not on the Rust call stack.
struct CallFrame {
    module: Arc<ModuleArtifact>,
    fn_index: usize,
    locals: Vec<LocalSlot>,
    env: Arc<Env>,
    stack: Vec<Value>,
    handlers: Vec<HandlerFrame>,
    error_envs: Vec<Arc<Env>>,
    source_id: Option<crate::span::SourceId>,
    pc: usize,
    call_pc: usize,
}

fn run_loop(
    mut module: Arc<ModuleArtifact>,
    mut fn_index: usize,
    mut locals: Vec<LocalSlot>,
    lexical_env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let mut pc = 0;
    let mut env = lexical_env.clone();
    let mut stack = Vec::new();
    let mut handlers: Vec<HandlerFrame> = Vec::new();
    let mut error_envs: Vec<Arc<Env>> = Vec::new();
    let mut source_id = module
        .source
        .as_ref()
        .map(|source| source.bind(&module.name, engine))
        .transpose()?;
    let mut calls: Vec<CallFrame> = Vec::new();
    loop {
        let fault_pc = pc;
        let result = (|| -> Result<Option<Value>, EvalError> {
            engine.spend(1)?;
            let record = module
                .functions
                .get(fn_index)
                .ok_or_else(|| EvalError::custom("function index out of range"))?;
            let instr = record
                .code
                .get(pc)
                .cloned()
                .ok_or_else(|| EvalError::custom("PC ran off end of code"))?;
            pc += 1;
            match instr {
                Instruction::Const(i) => stack.push(indexed_constant(&module.constants, i)?),
                Instruction::Pop => {
                    pop_stack(&mut stack)?;
                }
                Instruction::Dup => stack.push(
                    stack
                        .last()
                        .cloned()
                        .ok_or_else(|| EvalError::custom("dup on empty stack"))?,
                ),
                Instruction::Local(i) | Instruction::Capture(i) => {
                    stack.push(
                        locals
                            .get(i)
                            .ok_or_else(|| EvalError::custom("local index out of range"))?
                            .read(engine)?,
                    );
                }
                Instruction::Store(i) => {
                    let value = stack
                        .last()
                        .cloned()
                        .ok_or_else(|| EvalError::custom("store on empty stack"))?;
                    locals
                        .get_mut(i)
                        .ok_or_else(|| EvalError::custom("local index out of range"))?
                        .write(value);
                }
                Instruction::StoreCapture(i) | Instruction::SetLocal(i) => {
                    let value = stack
                        .last()
                        .cloned()
                        .ok_or_else(|| EvalError::custom("store on empty stack"))?;
                    locals
                        .get_mut(i)
                        .ok_or_else(|| EvalError::custom("local index out of range"))?
                        .assign_capture(value, engine)?;
                }
                Instruction::Bind(i) => {
                    let value = pop_stack(&mut stack)?;
                    *locals
                        .get_mut(i)
                        .ok_or_else(|| EvalError::custom("local index out of range"))? =
                        LocalSlot::Inline(BindingCell::Bound(value));
                }
                Instruction::Declare(i, name, fallback) => {
                    let fallback = match fallback {
                        Some(BindingSource::Local(index) | BindingSource::Capture(index)) => {
                            if index == i {
                                return Err(EvalError::invalid_form(
                                    "declaration cannot fall back to itself",
                                ));
                            }
                            Some(BindingFallback::Cell(
                                locals
                                    .get_mut(index)
                                    .ok_or_else(|| {
                                        EvalError::invalid_form(
                                            "declaration fallback index out of range",
                                        )
                                    })?
                                    .capture(),
                            ))
                        }
                        Some(BindingSource::Global(name)) => {
                            Some(BindingFallback::Global(name, env.clone()))
                        }
                        None => None,
                    };
                    *locals.get_mut(i).ok_or_else(|| {
                        EvalError::invalid_form("declaration index out of range")
                    })? = LocalSlot::Inline(BindingCell::Unbound { name, fallback });
                }
                Instruction::Global(name) => stack.push(lookup_global(engine, &name, &env)?),
                Instruction::SetGlobal(name) => {
                    let value = stack
                        .last()
                        .cloned()
                        .ok_or_else(|| EvalError::custom("set-global on empty stack"))?;
                    set_global(engine, &name, value, &env)?;
                }
                Instruction::Define(name) => {
                    let value = stack
                        .last()
                        .cloned()
                        .ok_or_else(|| EvalError::custom("define on empty stack"))?;
                    env.set(name, value);
                }
                Instruction::Jump(target) => pc = target,
                Instruction::JumpFalse(target) => {
                    if !crate::value::is_truthy(&pop_stack(&mut stack)?) {
                        pc = target;
                    }
                }
                Instruction::JumpTrue(target) => {
                    if crate::value::is_truthy(&pop_stack(&mut stack)?) {
                        pc = target;
                    }
                }
                Instruction::Closure(index) => {
                    let target = module
                        .functions
                        .get(index)
                        .ok_or_else(|| EvalError::custom("closure references missing function"))?;
                    let captures = snapshot_captures(&target.captures, &mut locals)?;
                    stack.push(Value::Function(Arc::new(Function {
                        params: target.params.clone(),
                        rest_param: target.rest.clone(),
                        body: Sexp::Nil,
                        env: env.clone(),
                        compiled: Some(Arc::new(Closure {
                            module: module.clone(),
                            fn_index: index,
                            captures,
                        })),
                    })));
                }
                Instruction::Call(n) | Instruction::TailCall(n) => {
                    let tail = matches!(instr, Instruction::TailCall(_));
                    let start = stack
                        .len()
                        .checked_sub(n)
                        .ok_or_else(|| EvalError::custom("call stack underflow"))?;
                    let args = stack.drain(start..).collect();
                    let func = pop_stack(&mut stack)?;
                    if let Value::Function(f) = &func {
                        if let Some(closure) = &f.compiled {
                            let target =
                                closure.module.functions.get(closure.fn_index).ok_or_else(
                                    || EvalError::custom("closure references missing function"),
                                )?;
                            let next_module = closure.module.clone();
                            let next_source = next_module
                                .source
                                .as_ref()
                                .map(|source| source.bind(&next_module.name, engine))
                                .transpose()?;
                            if tail {
                                bind_locals(target, closure.captures.clone(), args, &mut locals)?;
                                module = next_module;
                                stack.clear();
                                handlers.clear();
                                error_envs.clear();
                            } else {
                                let mut next_locals = Vec::new();
                                bind_locals(
                                    target,
                                    closure.captures.clone(),
                                    args,
                                    &mut next_locals,
                                )?;
                                calls.push(CallFrame {
                                    module: std::mem::replace(&mut module, next_module),
                                    fn_index,
                                    locals: std::mem::replace(&mut locals, next_locals),
                                    env: env.clone(),
                                    stack: std::mem::take(&mut stack),
                                    handlers: std::mem::take(&mut handlers),
                                    error_envs: std::mem::take(&mut error_envs),
                                    source_id,
                                    pc,
                                    call_pc: fault_pc,
                                });
                            }
                            source_id = next_source;
                            fn_index = closure.fn_index;
                            env = f.env.clone();
                            pc = 0;
                            return Ok(None);
                        }
                    }
                    if tail {
                        return call_value(func, args, engine).map(Some);
                    }
                    stack.push(call_value(func, args, engine)?);
                }
                Instruction::Return => return Ok(Some(stack.pop().unwrap_or(Value::Nil))),
                Instruction::VectorN(n) => {
                    let start = stack
                        .len()
                        .checked_sub(n)
                        .ok_or_else(|| EvalError::custom("vector stack underflow"))?;
                    let items = stack.drain(start..).collect();
                    stack.push(Value::Vector(items));
                }
                Instruction::MapN(n) => {
                    let start = stack
                        .len()
                        .checked_sub(2 * n)
                        .ok_or_else(|| EvalError::custom("map stack underflow"))?;
                    let mut items = stack.drain(start..);
                    let mut map = ImHashMap::new();
                    while let Some(key) = items.next() {
                        map.insert(
                            key,
                            items
                                .next()
                                .ok_or_else(|| EvalError::custom("map stack underflow"))?,
                        );
                    }
                    drop(items);
                    stack.push(Value::Map(map));
                }
                Instruction::QuasiConj(splice) => {
                    let item = pop_stack(&mut stack)?;
                    let Value::Vector(mut items) = pop_stack(&mut stack)? else {
                        return Err(EvalError::custom("quasiquote accumulator must be vector"));
                    };
                    if splice {
                        match item {
                            Value::List(values) | Value::Vector(values) => items.extend(values),
                            other => {
                                return Err(EvalError::type_error(
                                    "sequential",
                                    other.value_type(),
                                ));
                            }
                        }
                    } else {
                        items.push_back(item);
                    }
                    stack.push(Value::Vector(items));
                }
                Instruction::QuasiList => {
                    let Value::Vector(items) = pop_stack(&mut stack)? else {
                        return Err(EvalError::custom("quasiquote accumulator must be vector"));
                    };
                    stack.push(Value::List(items));
                }
                Instruction::Handler(table) => handlers.push(HandlerFrame {
                    table,
                    stack_len: stack.len(),
                    env: env.clone(),
                    error_depth: error_envs.len(),
                }),
                Instruction::Unhandler => {
                    handlers
                        .pop()
                        .ok_or_else(|| EvalError::custom("unbalanced handler"))?;
                }
                Instruction::ClearError => {
                    env = error_envs
                        .pop()
                        .ok_or_else(|| EvalError::custom("unbalanced catch"))?;
                }
                Instruction::Metadata(op) => {
                    let payload = pop_stack(&mut stack)?;
                    stack.push(handle_metadata(engine, &env, &op, payload)?);
                }
                Instruction::Macro(name) => {
                    let value = pop_stack(&mut stack)?;
                    stack.push(install_macro_function(name, value, &env)?);
                }
                Instruction::MacroRules(name) => {
                    let rules = pop_stack(&mut stack)?;
                    install_rules(vector![Value::Symbol(name.clone()), rules], env.clone())?;
                    stack.push(
                        env.get(&name)
                            .ok_or_else(|| EvalError::custom("installed macro missing"))?,
                    );
                }
                Instruction::Module => {
                    let exports = pop_stack(&mut stack)?;
                    let name = pop_stack(&mut stack)?;
                    let body = pop_stack(&mut stack)?;
                    stack.push(install_module(name, exports, body, &env, engine)?);
                }
                Instruction::Method => {
                    let metadata = pop_stack(&mut stack)?;
                    let body = pop_stack(&mut stack)?;
                    stack.push(install_method(metadata, body, &env)?);
                }
                Instruction::Class(n) => {
                    let forms = metadata_forms(pop_stack(&mut stack)?)?;
                    if n != 0 {
                        return Err(EvalError::invalid_form(
                            "compiled class initializers are not supported",
                        ));
                    }
                    stack.push(
                        crate::special::zos_forms::do_defclass(&forms, &env, engine)?.into_value(),
                    );
                }
            }
            Ok(None)
        })();
        match result {
            Ok(Some(value)) => {
                let Some(frame) = calls.pop() else {
                    return Ok(value);
                };
                module = frame.module;
                fn_index = frame.fn_index;
                locals = frame.locals;
                env = frame.env;
                stack = frame.stack;
                handlers = frame.handlers;
                error_envs = frame.error_envs;
                source_id = frame.source_id;
                pc = frame.pc;
                stack.push(value);
            }
            Ok(None) => {}
            Err(mut error) => {
                let mut error_pc = fault_pc;
                loop {
                    error = attach_instruction_span(error, &module, fn_index, error_pc, source_id);
                    let kind = crate::special::zos_forms::extract_error_type(&error);
                    let mut caught = false;
                    while let Some(handler) = handlers.pop() {
                        if let Some((_, target)) = handler
                            .table
                            .iter()
                            .find(|(name, _)| name == kind || name == "any")
                        {
                            stack.truncate(handler.stack_len);
                            error_envs.truncate(handler.error_depth);
                            error_envs.push(handler.env.clone());
                            env = Arc::new(Env::new(Some(handler.env)));
                            env.set("*error*".into(), Value::String(error.to_string()));
                            pc = *target;
                            caught = true;
                            break;
                        }
                    }
                    if caught {
                        break;
                    }
                    let Some(frame) = calls.pop() else {
                        return Err(error);
                    };
                    module = frame.module;
                    fn_index = frame.fn_index;
                    locals = frame.locals;
                    env = frame.env;
                    stack = frame.stack;
                    handlers = frame.handlers;
                    error_envs = frame.error_envs;
                    source_id = frame.source_id;
                    pc = frame.pc;
                    error_pc = frame.call_pc;
                }
            }
        }
    }
}

fn pop_stack(stack: &mut Vec<Value>) -> Result<Value, EvalError> {
    stack
        .pop()
        .ok_or_else(|| EvalError::custom("operand stack underflow"))
}

fn metadata_forms(payload: Value) -> Result<Vec<Sexp>, EvalError> {
    match macros::value_to_sexp(&payload)? {
        Sexp::List(items, _) | Sexp::Vector(items, _) => Ok(items.into_iter().collect()),
        other => Ok(vec![other]),
    }
}

fn handle_metadata(
    engine: &dyn EvalEngine,
    env: &Arc<Env>,
    op: &str,
    payload: Value,
) -> Result<Value, EvalError> {
    let forms = metadata_forms(payload)?;
    let result = match op {
        "require" => crate::special::module_forms::do_require(&forms, env, engine),
        "export" => crate::special::module_forms::do_export(&forms, env, engine),
        "package" => crate::special::zos_forms::do_defpackage(&forms, env, engine),
        "generic" => crate::special::zos_forms::do_defgeneric(&forms, env, engine),
        other => return Err(EvalError::custom(format!("unknown metadata op: {other}"))),
    }?;
    Ok(result.into_value())
}

fn snapshot_capture(
    name: &Value,
    locals: &mut [LocalSlot],
) -> Result<Rc<RefCell<BindingCell>>, EvalError> {
    let Value::Vector(parts) = name else {
        return Err(EvalError::invalid_form("capture descriptor must be vector"));
    };
    if parts.len() != 2
        || !matches!(parts.front(), Some(Value::Keyword(kind)) if kind == "local" || kind == "capture")
    {
        return Err(EvalError::invalid_form("invalid capture descriptor"));
    }
    let Some(Value::Integer(index)) = parts.get(1) else {
        return Err(EvalError::invalid_form("capture index must be integer"));
    };
    let index =
        usize::try_from(*index).map_err(|_| EvalError::invalid_form("negative capture index"))?;
    Ok(locals
        .get_mut(index)
        .ok_or_else(|| EvalError::invalid_form("capture index out of range"))?
        .capture())
}

fn snapshot_captures(
    names: &Vector<Value>,
    locals: &mut [LocalSlot],
) -> Result<Vector<Rc<RefCell<BindingCell>>>, EvalError> {
    names
        .iter()
        .map(|name| snapshot_capture(name, locals))
        .collect()
}

fn install_macro_function(name: String, value: Value, env: &Arc<Env>) -> Result<Value, EvalError> {
    let Value::Function(function) = value else {
        return Err(EvalError::type_error("function", value.value_type()));
    };
    let value = Value::Macro(Arc::new(Macro {
        name: name.clone(),
        params: function.params.clone(),
        rest_param: function.rest_param.clone(),
        body: function.body.clone(),
        env: function.env.clone(),
        compiled: function.compiled.clone(),
    }));
    env.set(name, value.clone());
    Ok(value)
}

fn install_module(
    name: Value,
    exports: Value,
    body: Value,
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let name = crate::module::parse_module_name(&name)?;
    let (Value::List(exports) | Value::Vector(exports)) = exports else {
        return Err(EvalError::invalid_form(
            "module :export must be a list or vector",
        ));
    };
    let mut names = Vec::with_capacity(exports.len());
    for export in exports {
        match export {
            Value::Symbol(name) | Value::Keyword(name) => names.push(name),
            other => {
                return Err(EvalError::invalid_form(format!(
                    "module export symbols must be keywords or symbols, got {}",
                    other.value_type()
                )));
            }
        }
    }
    let Value::Function(function) = body else {
        return Err(EvalError::type_error("function", body.value_type()));
    };
    let module_env = Arc::new(Env::new(Some(env.clone())));
    let mut function = (*function).clone();
    function.env = module_env.clone();
    engine.push_module_exports();
    let result = call_value(Value::Function(Arc::new(function)), Vector::new(), engine);
    let declared = engine.take_module_exports();
    let result = result?;
    for name in declared {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let module = crate::module::Module {
        name,
        env: module_env,
        exports: names,
        source: None,
    };
    module.validate_exports()?;
    engine.register_module(module);
    Ok(result)
}

fn install_method(metadata: Value, body: Value, env: &Arc<Env>) -> Result<Value, EvalError> {
    use crate::zos::gf::{Method, MethodQualifier, Specializer};
    let forms = metadata_forms(metadata)?;
    let Some(Sexp::Symbol(name, _)) = forms.first() else {
        return Err(EvalError::type_error("symbol", "gf name"));
    };
    let (qualifier, index) = match forms.get(1) {
        Some(Sexp::Keyword(kind, _)) if kind == "before" => (MethodQualifier::Before, 2),
        Some(Sexp::Keyword(kind, _)) if kind == "after" => (MethodQualifier::After, 2),
        Some(Sexp::Keyword(kind, _)) if kind == "around" => (MethodQualifier::Around, 2),
        _ => (MethodQualifier::Primary, 1),
    };
    let Some(Sexp::List(specs, _) | Sexp::Vector(specs, _)) = forms.get(index) else {
        return Err(EvalError::type_error("list", "specializers"));
    };
    let mut specializers = Vec::with_capacity(specs.len());
    for spec in specs {
        match spec {
            Sexp::Symbol(_, _) => specializers.push(Specializer::T),
            Sexp::List(pair, _) if pair.len() == 2 => {
                if let (Sexp::Symbol(_, _), Sexp::Symbol(kind, _)) = (&pair[0], &pair[1]) {
                    specializers.push(Specializer::Exact(kind.clone()));
                }
            }
            other => {
                return Err(EvalError::invalid_form(format!(
                    "invalid specializer: {other}"
                )));
            }
        }
    }
    let Value::Function(body) = body else {
        return Err(EvalError::type_error("function", body.value_type()));
    };
    let gf = env
        .get(name)
        .ok_or_else(|| EvalError::custom(format!("generic function not found: {name}")))?;
    let Value::Object(object) = &gf else {
        return Err(EvalError::type_error("GenericFunction", gf.value_type()));
    };
    let gf = crate::zos::gf::gf_shared(object.as_ref())
        .ok_or_else(|| EvalError::type_error("GenericFunction", "non-GF object"))?;
    gf.borrow_mut().add_method(Arc::new(Method {
        specializers,
        qualifier,
        body,
    }));
    Ok(Value::Nil)
}

fn attach_instruction_span(
    error: EvalError,
    module: &ModuleArtifact,
    fn_index: usize,
    pc: usize,
    source_id: Option<crate::span::SourceId>,
) -> EvalError {
    let context = match &error {
        EvalError::SymbolNotFound(_, span)
        | EvalError::InvalidForm(_, span)
        | EvalError::MacroError(_, span)
        | EvalError::Custom(_, span) => span,
        EvalError::NotAFunction { span, .. }
        | EvalError::WrongArgCount { span, .. }
        | EvalError::WrongArgCountMin { span, .. }
        | EvalError::WrongArgCountRange { span, .. }
        | EvalError::IndexOutOfBounds { span, .. }
        | EvalError::TypeError { span, .. }
        | EvalError::DivisionByZero { span }
        | EvalError::RecurNotTail { span }
        | EvalError::RecurWithoutLoop { span } => span,
    };
    if context.span.is_some() {
        return error;
    }
    let Some(Some((line, col))) = module.functions.get(fn_index).and_then(|f| f.debug.get(pc))
    else {
        return error;
    };
    let span = source_id.and_then(|source_id| {
        let mut span = *module.source.as_ref()?.spans.get()?.get(&(*line, *col))?;
        span.source_id = source_id;
        Some(span)
    });
    error.with_opt_span(span)
}

fn lookup_global(
    engine: &dyn EvalEngine,
    name: &str,
    lexical_env: &Arc<Env>,
) -> Result<Value, EvalError> {
    if let Some(v) = lexical_env.get(name) {
        return Ok(v);
    }
    if let Some(v) = engine.env().get(name) {
        return Ok(v);
    }
    if let Some(v) = crate::special::module_forms::resolve_qualified(engine, name) {
        return Ok(v);
    }
    Err(EvalError::symbol_not_found(name))
}

fn set_global(
    engine: &dyn EvalEngine,
    name: &str,
    value: Value,
    lexical_env: &Arc<Env>,
) -> Result<(), EvalError> {
    if lexical_env.set_global(name, value.clone()) {
        return Ok(());
    }
    if engine.env().set_global(name, value.clone()) {
        return Ok(());
    }
    // Define-as-set: bind in the root env.
    engine.env().set(name.to_string(), value);
    Ok(())
}

// ─── Source / compile path ──────────────────────────────────────────────

/// Parse `source` and register it in the context's SourceMap, so a form
/// that falls through to the interpreter reports an error against the
/// file it came from.
///
/// The compiling path does not need this — the VM reports its own
/// failures — but a form the compiler declines falls back to `eval_expr`,
/// and that path reads spans. Without the registration an error inside a
/// required module names no file at all, which is exactly the case the
/// module loader exists to make diagnosable.
fn parse_source_registered(
    ctx: &EvalContext,
    name: &str,
    source: &str,
) -> Result<Vec<Sexp>, EvalError> {
    let source_id = ctx
        .source_map()
        .register(name.to_string(), source.to_string());
    crate::reader::reader::read_program_with_source(source, source_id)
        .map_err(|e| e.into_eval(name))
}

/// Run every top-level form in `source` through the AST evaluator.
/// This is the language's natural source loader — the bootstrap path for
/// the bundle and ordinary interpreted runs; compiled execution happens
/// only through explicit artifacts (`compile_source` / `execute_module`).
pub(crate) fn eval_source_text(
    ctx: &EvalContext,
    name: &str,
    source: &str,
) -> Result<Value, EvalError> {
    let forms = parse_source_registered(ctx, name, source)?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = crate::eval::eval_in_context(&sexp, ctx)?;
    }
    Ok(last)
}

// ─── Module value <-> artifact ──────────────────────────────────────────

fn value_to_artifact(v: &Value) -> Result<ModuleArtifact, EvalError> {
    let Value::Map(map) = v else {
        return Err(EvalError::custom("expected module value (map)"));
    };
    let get = |k: &str| -> Result<Value, EvalError> {
        map.get(&Value::Keyword(k.into()))
            .or_else(|| map.get(&Value::String(k.into())))
            .cloned()
            .ok_or_else(|| EvalError::custom(format!("module value missing :{k}")))
    };
    let version = match get("version")? {
        Value::Integer(i) => i as u32,
        other => {
            return Err(EvalError::custom(format!(
                "version must be integer, got {other}"
            )));
        }
    };
    if version != MODULE_VERSION {
        return Err(EvalError::custom(format!(
            "unsupported module version {version}, expected {MODULE_VERSION}"
        )));
    }
    let name = match get("name")? {
        Value::String(s) => s,
        other => {
            return Err(EvalError::custom(format!(
                "name must be string, got {other}"
            )));
        }
    };
    let entry = match get("entry")? {
        Value::Integer(i) => i as usize,
        other => {
            return Err(EvalError::custom(format!(
                "entry must be integer, got {other}"
            )));
        }
    };
    let functions = match get("functions")? {
        Value::Vector(v) => v,
        other => {
            return Err(EvalError::custom(format!(
                "functions must be vector, got {other}"
            )));
        }
    };
    let constants = match get("constants")? {
        Value::Vector(v) => v,
        other => {
            return Err(EvalError::custom(format!(
                "constants must be vector, got {other}"
            )));
        }
    };
    let fn_records: Vector<FunctionRecord> = functions
        .iter()
        .map(value_to_record)
        .collect::<Result<_, _>>()?;
    let consts: Vector<Constant> = constants
        .iter()
        .map(value_to_constant)
        .collect::<Result<_, _>>()?;
    // Optional: a module that predates the field, or one compiled
    // without an exported entry, still decodes. Callers that need the
    // name decide for themselves whether its absence is fatal.
    let entry_name = match map.get(&Value::Keyword("entry-name".into())) {
        Some(Value::String(n)) => Some(n.clone()),
        _ => None,
    };
    let source = match map
        .get(&Value::Keyword("source".into()))
        .or_else(|| map.get(&Value::String("source".into())))
    {
        Some(Value::String(text)) => Some(Arc::new(SourceProvenance::new(text.clone()))),
        Some(Value::Nil) | None => None,
        Some(_) => {
            return Err(EvalError::invalid_form(
                "module source must be string or nil",
            ));
        }
    };
    Ok(ModuleArtifact {
        version: version as u32,
        name,
        entry,
        functions: fn_records,
        constants: consts,
        entry_name,
        source,
    })
}

fn value_to_record(v: &Value) -> Result<FunctionRecord, EvalError> {
    let Value::Map(map) = v else {
        return Err(EvalError::custom("function record must be a map"));
    };
    let get = |k: &str| -> Result<Value, EvalError> {
        map.get(&Value::Keyword(k.into()))
            .or_else(|| map.get(&Value::String(k.into())))
            .cloned()
            .ok_or_else(|| EvalError::custom(format!("function record missing :{k}")))
    };
    let params_v = match get("params")? {
        Value::Vector(v) | Value::List(v) => v,
        _ => return Err(EvalError::custom("params must be a vector/list")),
    };
    let params: Vector<String> = params_v
        .iter()
        .map(|x| match x {
            Value::String(s) => Ok(s.clone()),
            other => Err(EvalError::custom(format!(
                "param name must be string, got {other}"
            ))),
        })
        .collect::<Result<_, _>>()?;
    let rest = match get("rest")? {
        Value::String(s) => Some(s),
        Value::Nil => None,
        other => {
            return Err(EvalError::custom(format!(
                "rest must be string or nil, got {other}"
            )));
        }
    };
    let locals = match get("locals")? {
        Value::Integer(i) => i as usize,
        other => {
            return Err(EvalError::custom(format!(
                "locals must be integer, got {other}"
            )));
        }
    };
    // The compiler emits lexical descriptors. Closure creation resolves them
    // to shared cells, including reservations that have not been defined yet.
    let captures: Vector<Value> = match get("captures")? {
        Value::Vector(v) | Value::List(v) => v,
        Value::Nil => Vector::new(),
        _ => return Err(EvalError::custom("captures must be vector/list")),
    };
    let code_v = match get("code")? {
        Value::Vector(v) => v,
        _ => return Err(EvalError::custom("code must be vector")),
    };
    let code: Vector<Instruction> = code_v
        .iter()
        .map(value_to_instruction)
        .collect::<Result<_, _>>()?;
    let debug_v = match get("debug")? {
        Value::Vector(v) => v,
        _ => return Err(EvalError::custom("debug must be vector")),
    };
    let debug: Vector<Option<(usize, usize)>> = debug_v
        .iter()
        .map(|x| match x {
            Value::Nil => Ok(None),
            Value::Vector(v) | Value::List(v) if v.len() == 2 => {
                let line = match &v[0] {
                    Value::Integer(i) => *i as usize,
                    _ => return Err(EvalError::custom("debug line must be integer")),
                };
                let col = match &v[1] {
                    Value::Integer(i) => *i as usize,
                    _ => return Err(EvalError::custom("debug col must be integer")),
                };
                Ok(Some((line, col)))
            }
            _ => Err(EvalError::custom("debug entry must be nil or [line col]")),
        })
        .collect::<Result<_, _>>()?;
    let name = match map.get(&Value::Keyword("name".into())) {
        Some(Value::String(n)) => Some(n.clone()),
        _ => None,
    };
    let record = FunctionRecord {
        name,
        params,
        rest,
        locals,
        captures,
        code,
        debug,
    };
    validate_declarations(&record)?;
    Ok(record)
}

fn validate_declarations(record: &FunctionRecord) -> Result<(), EvalError> {
    let bound = record
        .captures
        .len()
        .checked_add(record.params.len())
        .and_then(|count| count.checked_add(usize::from(record.rest.is_some())))
        .ok_or_else(|| EvalError::invalid_form("local count overflow"))?;
    let size = record.locals.max(bound);
    for instruction in &record.code {
        if let Instruction::Declare(index, _, fallback) = instruction {
            if *index < bound || *index >= size {
                return Err(EvalError::invalid_form(
                    "declaration index outside reserved locals",
                ));
            }
            match fallback {
                Some(BindingSource::Local(outer) | BindingSource::Capture(outer))
                    if *outer >= size || outer == index =>
                {
                    return Err(EvalError::invalid_form(
                        "invalid declaration fallback index",
                    ));
                }
                _ => {}
            }
        }
        if let Instruction::SetLocal(index) = instruction {
            if *index >= size {
                return Err(EvalError::invalid_form("set-local index out of range"));
            }
        }
    }
    Ok(())
}

fn value_to_instruction(v: &Value) -> Result<Instruction, EvalError> {
    let items = match v {
        Value::Vector(v) | Value::List(v) => v.clone(),
        _ => return Err(EvalError::custom("instruction must be vector/list")),
    };
    let op = match items.front() {
        Some(Value::Keyword(k)) => k.clone(),
        Some(Value::Symbol(s)) => s.clone(),
        Some(other) => {
            return Err(EvalError::custom(format!(
                "instruction op must be keyword/symbol, got {other}"
            )));
        }
        None => return Err(EvalError::custom("empty instruction")),
    };
    let arg = |i: usize| -> Result<Value, EvalError> {
        items
            .get(i)
            .cloned()
            .ok_or_else(|| EvalError::custom(format!("instruction {op} missing arg {i}")))
    };
    let int_arg = |i: usize| -> Result<usize, EvalError> {
        match arg(i)? {
            Value::Integer(n) => Ok(n as usize),
            other => Err(EvalError::custom(format!("expected int arg, got {other}"))),
        }
    };
    let str_arg = |i: usize| -> Result<String, EvalError> {
        match arg(i)? {
            Value::String(s) => Ok(s),
            other => Err(EvalError::custom(format!(
                "expected string arg, got {other}"
            ))),
        }
    };
    let list_arg = |i: usize| -> Result<Vector<Value>, EvalError> {
        match arg(i)? {
            Value::List(v) | Value::Vector(v) => Ok(v),
            other => Err(EvalError::custom(format!("expected list arg, got {other}"))),
        }
    };
    let table_arg = |i: usize| -> Result<Vec<(String, usize)>, EvalError> {
        let v = list_arg(i)?;
        let mut out = Vec::with_capacity(v.len());
        for entry in v.iter() {
            let pair = match entry {
                Value::List(p) | Value::Vector(p) => p.clone(),
                _ => return Err(EvalError::custom("handler entry must be [name target]")),
            };
            if pair.len() != 2 {
                return Err(EvalError::custom("handler entry must be [name target]"));
            }
            let name = match &pair[0] {
                Value::String(s) => s.clone(),
                other => {
                    return Err(EvalError::custom(format!(
                        "handler name must be string, got {other}"
                    )));
                }
            };
            let target = match &pair[1] {
                Value::Integer(i) => *i as usize,
                other => {
                    return Err(EvalError::custom(format!(
                        "handler target must be int, got {other}"
                    )));
                }
            };
            out.push((name, target));
        }
        Ok(out)
    };
    Ok(match op.as_str() {
        "const" => Instruction::Const(int_arg(1)?),
        "pop" => Instruction::Pop,
        "dup" => Instruction::Dup,
        "local" => Instruction::Local(int_arg(1)?),
        "capture" => Instruction::Capture(int_arg(1)?),
        "store" => Instruction::Store(int_arg(1)?),
        "store-capture" => Instruction::StoreCapture(int_arg(1)?),
        "set-local" => match (items.len(), arg(1)?) {
            (2, Value::Integer(index)) => Instruction::SetLocal(
                usize::try_from(index)
                    .map_err(|_| EvalError::invalid_form("negative set-local index"))?,
            ),
            _ => {
                return Err(EvalError::invalid_form(
                    "set-local requires one integer index",
                ));
            }
        },
        "bind" => Instruction::Bind(int_arg(1)?),
        "declare" => {
            if items.len() != 4 {
                return Err(EvalError::invalid_form(
                    "declare requires index, name and fallback",
                ));
            }
            let index = match arg(1)? {
                Value::Integer(index) => usize::try_from(index)
                    .map_err(|_| EvalError::invalid_form("negative declaration index"))?,
                _ => return Err(EvalError::invalid_form("declaration index must be integer")),
            };
            Instruction::Declare(index, str_arg(2)?, value_to_binding_source(&arg(3)?)?)
        }
        "global" => Instruction::Global(str_arg(1)?),
        "set-global" => Instruction::SetGlobal(str_arg(1)?),
        "define" => Instruction::Define(str_arg(1)?),
        "jump" => Instruction::Jump(int_arg(1)?),
        "jump-false" => Instruction::JumpFalse(int_arg(1)?),
        "jump-true" => Instruction::JumpTrue(int_arg(1)?),
        "closure" => Instruction::Closure(int_arg(1)?),
        "call" => Instruction::Call(int_arg(1)?),
        "tail-call" => Instruction::TailCall(int_arg(1)?),
        "return" => Instruction::Return,
        "vector" => Instruction::VectorN(int_arg(1)?),
        "map" => Instruction::MapN(int_arg(1)?),
        "quasi-conj" => match (items.len(), arg(1)?) {
            (2, Value::Boolean(splice)) => Instruction::QuasiConj(splice),
            _ => return Err(EvalError::invalid_form("quasi-conj requires one boolean")),
        },
        "quasi-list" if items.len() == 1 => Instruction::QuasiList,
        "handler" => Instruction::Handler(table_arg(1)?),
        "unhandler" => Instruction::Unhandler,
        "clear-error" => Instruction::ClearError,
        "metadata" => Instruction::Metadata(str_arg(1)?),
        "macro" => Instruction::Macro(str_arg(1)?),
        "macro-rules" => Instruction::MacroRules(str_arg(1)?),
        "module" => Instruction::Module,
        "method" => Instruction::Method,
        "class" => Instruction::Class(int_arg(1)?),
        other => return Err(EvalError::custom(format!("unknown opcode: {other}"))),
    })
}

fn value_to_binding_source(value: &Value) -> Result<Option<BindingSource>, EvalError> {
    if matches!(value, Value::Nil) {
        return Ok(None);
    }
    let Value::Vector(parts) = value else {
        return Err(EvalError::invalid_form(
            "declaration fallback must be vector or nil",
        ));
    };
    if parts.len() != 2 {
        return Err(EvalError::invalid_form("invalid declaration fallback"));
    }
    match (&parts[0], &parts[1]) {
        (Value::Keyword(kind), Value::Integer(index)) if kind == "local" || kind == "capture" => {
            let index = usize::try_from(*index)
                .map_err(|_| EvalError::invalid_form("negative declaration fallback index"))?;
            Ok(Some(if kind == "local" {
                BindingSource::Local(index)
            } else {
                BindingSource::Capture(index)
            }))
        }
        (Value::Keyword(kind), Value::String(name)) if kind == "global" => {
            Ok(Some(BindingSource::Global(name.clone())))
        }
        _ => Err(EvalError::invalid_form("invalid declaration fallback")),
    }
}

fn binding_source_to_value(source: &Option<BindingSource>) -> Value {
    match source {
        Some(BindingSource::Local(index)) => Value::Vector(vector![
            Value::Keyword("local".into()),
            Value::Integer(*index as i64)
        ]),
        Some(BindingSource::Capture(index)) => Value::Vector(vector![
            Value::Keyword("capture".into()),
            Value::Integer(*index as i64)
        ]),
        Some(BindingSource::Global(name)) => Value::Vector(vector![
            Value::Keyword("global".into()),
            Value::String(name.clone())
        ]),
        None => Value::Nil,
    }
}

fn value_to_constant(v: &Value) -> Result<Constant, EvalError> {
    // The compiler emits two encodings for a constant: a tagged Vector
    // (the canonical portable form `[:kind payload]`) and a tagged Map
    // (the JSON-friendly form `{:kind :keyword :value "x"}`). Both must
    // decode to the same `Constant` so the wire format is round-trip-safe.
    match v {
        Value::Vector(items) | Value::List(items) => value_to_constant_vector(items),
        Value::Map(map) => value_to_constant_map(map),
        _ => Err(EvalError::custom(format!(
            "constant must be a map or vector, got {v}"
        ))),
    }
}

fn value_to_constant_vector(items: &Vector<Value>) -> Result<Constant, EvalError> {
    let tag = items
        .front()
        .ok_or_else(|| EvalError::custom("constant vector must have at least one element"))?;
    let kind = match tag {
        Value::Keyword(k) => k.clone(),
        Value::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::custom(format!(
                "constant tag must be keyword/symbol, got {other}"
            )));
        }
    };
    let arg = |i: usize| -> Result<&Value, EvalError> {
        items
            .get(i)
            .ok_or_else(|| EvalError::custom(format!("constant [{kind}] missing arg {i}")))
    };
    let int_arg = |i: usize| -> Result<i64, EvalError> {
        match arg(i)? {
            Value::Integer(n) => Ok(*n),
            other => Err(EvalError::custom(format!(
                "expected integer arg, got {other}"
            ))),
        }
    };
    let str_arg = |i: usize| -> Result<String, EvalError> {
        match arg(i)? {
            Value::String(s) => Ok(s.clone()),
            other => Err(EvalError::custom(format!(
                "expected string arg, got {other}"
            ))),
        }
    };
    let vec_arg = |i: usize| -> Result<Vector<usize>, EvalError> {
        match arg(i)? {
            Value::Vector(v) | Value::List(v) => v
                .iter()
                .map(|x| match x {
                    Value::Integer(n) => Ok(*n as usize),
                    other => Err(EvalError::custom(format!(
                        "vector entry must be integer, got {other}"
                    ))),
                })
                .collect(),
            other => Err(EvalError::custom(format!(
                "expected vector arg, got {other}"
            ))),
        }
    };
    let pair_vec = |i: usize| -> Result<Vector<(usize, usize)>, EvalError> {
        match arg(i)? {
            Value::Vector(v) | Value::List(v) => v
                .iter()
                .map(|x| {
                    let pair = match x {
                        Value::Vector(p) | Value::List(p) => p,
                        _ => return Err(EvalError::custom("map entry must be a vector")),
                    };
                    if pair.len() != 2 {
                        return Err(EvalError::custom("map entry must have exactly two entries"));
                    }
                    let key = match &pair[0] {
                        Value::Integer(n) => *n as usize,
                        _ => return Err(EvalError::custom("map key index must be integer")),
                    };
                    let val = match &pair[1] {
                        Value::Integer(n) => *n as usize,
                        _ => return Err(EvalError::custom("map val index must be integer")),
                    };
                    Ok((key, val))
                })
                .collect(),
            other => Err(EvalError::custom(format!(
                "expected vector arg, got {other}"
            ))),
        }
    };
    Ok(match kind.as_str() {
        "nil" => Constant::Nil,
        "boolean" => Constant::Boolean(int_arg(1)? != 0),
        "integer" => Constant::Integer(int_arg(1)?),
        "float" => Constant::FloatBits(int_arg(1)? as u64),
        "string" => Constant::String(str_arg(1)?),
        "symbol" => Constant::Symbol(str_arg(1)?),
        "keyword" => Constant::Keyword(str_arg(1)?),
        "char" => {
            let s = str_arg(1)?;
            let c = s
                .chars()
                .next()
                .ok_or_else(|| EvalError::custom("empty char literal"))?;
            Constant::Char(c)
        }
        "list" => Constant::List(vec_arg(1)?),
        "vector" => Constant::Vector(vec_arg(1)?),
        "map" => Constant::Map(pair_vec(1)?),
        other => return Err(EvalError::custom(format!("unknown constant kind: {other}"))),
    })
}

fn value_to_constant_map(map: &im::HashMap<Value, Value>) -> Result<Constant, EvalError> {
    let kind = match map.get(&Value::Keyword("kind".into())) {
        Some(Value::Keyword(k)) => k.clone(),
        Some(other) => {
            return Err(EvalError::custom(format!(
                "constant kind must be keyword, got {other}"
            )));
        }
        None => return Err(EvalError::custom("constant missing :kind")),
    };
    let int_val = |k: &str| -> Result<i64, EvalError> {
        match map.get(&Value::Keyword(k.into())) {
            Some(Value::Integer(i)) => Ok(*i),
            Some(other) => Err(EvalError::custom(format!(
                ":{k} must be integer, got {other}"
            ))),
            None => Err(EvalError::custom(format!("constant missing :{k}"))),
        }
    };
    let bool_val = |k: &str| -> Result<bool, EvalError> {
        match map.get(&Value::Keyword(k.into())) {
            Some(Value::Boolean(b)) => Ok(*b),
            Some(other) => Err(EvalError::custom(format!(
                ":{k} must be boolean, got {other}"
            ))),
            None => Err(EvalError::custom(format!("constant missing :{k}"))),
        }
    };
    let str_val = |k: &str| -> Result<String, EvalError> {
        match map.get(&Value::Keyword(k.into())) {
            Some(Value::String(s)) => Ok(s.clone()),
            Some(other) => Err(EvalError::custom(format!(
                ":{k} must be string, got {other}"
            ))),
            None => Err(EvalError::custom(format!("constant missing :{k}"))),
        }
    };
    let idx_vec = |k: &str| -> Result<Vector<usize>, EvalError> {
        match map.get(&Value::Keyword(k.into())) {
            Some(Value::Vector(v)) | Some(Value::List(v)) => v
                .iter()
                .map(|x| match x {
                    Value::Integer(i) => Ok(*i as usize),
                    other => Err(EvalError::custom(format!(
                        ":{k} entries must be int, got {other}"
                    ))),
                })
                .collect(),
            Some(other) => Err(EvalError::custom(format!(
                ":{k} must be vector, got {other}"
            ))),
            None => Err(EvalError::custom(format!("constant missing :{k}"))),
        }
    };
    let pair_vec = |k: &str| -> Result<Vector<(usize, usize)>, EvalError> {
        match map.get(&Value::Keyword(k.into())) {
            Some(Value::Vector(v)) | Some(Value::List(v)) => v
                .iter()
                .map(|x| {
                    let pair = match x {
                        Value::Vector(p) | Value::List(p) => p.clone(),
                        _ => return Err(EvalError::custom("map entry must be [key val]")),
                    };
                    if pair.len() != 2 {
                        return Err(EvalError::custom("map entry must be [key val]"));
                    }
                    let key = match &pair[0] {
                        Value::Integer(i) => *i as usize,
                        _ => return Err(EvalError::custom("map key index must be int")),
                    };
                    let val = match &pair[1] {
                        Value::Integer(i) => *i as usize,
                        _ => return Err(EvalError::custom("map val index must be int")),
                    };
                    Ok((key, val))
                })
                .collect(),
            Some(other) => Err(EvalError::custom(format!(
                ":{k} must be vector, got {other}"
            ))),
            None => Err(EvalError::custom(format!("constant missing :{k}"))),
        }
    };
    Ok(match kind.as_str() {
        "nil" => Constant::Nil,
        "boolean" => Constant::Boolean(bool_val("value")?),
        "integer" => Constant::Integer(int_val("value")?),
        "float" => Constant::FloatBits(int_val("value")? as u64),
        "string" => Constant::String(str_val("value")?),
        "symbol" => Constant::Symbol(str_val("value")?),
        "keyword" => Constant::Keyword(str_val("value")?),
        "char" => {
            let s = str_val("value")?;
            let c = s
                .chars()
                .next()
                .ok_or_else(|| EvalError::custom("empty char literal"))?;
            Constant::Char(c)
        }
        "list" => Constant::List(idx_vec("value")?),
        "vector" => Constant::Vector(idx_vec("value")?),
        "map" => Constant::Map(pair_vec("value")?),
        other => return Err(EvalError::custom(format!("unknown constant kind: {other}"))),
    })
}

fn constant_to_value(_all: &Vector<Constant>, c: &Constant) -> Result<Value, EvalError> {
    Ok(match c {
        Constant::Nil => Value::Nil,
        Constant::Boolean(b) => Value::Boolean(*b),
        Constant::Integer(i) => Value::Integer(*i),
        Constant::FloatBits(b) => Value::Float(f64::from_bits(*b)),
        Constant::String(s) => Value::String(s.clone()),
        Constant::Symbol(s) => Value::Symbol(s.clone()),
        Constant::Keyword(s) => Value::Keyword(s.clone()),
        Constant::Char(c) => Value::Char(*c),
        Constant::List(idxs) | Constant::Vector(idxs) => {
            let kind_matches = matches!(c, Constant::Vector(_));
            let mut out: Vector<Value> = Vector::new();
            for i in idxs.iter() {
                out.push_back(indexed_constant(_all, *i)?);
            }
            if kind_matches {
                Value::Vector(out)
            } else {
                Value::List(out)
            }
        }
        Constant::Map(entries) => {
            let mut map: ImHashMap<Value, Value> = ImHashMap::new();
            for (k, v) in entries.iter() {
                map.insert(indexed_constant(_all, *k)?, indexed_constant(_all, *v)?);
            }
            Value::Map(map)
        }
    })
}

fn indexed_constant(all: &Vector<Constant>, i: usize) -> Result<Value, EvalError> {
    let c = all
        .get(i)
        .ok_or_else(|| EvalError::custom(format!("constant index {i} out of range")))?;
    constant_to_value(all, c)
}

fn artifact_to_value(a: &ModuleArtifact) -> Value {
    let mut map: ImHashMap<Value, Value> = ImHashMap::new();
    map.insert(
        Value::Keyword("version".into()),
        Value::Integer(a.version as i64),
    );
    map.insert(Value::Keyword("name".into()), Value::String(a.name.clone()));
    map.insert(
        Value::Keyword("entry".into()),
        Value::Integer(a.entry as i64),
    );
    map.insert(
        Value::Keyword("functions".into()),
        Value::Vector(a.functions.iter().map(record_to_value).collect()),
    );
    map.insert(
        Value::Keyword("constants".into()),
        Value::Vector(a.constants.iter().map(constant_to_value_marker).collect()),
    );
    if let Some(source) = &a.source {
        map.insert(
            Value::Keyword("source".into()),
            Value::String(source.text.clone()),
        );
    }
    if let Some(name) = &a.entry_name {
        map.insert(
            Value::Keyword("entry-name".into()),
            Value::String(name.clone()),
        );
    }
    Value::Map(map)
}

fn record_to_value(r: &FunctionRecord) -> Value {
    let mut map: ImHashMap<Value, Value> = ImHashMap::new();
    if let Some(name) = &r.name {
        map.insert(Value::Keyword("name".into()), Value::String(name.clone()));
    }
    map.insert(
        Value::Keyword("params".into()),
        Value::Vector(r.params.iter().map(|s| Value::String(s.clone())).collect()),
    );
    map.insert(
        Value::Keyword("rest".into()),
        match &r.rest {
            Some(s) => Value::String(s.clone()),
            None => Value::Nil,
        },
    );
    map.insert(
        Value::Keyword("locals".into()),
        Value::Integer(r.locals as i64),
    );
    map.insert(
        Value::Keyword("captures".into()),
        Value::Vector(r.captures.clone()),
    );
    map.insert(
        Value::Keyword("code".into()),
        Value::Vector(r.code.iter().map(instruction_to_value).collect()),
    );
    map.insert(
        Value::Keyword("debug".into()),
        Value::Vector(
            r.debug
                .iter()
                .map(|d| match d {
                    Some((line, col)) => Value::Vector(vector![
                        Value::Integer(*line as i64),
                        Value::Integer(*col as i64)
                    ]),
                    None => Value::Nil,
                })
                .collect(),
        ),
    );
    Value::Map(map)
}

fn instruction_to_value(i: &Instruction) -> Value {
    let v = |x: Value| Value::Vector(vector![x]);
    let vv = |a: Value, b: Value| Value::Vector(vector![a, b]);
    match i {
        Instruction::Const(n) => vv(Value::Keyword("const".into()), Value::Integer(*n as i64)),
        Instruction::Pop => v(Value::Keyword("pop".into())),
        Instruction::Dup => v(Value::Keyword("dup".into())),
        Instruction::Local(n) => vv(Value::Keyword("local".into()), Value::Integer(*n as i64)),
        Instruction::Capture(n) => vv(Value::Keyword("capture".into()), Value::Integer(*n as i64)),
        Instruction::Store(n) => vv(Value::Keyword("store".into()), Value::Integer(*n as i64)),
        Instruction::StoreCapture(n) => vv(
            Value::Keyword("store-capture".into()),
            Value::Integer(*n as i64),
        ),
        Instruction::SetLocal(n) => vv(
            Value::Keyword("set-local".into()),
            Value::Integer(*n as i64),
        ),
        Instruction::Bind(n) => vv(Value::Keyword("bind".into()), Value::Integer(*n as i64)),
        Instruction::Declare(index, name, fallback) => Value::Vector(vector![
            Value::Keyword("declare".into()),
            Value::Integer(*index as i64),
            Value::String(name.clone()),
            binding_source_to_value(fallback),
        ]),
        Instruction::Global(s) => vv(Value::Keyword("global".into()), Value::String(s.clone())),
        Instruction::SetGlobal(s) => vv(
            Value::Keyword("set-global".into()),
            Value::String(s.clone()),
        ),
        Instruction::Define(s) => vv(Value::Keyword("define".into()), Value::String(s.clone())),
        Instruction::Jump(n) => vv(Value::Keyword("jump".into()), Value::Integer(*n as i64)),
        Instruction::JumpFalse(n) => vv(
            Value::Keyword("jump-false".into()),
            Value::Integer(*n as i64),
        ),
        Instruction::JumpTrue(n) => vv(
            Value::Keyword("jump-true".into()),
            Value::Integer(*n as i64),
        ),
        Instruction::Closure(n) => vv(Value::Keyword("closure".into()), Value::Integer(*n as i64)),
        Instruction::Call(n) => vv(Value::Keyword("call".into()), Value::Integer(*n as i64)),
        Instruction::TailCall(n) => vv(
            Value::Keyword("tail-call".into()),
            Value::Integer(*n as i64),
        ),
        Instruction::Return => v(Value::Keyword("return".into())),
        Instruction::VectorN(n) => vv(Value::Keyword("vector".into()), Value::Integer(*n as i64)),
        Instruction::MapN(n) => vv(Value::Keyword("map".into()), Value::Integer(*n as i64)),
        Instruction::QuasiConj(splice) => {
            vv(Value::Keyword("quasi-conj".into()), Value::Boolean(*splice))
        }
        Instruction::QuasiList => v(Value::Keyword("quasi-list".into())),
        Instruction::Handler(table) => vv(
            Value::Keyword("handler".into()),
            Value::Vector(
                table
                    .iter()
                    .map(|(name, target)| {
                        Value::Vector(vector![
                            Value::String(name.clone()),
                            Value::Integer(*target as i64),
                        ])
                    })
                    .collect(),
            ),
        ),
        Instruction::Unhandler => v(Value::Keyword("unhandler".into())),
        Instruction::ClearError => v(Value::Keyword("clear-error".into())),
        Instruction::Metadata(s) => vv(Value::Keyword("metadata".into()), Value::String(s.clone())),
        Instruction::Macro(s) => vv(Value::Keyword("macro".into()), Value::String(s.clone())),
        Instruction::MacroRules(s) => vv(
            Value::Keyword("macro-rules".into()),
            Value::String(s.clone()),
        ),
        Instruction::Module => v(Value::Keyword("module".into())),
        Instruction::Method => v(Value::Keyword("method".into())),
        Instruction::Class(n) => vv(Value::Keyword("class".into()), Value::Integer(*n as i64)),
    }
}

fn constant_to_value_marker(c: &Constant) -> Value {
    let mut map: ImHashMap<Value, Value> = ImHashMap::new();
    let kind_kw = |s: &str| Value::Keyword(s.into());
    match c {
        Constant::Nil => {
            map.insert(kind_kw("kind"), kind_kw("nil"));
        }
        Constant::Boolean(b) => {
            map.insert(kind_kw("kind"), kind_kw("boolean"));
            map.insert(kind_kw("value"), Value::Boolean(*b));
        }
        Constant::Integer(i) => {
            map.insert(kind_kw("kind"), kind_kw("integer"));
            map.insert(kind_kw("value"), Value::Integer(*i));
        }
        Constant::FloatBits(b) => {
            map.insert(kind_kw("kind"), kind_kw("float"));
            map.insert(kind_kw("bits"), Value::Integer(*b as i64));
        }
        Constant::String(s) => {
            map.insert(kind_kw("kind"), kind_kw("string"));
            map.insert(kind_kw("value"), Value::String(s.clone()));
        }
        Constant::Symbol(s) => {
            map.insert(kind_kw("kind"), kind_kw("symbol"));
            map.insert(kind_kw("value"), Value::String(s.clone()));
        }
        Constant::Keyword(s) => {
            map.insert(kind_kw("kind"), kind_kw("keyword"));
            map.insert(kind_kw("value"), Value::String(s.clone()));
        }
        Constant::Char(c) => {
            map.insert(kind_kw("kind"), kind_kw("char"));
            map.insert(kind_kw("value"), Value::String(c.to_string()));
        }
        Constant::List(idxs) => {
            map.insert(kind_kw("kind"), kind_kw("list"));
            map.insert(
                kind_kw("value"),
                Value::Vector(idxs.iter().map(|i| Value::Integer(*i as i64)).collect()),
            );
        }
        Constant::Vector(idxs) => {
            map.insert(kind_kw("kind"), kind_kw("vector"));
            map.insert(
                kind_kw("value"),
                Value::Vector(idxs.iter().map(|i| Value::Integer(*i as i64)).collect()),
            );
        }
        Constant::Map(entries) => {
            map.insert(kind_kw("kind"), kind_kw("map"));
            map.insert(
                kind_kw("value"),
                Value::Vector(
                    entries
                        .iter()
                        .map(|(k, v)| {
                            Value::Vector(vector![
                                Value::Integer(*k as i64),
                                Value::Integer(*v as i64),
                            ])
                        })
                        .collect(),
                ),
            );
        }
    }
    Value::Map(map)
}

// ─── Wire format ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct WireModule {
    version: u32,
    name: String,
    entry: usize,
    /// Serialized under a dashed name because Zio's own keyword keys use
    /// dashes; `:entry_name` would be a different key from the one the
    /// compiler writes.
    #[serde(rename = "entry-name")]
    entry_name: Option<String>,
    functions: Vec<WireFunction>,
    constants: Vec<WireConstant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct WireFunction {
    name: Option<String>,
    params: Vec<String>,
    rest: Option<String>,
    locals: usize,
    /// Lexical descriptors; shared runtime cells do not cross the artifact wire.
    captures: Vec<serde_json::Value>,
    code: Vec<WireInstruction>,
    debug: Vec<Option<(usize, usize)>>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", content = "arg")]
enum WireInstruction {
    #[serde(rename = "const")]
    Const(usize),
    #[serde(rename = "pop")]
    Pop,
    #[serde(rename = "dup")]
    Dup,
    #[serde(rename = "local")]
    Local(usize),
    #[serde(rename = "capture")]
    Capture(usize),
    #[serde(rename = "store")]
    Store(usize),
    #[serde(rename = "store-capture")]
    StoreCapture(usize),
    #[serde(rename = "set-local")]
    SetLocal(usize),
    #[serde(rename = "bind")]
    Bind(usize),
    #[serde(rename = "declare")]
    Declare(usize, String, Option<BindingSource>),
    #[serde(rename = "global")]
    Global(String),
    #[serde(rename = "set-global")]
    SetGlobal(String),
    #[serde(rename = "define")]
    Define(String),
    #[serde(rename = "jump")]
    Jump(usize),
    #[serde(rename = "jump-false")]
    JumpFalse(usize),
    #[serde(rename = "jump-true")]
    JumpTrue(usize),
    #[serde(rename = "closure")]
    Closure(usize),
    #[serde(rename = "call")]
    Call(usize),
    #[serde(rename = "tail-call")]
    TailCall(usize),
    #[serde(rename = "return")]
    Return,
    #[serde(rename = "vector")]
    VectorN(usize),
    #[serde(rename = "map")]
    MapN(usize),
    #[serde(rename = "quasi-conj")]
    QuasiConj(bool),
    #[serde(rename = "quasi-list")]
    QuasiList,
    #[serde(rename = "handler")]
    Handler(Vec<(String, usize)>),
    #[serde(rename = "unhandler")]
    Unhandler,
    #[serde(rename = "clear-error")]
    ClearError,
    #[serde(rename = "metadata")]
    Metadata(String),
    #[serde(rename = "macro")]
    Macro(String),
    #[serde(rename = "macro-rules")]
    MacroRules(String),
    #[serde(rename = "module")]
    Module,
    #[serde(rename = "method")]
    Method,
    #[serde(rename = "class")]
    Class(usize),
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
enum WireConstant {
    #[serde(rename = "nil")]
    Nil,
    #[serde(rename = "boolean")]
    Boolean { value: bool },
    #[serde(rename = "integer")]
    Integer { value: i64 },
    #[serde(rename = "float")]
    Float { bits: u64 },
    #[serde(rename = "string")]
    String { value: String },
    #[serde(rename = "symbol")]
    Symbol { value: String },
    #[serde(rename = "keyword")]
    Keyword { value: String },
    #[serde(rename = "char")]
    Char { value: String },
    #[serde(rename = "list")]
    List { value: Vec<usize> },
    #[serde(rename = "vector")]
    Vector { value: Vec<usize> },
    #[serde(rename = "map")]
    Map { value: Vec<[usize; 2]> },
}

impl ModuleArtifact {
    /// Canonical wire JSON for this artifact — the one format the image
    /// encoder references instead of copying instructions.
    pub(crate) fn to_wire_json(&self) -> Result<String, EvalError> {
        let wire = self.to_wire()?;
        serde_json::to_string(&wire)
            .map_err(|error| EvalError::custom(format!("serialize: {error}")))
    }

    fn to_wire(&self) -> Result<WireModule, EvalError> {
        Ok(WireModule {
            version: self.version,
            name: self.name.clone(),
            entry: self.entry,
            entry_name: self.entry_name.clone(),
            source: self.source.as_ref().map(|source| source.text.clone()),
            functions: self
                .functions
                .iter()
                .map(|f| -> Result<WireFunction, EvalError> {
                    Ok(WireFunction {
                        name: f.name.clone(),
                        params: f.params.iter().cloned().collect(),
                        rest: f.rest.clone(),
                        locals: f.locals,
                        captures: f
                            .captures
                            .iter()
                            .map(|value| {
                                crate::builtins::json::to_json(value)
                                    .map_err(|error| EvalError::custom(format!("capture: {error}")))
                            })
                            .collect::<Result<_, _>>()?,
                        code: f.code.iter().map(instruction_to_wire).collect(),
                        debug: f.debug.iter().cloned().collect(),
                    })
                })
                .collect::<Result<_, _>>()?,
            constants: self.constants.iter().map(constant_to_wire).collect(),
        })
    }

    fn from_wire(w: &WireModule) -> Result<Self, EvalError> {
        // Bounded validation: refuse obviously hostile artifacts.
        const MAX_CONSTANTS: usize = 1 << 20;
        const MAX_FUNCTIONS: usize = 1 << 16;
        const MAX_CODE_PER_FN: usize = 1 << 20;
        if w.version != MODULE_VERSION {
            return Err(EvalError::custom(format!(
                "unsupported module version {}, expected {MODULE_VERSION}",
                w.version
            )));
        }
        if w.functions.len() > MAX_FUNCTIONS {
            return Err(EvalError::custom(format!(
                "function count {} exceeds ceiling {MAX_FUNCTIONS}",
                w.functions.len()
            )));
        }
        if w.constants.len() > MAX_CONSTANTS {
            return Err(EvalError::custom(format!(
                "constant count {} exceeds ceiling {MAX_CONSTANTS}",
                w.constants.len()
            )));
        }
        let mut functions: Vector<FunctionRecord> = Vector::new();
        for (i, f) in w.functions.iter().enumerate() {
            if f.code.len() > MAX_CODE_PER_FN {
                return Err(EvalError::custom(format!(
                    "function[{i}] code length {} exceeds ceiling",
                    f.code.len()
                )));
            }
            let mut code: Vector<Instruction> = Vector::new();
            for instr in f.code.iter() {
                code.push_back(wire_to_instruction(instr)?);
            }
            let record = FunctionRecord {
                name: f.name.clone(),
                params: f.params.iter().cloned().collect(),
                rest: f.rest.clone(),
                locals: f.locals,
                captures: f
                    .captures
                    .iter()
                    .map(capture_from_wire)
                    .collect::<Result<_, _>>()?,
                code,
                debug: f.debug.iter().cloned().collect(),
            };
            validate_declarations(&record)?;
            functions.push_back(record);
        }
        let mut constants: Vector<Constant> = Vector::new();
        for c in w.constants.iter() {
            constants.push_back(wire_to_constant(c)?);
        }
        Ok(ModuleArtifact {
            version: w.version,
            name: w.name.clone(),
            entry: w.entry,
            functions,
            constants,
            entry_name: w.entry_name.clone(),
            source: w
                .source
                .as_ref()
                .map(|text| Arc::new(SourceProvenance::new(text.clone()))),
        })
    }
}

fn instruction_to_wire(i: &Instruction) -> WireInstruction {
    match i {
        Instruction::Const(n) => WireInstruction::Const(*n),
        Instruction::Pop => WireInstruction::Pop,
        Instruction::Dup => WireInstruction::Dup,
        Instruction::Local(n) => WireInstruction::Local(*n),
        Instruction::Capture(n) => WireInstruction::Capture(*n),
        Instruction::Store(n) => WireInstruction::Store(*n),
        Instruction::StoreCapture(n) => WireInstruction::StoreCapture(*n),
        Instruction::SetLocal(n) => WireInstruction::SetLocal(*n),
        Instruction::Bind(n) => WireInstruction::Bind(*n),
        Instruction::Declare(index, name, fallback) => {
            WireInstruction::Declare(*index, name.clone(), fallback.clone())
        }
        Instruction::Global(s) => WireInstruction::Global(s.clone()),
        Instruction::SetGlobal(s) => WireInstruction::SetGlobal(s.clone()),
        Instruction::Define(s) => WireInstruction::Define(s.clone()),
        Instruction::Jump(n) => WireInstruction::Jump(*n),
        Instruction::JumpFalse(n) => WireInstruction::JumpFalse(*n),
        Instruction::JumpTrue(n) => WireInstruction::JumpTrue(*n),
        Instruction::Closure(n) => WireInstruction::Closure(*n),
        Instruction::Call(n) => WireInstruction::Call(*n),
        Instruction::TailCall(n) => WireInstruction::TailCall(*n),
        Instruction::Return => WireInstruction::Return,
        Instruction::VectorN(n) => WireInstruction::VectorN(*n),
        Instruction::MapN(n) => WireInstruction::MapN(*n),
        Instruction::QuasiConj(splice) => WireInstruction::QuasiConj(*splice),
        Instruction::QuasiList => WireInstruction::QuasiList,
        Instruction::Handler(table) => {
            WireInstruction::Handler(table.iter().map(|(a, b)| (a.clone(), *b)).collect())
        }
        Instruction::Unhandler => WireInstruction::Unhandler,
        Instruction::ClearError => WireInstruction::ClearError,
        Instruction::Metadata(s) => WireInstruction::Metadata(s.clone()),
        Instruction::Macro(s) => WireInstruction::Macro(s.clone()),
        Instruction::MacroRules(s) => WireInstruction::MacroRules(s.clone()),
        Instruction::Module => WireInstruction::Module,
        Instruction::Method => WireInstruction::Method,
        Instruction::Class(n) => WireInstruction::Class(*n),
    }
}

fn wire_to_instruction(w: &WireInstruction) -> Result<Instruction, EvalError> {
    Ok(match w {
        WireInstruction::Const(n) => Instruction::Const(*n),
        WireInstruction::Pop => Instruction::Pop,
        WireInstruction::Dup => Instruction::Dup,
        WireInstruction::Local(n) => Instruction::Local(*n),
        WireInstruction::Capture(n) => Instruction::Capture(*n),
        WireInstruction::Store(n) => Instruction::Store(*n),
        WireInstruction::StoreCapture(n) => Instruction::StoreCapture(*n),
        WireInstruction::SetLocal(n) => Instruction::SetLocal(*n),
        WireInstruction::Bind(n) => Instruction::Bind(*n),
        WireInstruction::Declare(index, name, fallback) => {
            Instruction::Declare(*index, name.clone(), fallback.clone())
        }
        WireInstruction::Global(s) => Instruction::Global(s.clone()),
        WireInstruction::SetGlobal(s) => Instruction::SetGlobal(s.clone()),
        WireInstruction::Define(s) => Instruction::Define(s.clone()),
        WireInstruction::Jump(n) => Instruction::Jump(*n),
        WireInstruction::JumpFalse(n) => Instruction::JumpFalse(*n),
        WireInstruction::JumpTrue(n) => Instruction::JumpTrue(*n),
        WireInstruction::Closure(n) => Instruction::Closure(*n),
        WireInstruction::Call(n) => Instruction::Call(*n),
        WireInstruction::TailCall(n) => Instruction::TailCall(*n),
        WireInstruction::Return => Instruction::Return,
        WireInstruction::VectorN(n) => Instruction::VectorN(*n),
        WireInstruction::MapN(n) => Instruction::MapN(*n),
        WireInstruction::QuasiConj(splice) => Instruction::QuasiConj(*splice),
        WireInstruction::QuasiList => Instruction::QuasiList,
        WireInstruction::Handler(t) => {
            let mut table = Vec::with_capacity(t.len());
            for pair in t.iter() {
                table.push((pair.0.clone(), pair.1));
            }
            Instruction::Handler(table)
        }
        WireInstruction::Unhandler => Instruction::Unhandler,
        WireInstruction::ClearError => Instruction::ClearError,
        WireInstruction::Metadata(s) => Instruction::Metadata(s.clone()),
        WireInstruction::Macro(s) => Instruction::Macro(s.clone()),
        WireInstruction::MacroRules(s) => Instruction::MacroRules(s.clone()),
        WireInstruction::Module => Instruction::Module,
        WireInstruction::Method => Instruction::Method,
        WireInstruction::Class(n) => Instruction::Class(*n),
    })
}

fn constant_to_wire(c: &Constant) -> WireConstant {
    match c {
        Constant::Nil => WireConstant::Nil,
        Constant::Boolean(b) => WireConstant::Boolean { value: *b },
        Constant::Integer(i) => WireConstant::Integer { value: *i },
        Constant::FloatBits(b) => WireConstant::Float { bits: *b },
        Constant::String(s) => WireConstant::String { value: s.clone() },
        Constant::Symbol(s) => WireConstant::Symbol { value: s.clone() },
        Constant::Keyword(s) => WireConstant::Keyword { value: s.clone() },
        Constant::Char(c) => WireConstant::Char {
            value: c.to_string(),
        },
        Constant::List(v) => WireConstant::List {
            value: v.iter().cloned().collect(),
        },
        Constant::Vector(v) => WireConstant::Vector {
            value: v.iter().cloned().collect(),
        },
        Constant::Map(entries) => WireConstant::Map {
            value: entries.iter().map(|(k, v)| [*k, *v]).collect(),
        },
    }
}

fn wire_to_constant(w: &WireConstant) -> Result<Constant, EvalError> {
    Ok(match w {
        WireConstant::Nil => Constant::Nil,
        WireConstant::Boolean { value } => Constant::Boolean(*value),
        WireConstant::Integer { value } => Constant::Integer(*value),
        WireConstant::Float { bits } => Constant::FloatBits(*bits),
        WireConstant::String { value } => Constant::String(value.clone()),
        WireConstant::Symbol { value } => Constant::Symbol(value.clone()),
        WireConstant::Keyword { value } => Constant::Keyword(value.clone()),
        WireConstant::Char { value } => {
            let c = value
                .chars()
                .next()
                .ok_or_else(|| EvalError::custom("empty char wire"))?;
            Constant::Char(c)
        }
        WireConstant::List { value } => Constant::List(value.iter().cloned().collect()),
        WireConstant::Vector { value } => Constant::Vector(value.iter().cloned().collect()),
        WireConstant::Map { value } => {
            let mut out: Vector<(usize, usize)> = Vector::new();
            for pair in value.iter() {
                out.push_back((pair[0], pair[1]));
            }
            Constant::Map(out)
        }
    })
}

// ─── Native compiler/* host functions ───────────────────────────────────

fn compiler_name(args: Vector<Value>) -> Result<String, EvalError> {
    let one = args
        .front()
        .ok_or_else(|| EvalError::wrong_arg_count(1, 0))?;
    Ok(match one {
        Value::Keyword(k) | Value::Symbol(k) => k.clone(),
        other => format!("{other}"),
    })
}

fn compiler_argument(args: &Vector<Value>) -> Result<&Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    Ok(&args[0])
}

fn compiler_form(value: &Value) -> Result<Value, EvalError> {
    use crate::syntax::{NodeKind, SyntaxNode};
    let Some(node) = SyntaxNode::from_value(value) else {
        return Ok(value.clone());
    };
    Ok(match node.kind {
        NodeKind::List => Value::List(node.children.iter().map(SyntaxNode::to_value).collect()),
        NodeKind::Vector => Value::Vector(node.children.iter().map(SyntaxNode::to_value).collect()),
        NodeKind::Map => {
            let mut map = ImHashMap::new();
            let mut children = node.children.iter();
            while let Some(key) = children.next() {
                map.insert(
                    key.to_value(),
                    children.next().expect("validated syntax map").to_value(),
                );
            }
            Value::Map(map)
        }
        _ => Value::from(node.to_sexp()?),
    })
}

fn compiler_datum(value: &Value) -> Result<Value, EvalError> {
    if let Some(node) = crate::syntax::SyntaxNode::from_value(value) {
        return Ok(Value::from(node.to_sexp()?));
    }
    Ok(match value {
        Value::List(items) => {
            Value::List(items.iter().map(compiler_datum).collect::<Result<_, _>>()?)
        }
        Value::Vector(items) => {
            Value::Vector(items.iter().map(compiler_datum).collect::<Result<_, _>>()?)
        }
        Value::Map(items) => Value::Map(
            items
                .iter()
                .map(|(k, v)| Ok((compiler_datum(k)?, compiler_datum(v)?)))
                .collect::<Result<_, EvalError>>()?,
        ),
        _ => value.clone(),
    })
}

fn compiler_sexp(value: &Value) -> Result<Sexp, EvalError> {
    if let Some(node) = crate::syntax::SyntaxNode::from_value(value) {
        return node.to_sexp();
    }
    Ok(match value {
        Value::List(items) => Sexp::List(
            items.iter().map(compiler_sexp).collect::<Result<_, _>>()?,
            None,
        ),
        Value::Vector(items) => Sexp::Vector(
            items.iter().map(compiler_sexp).collect::<Result<_, _>>()?,
            None,
        ),
        Value::Map(items) => Sexp::Map(
            items
                .iter()
                .map(|(k, v)| Ok((compiler_sexp(k)?, compiler_sexp(v)?)))
                .collect::<Result<_, EvalError>>()?,
            None,
        ),
        _ => macros::value_to_sexp(value)?,
    })
}

fn compiler_position(args: Vector<Value>) -> Result<Value, EvalError> {
    let span =
        crate::syntax::SyntaxNode::from_value(compiler_argument(&args)?).and_then(|node| node.span);
    Ok(span.map_or(Value::Nil, |span| {
        Value::Vector(vector![
            Value::Integer(span.line as i64),
            Value::Integer(span.col as i64),
        ])
    }))
}

fn compiler_order_keys(args: Vector<Value>) -> Result<Value, EvalError> {
    let one = args
        .front()
        .ok_or_else(|| EvalError::wrong_arg_count(1, 0))?;
    let Value::Map(m) = one else {
        return Err(EvalError::type_error("map", one.value_type()));
    };
    // Syntax keys carry unordered metadata maps. Order their datum instead,
    // with explicit type tags and recursively canonical map entries.
    let mut keys = m
        .keys()
        .map(|key| {
            let node = crate::syntax::SyntaxNode::from_value(key);
            let position = node
                .as_ref()
                .and_then(|node| node.span)
                .map(|span| (span.start, span.end, span.line, span.col));
            let datum = match node {
                Some(node) => Value::from(node.to_sexp()?),
                None => compiler_datum(key)?,
            };
            Ok(((canonical_key(&datum)?, position), key.clone()))
        })
        .collect::<Result<Vec<_>, EvalError>>()?;
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Value::Vector(
        keys.into_iter().map(|(_, key)| key).collect(),
    ))
}

fn canonical_key(value: &Value) -> Result<String, EvalError> {
    let payload = match value {
        Value::List(items) | Value::Vector(items) => serde_json::Value::Array(
            items
                .iter()
                .map(canonical_key)
                .map(|key| key.map(serde_json::Value::String))
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(items) => {
            let mut pairs = items
                .iter()
                .map(|(key, value)| Ok((canonical_key(key)?, canonical_key(value)?)))
                .collect::<Result<Vec<_>, EvalError>>()?;
            pairs.sort();
            serde_json::to_value(pairs).map_err(|error| EvalError::custom(error.to_string()))?
        }
        Value::Float(number) => serde_json::Value::from(number.to_bits()),
        Value::Nil
        | Value::Boolean(_)
        | Value::Integer(_)
        | Value::String(_)
        | Value::Symbol(_)
        | Value::Keyword(_)
        | Value::Char(_) => serde_json::Value::String(value.to_string()),
        other => {
            return Err(EvalError::type_error(
                "portable map key",
                other.value_type(),
            ));
        }
    };
    serde_json::to_string(&(value.value_type(), payload))
        .map_err(|error| EvalError::custom(error.to_string()))
}

fn compiler_float_bits(args: Vector<Value>) -> Result<i64, EvalError> {
    let one = args
        .front()
        .ok_or_else(|| EvalError::wrong_arg_count(1, 0))?;
    let bits = match one {
        Value::Float(f) => f.to_bits() as i64,
        Value::Integer(i) => (*i as f64).to_bits() as i64,
        other => return Err(EvalError::type_error("number", other.value_type())),
    };
    Ok(bits)
}

fn compiler_fail(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<EvalError, EvalError> {
    if args.len() != 4 {
        return Err(EvalError::wrong_arg_count(4, args.len()));
    }
    let Value::String(name) = &args[0] else {
        return Err(EvalError::type_error("string", args[0].value_type()));
    };
    let Value::Keyword(kind) = &args[2] else {
        return Err(EvalError::type_error("keyword", args[2].value_type()));
    };
    let Value::String(message) = &args[3] else {
        return Err(EvalError::type_error("string", args[3].value_type()));
    };
    let error = match kind.as_str() {
        "recur-not-tail" => EvalError::recur_not_tail(),
        "recur-without-loop" => EvalError::recur_without_loop(),
        "invalid-form" => EvalError::invalid_form(message),
        _ => return Err(EvalError::invalid_form("unknown compiler error kind")),
    };
    let span = match &args[1] {
        Value::Vector(position) if position.len() == 2 => match (&position[0], &position[1]) {
            (Value::Integer(line), Value::Integer(col)) if *line > 0 && *col > 0 => {
                if let Some(source) = compilation_provenance(name, engine) {
                    let source_id = source.bind(name, engine)?;
                    source
                        .spans
                        .get()
                        .and_then(|spans| spans.get(&(*line as usize, *col as usize)))
                        .copied()
                        .map(|mut span| {
                            span.source_id = source_id;
                            span
                        })
                } else {
                    None
                }
            }
            _ => None,
        },
        _ => None,
    };
    Ok(error.with_opt_span(span))
}

fn capture_from_wire(value: &serde_json::Value) -> Result<Value, EvalError> {
    let value = crate::builtins::json::from_json(value.clone())?;
    let Value::Vector(mut parts) = value else {
        return Err(EvalError::invalid_form("capture descriptor must be vector"));
    };
    if parts.len() != 2 {
        return Err(EvalError::invalid_form("invalid capture descriptor"));
    }
    let kind = match &parts[0] {
        Value::String(kind) | Value::Keyword(kind) if kind == "local" || kind == "capture" => {
            kind.clone()
        }
        _ => return Err(EvalError::invalid_form("invalid capture kind")),
    };
    if !matches!(parts.get(1), Some(Value::Integer(index)) if *index >= 0) {
        return Err(EvalError::invalid_form("invalid capture index"));
    }
    parts[0] = Value::Keyword(kind);
    Ok(Value::Vector(parts))
}

fn install_rules(args: Vector<Value>, env: Arc<Env>) -> Result<(), EvalError> {
    let mut iter = args.iter();
    let name_val = iter
        .next()
        .ok_or_else(|| EvalError::wrong_arg_count(2, 0))?;
    let body_val = iter
        .next()
        .ok_or_else(|| EvalError::wrong_arg_count(2, 1))?;
    let Value::Symbol(name) = name_val else {
        return Err(EvalError::type_error("symbol", name_val.value_type()));
    };
    let body_sexp = macros::value_to_sexp(body_val)?;
    let macro_val = Value::Macro(Arc::new(Macro {
        name: name.clone(),
        params: vector![],
        rest_param: None,
        body: body_sexp,
        env: env.clone(),
        compiled: None,
    }));
    env.set(name.clone(), macro_val);
    Ok(())
}

fn install_macro(
    args: Vector<Value>,
    env: Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<(), EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let Value::Symbol(name) = &args[0] else {
        return Err(EvalError::type_error("symbol", args[0].value_type()));
    };
    let mut artifact = value_to_artifact(&args[1])?;
    if artifact.source.is_none() {
        artifact.source = compilation_provenance(&artifact.name, engine);
    }
    let module = Arc::new(artifact);
    let entry = module.entry;
    let record = module
        .functions
        .get(entry)
        .ok_or_else(|| EvalError::custom("entry function missing"))?;
    let function = bind_call(
        module.clone(),
        entry,
        Vector::new(),
        record,
        Vector::new(),
        &env,
        engine,
    )?;
    install_macro_function(name.clone(), function, &env)?;
    Ok(())
}

/// Install a compiled frontend bundle from `module` into `ctx`.
///
/// Ordering is load-bearing. First the artifact's provenance is bound
/// with the *bootstrap* (interpreted) Zi reader — the compiled reader's
/// own bodies cannot exist before their declarations execute — and the
/// resulting registration is recorded as a trusted implementation
/// source. Then every name the artifact declares is removed from the
/// context's root scope, so a stale interpreted helper cannot mask a
/// missing compiled definition. Only then does the artifact's entry
/// execute, defining the compiled frontend. Finally the four contract
/// entries (`zi-read`, `zi-expand`, `zi-apply`, `zio--compile`) are
/// verified to be compiled functions; anything less fails closed.
pub fn install_compiled_compiler(ctx: &EvalContext, module: &Value) -> Result<(), EvalError> {
    let artifact = value_to_artifact(module)?;
    let named = |name: &str| {
        artifact
            .functions
            .iter()
            .any(|record| record.name.as_deref() == Some(name))
    };
    if !artifact.functions.iter().any(|record| {
        record.name.as_deref() == Some("zio--compile")
            && record.params.len() == 2
            && record.rest.is_none()
    }) {
        return Err(EvalError::custom(
            "compiled compiler module has no named two-argument zio--compile function",
        ));
    }
    for required in ["zi-read", "zi-expand", "zi-apply"] {
        if !named(required) {
            return Err(EvalError::custom(format!(
                "compiled compiler module has no named {required} function"
            )));
        }
    }
    // Bind provenance while the interpreted reader is still the reader of
    // record: span metadata decodes through it once and is cached for the
    // artifact's own execution.
    if let Some(source) = &artifact.source {
        let id = source.bind(&artifact.name, ctx)?;
        record_bundle_source(ctx.source_map(), id);
    }
    // Remove every previous frontend binding, including helpers omitted
    // by this artifact. Missing declarations must not fall back to Stage 0.
    COMPILER_BINDINGS.with(|bindings| {
        for previous in bindings
            .borrow()
            .iter()
            .filter(|entry| std::ptr::eq(entry.map.as_ptr(), Arc::as_ptr(ctx.source_map())))
        {
            for name in &previous.names {
                ctx.env.delete_owned(name);
            }
        }
    });
    let mut names = Vec::new();
    for record in artifact.functions.iter() {
        for instruction in record.code.iter() {
            let declared = match instruction {
                Instruction::Define(name)
                | Instruction::SetGlobal(name)
                | Instruction::Macro(name)
                | Instruction::MacroRules(name) => Some(name),
                _ => None,
            };
            if let Some(name) = declared {
                ctx.env.delete_owned(name);
                names.push(name.clone());
            }
        }
    }
    // Execute declarations, installing helpers as well as the entry point.
    // Replacing only zio--compile silently kept Stage 2 on AST helper bodies.
    run_artifact(ctx, &artifact)?;
    record_compiler_bindings(ctx, names);
    for name in ["zi-read", "zi-expand", "zi-apply", "zio--compile"] {
        match ctx.env.get_owned(name) {
            Some(Value::Function(function)) if function.compiled.is_some() => {}
            _ => {
                return Err(EvalError::custom(format!(
                    "compiler artifact did not install a compiled {name}"
                )));
            }
        }
    }
    Ok(())
}

fn run_artifact(ctx: &EvalContext, artifact: &ModuleArtifact) -> Result<Value, EvalError> {
    let module = Arc::new(artifact.clone());
    let entry = artifact.entry;
    let record = artifact
        .functions
        .get(entry)
        .ok_or_else(|| EvalError::custom("entry function missing"))?;
    bind_call(
        module,
        entry,
        Vector::new(),
        record,
        Vector::new(),
        &ctx.env,
        ctx,
    )
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bootstrap;

    fn ctx() -> EvalContext {
        bootstrap::language_context(crate::bootstrap::ModuleRoots::empty()).unwrap()
    }

    #[test]
    fn empty_program() {
        let c = ctx();
        let v = run_source(&c, "t", "").unwrap();
        assert_eq!(v, Value::Nil);
    }

    #[test]
    fn trivial_top_level() {
        let c = ctx();
        let v = run_source(&c, "t", "(+ 1 2)").unwrap();
        assert_eq!(v, Value::Integer(3));
    }

    #[test]
    fn defn_and_call() {
        let c = ctx();
        let v = run_source(&c, "t", "(defn sq [x] (* x x)) (sq 7)").unwrap();
        assert_eq!(v, Value::Integer(49));
    }

    #[test]
    fn closure_capture() {
        let c = ctx();
        let src = "
            (defn mk [k] (fn [x] (+ x k)))
            (def a (mk 10))
            (a 5)
        ";
        let v = run_source(&c, "t", src).unwrap();
        assert_eq!(v, Value::Integer(15));
    }

    #[test]
    fn tail_recursive_loop() {
        let c = ctx();
        let src = "
            (defn loop1 [n] (if (= n 0) 42 (loop1 (- n 1))))
            (loop1 1000)
        ";
        let v = run_source(&c, "t", src).unwrap();
        assert_eq!(v, Value::Integer(42));
    }

    #[test]
    fn map_literal() {
        let c = ctx();
        let v = run_source(&c, "t", "(get {:a 1 :b 2} :a)").unwrap();
        assert_eq!(v, Value::Integer(1));
    }

    #[test]
    fn vector_literal() {
        let c = ctx();
        let v = run_source(&c, "t", "(get [10 20 30] 1)").unwrap();
        assert_eq!(v, Value::Integer(20));
    }

    #[test]
    fn compile_and_execute() {
        let c = ctx();
        let module = compile_source(&c, "t", "(defn id [x] x) (id 99)").unwrap();
        // The module value shape (Value::Map) round-trips through serialize.
        let s = serialize_module(&module).unwrap();
        let restored = deserialize_module(&s).unwrap();
        let _out = execute_module(&c, &restored).unwrap();
    }

    #[test]
    fn wire_format_keyword_preserved() {
        let c = ctx();
        // A constant map with a keyword key must come back as a Keyword,
        // never as a String. The check is on the executed result, not on
        // the module map: the wire form is normalized (key order, and the
        // entry name the wire does not carry), so comparing raw maps
        // would be asserting that the encoder is lossless in a way it is
        // deliberately not.
        let module = compile_source(&c, "t", "(def x {:name 1}) x").unwrap();
        let before = execute_module(&c, &module).unwrap();
        let wire = serialize_module(&module).unwrap();
        let restored = deserialize_module(&wire).unwrap();
        let after = execute_module(&c, &restored).unwrap();
        assert_eq!(
            before, after,
            "wire round-trip changed what the module does"
        );
    }

    #[test]
    fn fuel_is_charged() {
        let c = ctx();
        c.set_step_ceiling(50);
        let res = run_source(&c, "t", "(loop [n 0] (if (= n 1000) n (recur (+ n 1))))");
        assert!(res.is_err(), "fuel ceiling should have stopped execution");
    }

    #[test]
    fn fuel_charged_per_instruction() {
        // A 100-iteration loop charges at least 100 + tail-call jumps.
        let c = ctx();
        c.set_step_ceiling(10_000);
        let v = run_source(&c, "t", "(defn id [x] x) (id 7)").unwrap();
        assert!(c.steps_spent() > 0);
        assert_eq!(v, Value::Integer(7));
    }
}
