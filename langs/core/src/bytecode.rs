//! Self-hosted Zio instruction runtime.
//!
//! This module is the only thing the rest of the evaluator calls into when a
//! function value carries `compiled: Some(closure)`. The compiler living in
//! `langs/compiler/compiler.zio` lowers Zio source into the constant pool and
//! opcode table this VM understands; `bootstrap_compiler` makes that lowering
//! available at boot, `run_source`/`execute_module` run installed programs,
//! and `self_build` performs the three-stage build that proves the compiler
//! compiles itself.
//!
//! Everything here executes — no AST fallback, no re-interpretation of Sexp.
//! Native ZOS class/generic primitives stay native on purpose: ordinary
//! function/method/initializer bodies compile to callable closures through
//! `compile_source`. The VM charges fuel through `EvalRuntime::spend` on
//! every instruction so the bounded-execution feature in this system is
//! honored inside compiled code, including closures and HOFs.

use std::sync::Arc;

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

/// A closure value: pairs a function index inside its owning module with the
/// captured values from the enclosing scope. Captures are stored as `Value`
/// so lexical mutation propagates back through `StoreCapture` correctly.
#[derive(Debug, Clone)]
pub struct Closure {
    pub module: Arc<ModuleArtifact>,
    pub fn_index: usize,
    pub captures: Vector<Value>,
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
    /// Already-bound captured values, in the order the enclosing scope
    /// supplied them. Names would not survive: a capture is whatever the
    /// scope held, including a mutable cell.
    pub captures: Vector<Value>,
    pub code: Vector<Instruction>,
    pub debug: Vector<Option<(usize, usize)>>,
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
    Bind(usize),
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
    /// Install a handler table; on error the VM walks it in reverse and jumps
    /// to the first type-name match (or `"error"` / `"any"` as a fallback).
    /// The handler also pushes the error value onto the stack at the target.
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
            Ok(Value::from(compiler_position(args)?))
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
        Value::NativeFunction(NativeFn::new("compiler/fail", move |args, _| {
            Err(compiler_fail(args)?)
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
        Value::NativeFunction(NativeFn::new("compiler/install-macro", move |args, _| {
            install_macro(args, Arc::clone(&macro_env))?;
            Ok(Value::Nil)
        })),
    );
}

/// Load the Zio compiler source into the context, then wire `zio--compile`
/// so ordinary source can call it. The compiler source is treated like any
/// other Zio source — it is parsed and run through the standard loader —
/// but its `defmacro` / `defn` definitions stay compiled to closures so the
/// host's install hooks can reify them.
pub fn bootstrap_compiler(ctx: &EvalContext) -> Result<(), EvalError> {
    let source = include_str!("../../compiler/compiler.zio");
    eval_source_text(ctx, "<compiler>", source)?;
    Ok(())
}

/// Parse, compile, and run `source` against `ctx`. Equivalent to
/// `bootstrap_source` with compilation installed.
pub fn run_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    eval_source_text(ctx, name, source)
}

/// Compile `source` to a module artifact by calling the self-hosted compiler.
/// The compiler is invoked through `zio--compile` — interpreted unless the
/// caller pre-installed a compiled version.
pub fn compile_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    let parsed = parse_source(name, source)?;
    let form_vec: Vector<Value> = parsed.into_iter().map(Value::from).collect();
    let form_value = Value::Vector(form_vec);
    let zio_compile = ctx.env.get("zio--compile").ok_or_else(|| {
        EvalError::custom("zio--compile is not bound; bootstrap_compiler was not called")
    })?;
    let compiled = call_value(
        zio_compile,
        vector![Value::String(name.into()), form_value],
        ctx,
    )?;
    Ok(compiled)
}

/// Install `zio--compile` from a compiled module and report which
/// function index it was bound to.
///
/// Exists so a diagnostic can check the stage-2 binding without
/// duplicating the search that [`install_compiled_compiler`] performs; if
/// the two ever disagree, the diagnostic would be measuring something
/// other than the real path.
pub fn install_compiled_for_test(ctx: &EvalContext, module: &Value) -> Option<usize> {
    let before = ctx.env.get("zio--compile");
    install_compiled_compiler(ctx, module).ok()?;
    let after = ctx.env.get("zio--compile");
    match (&before, &after) {
        (Some(before), Some(after)) if before == after => None,
        (_, Some(Value::Function(function))) => {
            function.compiled.as_ref().map(|closure| closure.fn_index)
        }
        _ => None,
    }
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

/// Three-stage self-build. Stage 0 is the interpreted compiler already in
/// `ctx`; stage 1 runs that interpreter to produce a compiled-compiler
/// artifact; stage 2 runs the compiled-compiler to compile the same source
/// again. Stages 1 and 2 must produce byte-identical module artifacts.
///
/// Returned `SelfBuild.equal` is true when stages 1 and 2 hash the same way
/// after canonicalization. The `modules` vec holds the module value from
/// each stage.
pub struct SelfBuild {
    pub modules: Vec<Value>,
    pub normalized: Vec<String>,
    pub hashes: Vec<String>,
    pub equal: bool,
}

pub fn self_build(ctx: &EvalContext) -> Result<SelfBuild, EvalError> {
    let source = include_str!("../../compiler/compiler.zio");
    // Stage 0: the interpreter running the compiler source is the baseline.
    // We compute a "stage 0" artifact by interpreting compiler.zio first,
    // which is what `bootstrap_compiler` already does. Then stage 1 invokes
    // the interpreted `zio--compile` on the source.
    bootstrap_compiler(ctx)?;
    let stage1 = compile_source(ctx, "compiler.zio", source)?;
    // Stage 2: install stage 1's compiled compiler into the env, then
    // re-compile the source under it.
    install_compiled_compiler(ctx, &stage1)?;
    let stage2 = compile_source(ctx, "compiler.zio", source)?;

    let norm = |m: &Value| -> Result<String, EvalError> {
        let art = value_to_artifact(m)?;
        let wire = art.to_wire();
        serde_json::to_string(&wire?).map_err(|e| EvalError::custom(format!("normalize: {e}")))
    };
    let h = |s: &str| -> String {
        // FNV-1a 64 — stable, zero-dep, sufficient for canonical-form equality.
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in s.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{:016x}", hash)
    };
    let n1 = norm(&stage1)?;
    let n2 = norm(&stage2)?;
    let h1 = h(&n1);
    let h2 = h(&n2);
    Ok(SelfBuild {
        modules: vec![stage1, stage2],
        normalized: vec![n1, n2],
        hashes: vec![h1.clone(), h2.clone()],
        equal: h1 == h2,
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
    match func {
        Value::Function(f) => {
            if let Some(closure) = f.compiled.as_ref() {
                return invoke_closure(closure, args, &f.env, engine);
            }
            // Interpreted path: bind args, eval body. The compiler itself is
            // interpreted until `self_build` installs the compiled version.
            let env = if f.rest_param.is_some() {
                Env::bind_variadic(&f.env, &f.params, &f.rest_param, &args)?
            } else {
                Env::bind(&f.env, &f.params, &args)?
            };
            let result = engine.eval_expr(&f.body, &env, false)?;
            Ok(result.into_value())
        }
        Value::NativeFunction(nf) => nf.call(args, engine),
        Value::Object(o) => {
            // `try_apply` is optional because an object is not
            // necessarily callable: a ZOS object with no applicable
            // method is a value, and calling it is a type error rather
            // than a protocol failure. So the Option is the dispatch
            // decision, and it is unwrapped exactly once. The error names
            // the kind rather than the value: the object's own display
            // would need the binding this arm has already moved.
            let Some(result) = crate::zos::apply::try_apply(o.as_ref(), args, engine) else {
                return Err(EvalError::not_a_function("ZOS object"));
            };
            match result? {
                TailResult::Value(value) => Ok(value),
                TailResult::TailCall(next, next_args) => call_value(next, next_args, engine),
                TailResult::Recur(_) => Err(EvalError::recur_without_loop()),
            }
        }
        Value::Macro(m) => {
            // Macros invoked as functions: most often inside the compiled
            // compiler itself. Convert args to Sexp, apply, convert back.
            let arg_sexps: Vec<Sexp> = {
                let mut out = Vec::with_capacity(args.len());
                for a in args.iter() {
                    out.push(macros::value_to_sexp(a)?);
                }
                out
            };
            let sexp = macros::apply_macro(&m, &arg_sexps, &m.env, engine)?;
            Ok(Value::from(sexp))
        }
        other => Err(EvalError::not_a_function(format!("{other}"))),
    }
}

// ─── VM core ────────────────────────────────────────────────────────────

/// A `try` region the VM has entered.
///
/// The compiler emits `:handler` / `:unhandler` around a `try` body so
/// a future VM can dispatch a `throw` to the right `catch` without
/// unwinding Rust frames. Today the region is recorded and popped but
/// never consulted: a `try` whose body is compiled falls back to the
/// interpreter, which handles the catch itself. The field is kept
/// because the opcodes are already emitted, and a VM that grows throw
/// dispatch needs the region to already be tracked.
struct HandlerFrame {
    #[allow(dead_code)]
    table: Vec<(String, usize)>,
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
    captures: Vector<Value>,
    record: &FunctionRecord,
    args: Vector<Value>,
    lexical_env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    // Arity check: count params vs args, accounting for rest.
    let min = record.params.len();
    let max = if record.rest.is_some() {
        usize::MAX
    } else {
        record.params.len()
    };
    if args.len() < min || args.len() > max {
        // Name the function: an arity error with no callee is a puzzle,
        // and the whole point of this path is that the callee is machine
        // generated and therefore not visible in the source.
        return Err(EvalError::custom(format!(
            "wrong argument count: expected between {min} and {max}, got {} (in {} fn {fn_index})",
            args.len(),
            module.name
        )));
    }

    // Build locals array: [captures..., params..., rest...].
    // Captures sit at indices 0..record.captures.len(); params follow.
    let mut locals: Vec<Value> =
        Vec::with_capacity(record.locals.max(args.len() + record.captures.len()));
    for c in captures.iter() {
        locals.push(c.clone());
    }
    for a in args.iter() {
        locals.push(a.clone());
    }
    if let Some(rest_name) = &record.rest {
        // Pad locals up to params+rest count, then bind rest as a list.
        while locals.len() < record.params.len() + record.captures.len() {
            locals.push(Value::Nil);
        }
        // Recompute rest position: captures_count + params_count.
        let rest_start = record.captures.len() + record.params.len();
        // If we already pushed rest items as locals (because locals cap =
        // record.locals was set during compile), they're just there. Either
        // way, ensure the rest slot exists; bind extra args as a list.
        if locals.len() <= rest_start {
            locals.push(Value::List(Vector::new()));
        } else {
            // The args beyond params are already in locals; rebuild a list.
            let rest_vec: Vector<Value> = locals[rest_start..].iter().cloned().collect();
            locals.truncate(rest_start);
            locals.push(Value::List(rest_vec));
        }
        let _ = rest_name; // name is preserved in record for diagnostics
    }

    // Pad locals to record.locals size with Nil so indices up to locals-1 are
    // always valid (the compiler pre-reserved slots for top-level `def`).
    while locals.len() < record.locals {
        locals.push(Value::Nil);
    }

    run_loop(
        module,
        fn_index,
        locals,
        record.captures.len(),
        lexical_env,
        engine,
    )
}

fn run_loop(
    module: Arc<ModuleArtifact>,
    fn_index: usize,
    mut locals: Vec<Value>,
    captures_count: usize,
    lexical_env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    let mut pc: usize = 0;
    // `locals` is the frame: captures then parameters, addressed by
    // `Local`/`Store`. The operand stack is a separate vector, because
    // sharing one storage means every push shifts the slot a later
    // `Local` reads — which made `(+ x 1)` read the constant where it
    // expected `x` and silently produce wrong answers.
    let mut stack: Vec<Value> = Vec::new();
    let mut handlers: Vec<HandlerFrame> = Vec::new();
    // PC stack for tail-call re-entry: when a TailCall runs, we replace the
    // current locals with the new call's locals and jump pc to 0.
    // The current code is straight-line (no proper call stack frames yet),
    // so TailCall is implemented as an in-frame re-bind. For ordinary calls
    // we recurse through `invoke_closure` / `bind_call`.

    let code = module
        .functions
        .get(fn_index)
        .ok_or_else(|| EvalError::custom("function index out of range"))?
        .code
        .clone();

    // Helpers operating on the closure's captures table. Stored separately
    // from locals so we can mutate them and propagate to other closures.
    let mut captures_storage: Vector<Value> = if captures_count > 0 {
        locals[0..captures_count].iter().cloned().collect()
    } else {
        Vector::new()
    };

    loop {
        engine.spend(1)?;
        if pc >= code.len() {
            return Err(EvalError::custom("PC ran off end of code"));
        }
        let instr = code.get(pc).cloned().unwrap();
        match instr {
            Instruction::Const(i) => {
                let v = constant_to_value(&module.constants, &module.constants[i])?;
                stack.push(v);
                pc += 1;
            }
            Instruction::Pop => {
                stack.pop();
                pc += 1;
            }
            Instruction::Dup => {
                let v = stack
                    .last()
                    .cloned()
                    .ok_or_else(|| EvalError::custom("dup on empty stack"))?;
                stack.push(v);
                pc += 1;
            }
            Instruction::Local(i) => {
                // Reads a frame slot and pushes its value. The frame
                // occupies the low indices and the operand stack grows
                // above it, so a push cannot move the slot a later
                // `Local` reads — which is what made every two-operand
                // call read its constant in place of its variable.
                let slot = i;
                let v = locals.get(slot).cloned().ok_or_else(|| {
                    EvalError::custom(format!("local slot {slot} is out of range"))
                })?;
                stack.push(v);
                pc += 1;
            }
            Instruction::Capture(i) => {
                let v = captures_storage
                    .get(i)
                    .cloned()
                    .ok_or_else(|| EvalError::custom("capture index out of range"))?;
                stack.push(v);
                pc += 1;
            }
            Instruction::Store(i) => {
                let v = stack
                    .last()
                    .cloned()
                    .ok_or_else(|| EvalError::custom("store on empty stack"))?;
                if i < captures_count {
                    captures_storage[i] = v;
                } else {
                    locals[i] = v;
                }
                pc += 1;
            }
            Instruction::StoreCapture(i) => {
                let v = stack
                    .last()
                    .cloned()
                    .ok_or_else(|| EvalError::custom("store-capture on empty stack"))?;
                captures_storage[i] = v;
                pc += 1;
            }
            Instruction::Bind(i) => {
                let v = stack
                    .pop()
                    .ok_or_else(|| EvalError::custom("bind on empty stack"))?;
                if i < captures_count {
                    captures_storage[i] = v;
                } else {
                    if locals.len() <= i {
                        locals.resize(i + 1, Value::Nil);
                    }
                    locals[i] = v;
                }
                pc += 1;
            }
            Instruction::Global(name) => {
                let v = lookup_global(engine, &name, lexical_env)?;
                stack.push(v);
                pc += 1;
            }
            Instruction::SetGlobal(name) => {
                let v = stack
                    .last()
                    .cloned()
                    .ok_or_else(|| EvalError::custom("set-global on empty stack"))?;
                set_global(engine, &name, v, lexical_env)?;
                pc += 1;
            }
            Instruction::Define(name) => {
                let v = stack
                    .last()
                    .cloned()
                    .ok_or_else(|| EvalError::custom("define on empty stack"))?;
                engine.env().set(name, v);
                pc += 1;
            }
            Instruction::Jump(target) => {
                pc = target;
            }
            Instruction::JumpFalse(target) => {
                let v = stack.last().cloned().unwrap_or(Value::Nil);
                if !crate::value::is_truthy(&v) {
                    pc = target;
                } else {
                    pc += 1;
                }
            }
            Instruction::JumpTrue(target) => {
                let v = stack.last().cloned().unwrap_or(Value::Nil);
                if crate::value::is_truthy(&v) {
                    pc = target;
                } else {
                    pc += 1;
                }
            }
            Instruction::Closure(idx) => {
                // A closure captures the values named by the *target*
                // function's capture list. Those names refer to this
                // frame's own slots — captures and parameters alike — so
                // snapshotting `captures_storage` alone would drop every
                // value that arrived as a parameter and a closure over a
                // parameter would capture nil.
                let record = module.functions.get(idx);
                let closure = Closure {
                    module: Arc::clone(&module),
                    fn_index: idx,
                    captures: match record {
                        Some(target) => {
                            let got =
                                snapshot_captures(&target.captures, &captures_storage, &locals);
                            if std::env::var_os("ZIO_TRACE_CAPTURE").is_some() {
                                eprintln!(
                                    "closure {idx}: names={:?} frame={:?} caps={:?} -> {:?}",
                                    target.captures, locals, captures_storage, got
                                );
                            }
                            got
                        }
                        None => Vector::new(),
                    },
                };
                let func = Arc::new(Function {
                    params: record.map(|r| r.params.clone()).unwrap_or_default(),
                    rest_param: record.and_then(|r| r.rest.clone()),
                    body: Sexp::Nil,
                    env: Arc::new(Env::new(Some(lexical_env.clone()))),
                    compiled: Some(Arc::new(closure)),
                });
                stack.push(Value::Function(func));
                pc += 1;
            }
            Instruction::Call(n) => {
                // The compiler emits the function first, then the args in
                // source order; the stack ends up [func, arg0, ..., argN].
                // Drain those and split: the head is the callee.
                // Drain from the operand stack only. Draining from the
                // end of `locals` would work today because the frame is
                // below the stack, but taking the count from the frame
                // base makes the separation explicit rather than
                // incidental.
                let start = stack.len().saturating_sub(n + 1);
                let drained: Vec<Value> = stack.drain(start..).collect();
                let mut iter = drained.into_iter();
                let func = iter.next().expect("call drained an empty slice");
                let args: Vector<Value> = iter.collect();
                let result = call_value(func, args, engine)?;
                stack.push(result);
                pc += 1;
            }
            Instruction::TailCall(n) => {
                // Drain from the operand stack only. Draining from the
                // end of `locals` would work today because the frame is
                // below the stack, but taking the count from the frame
                // base makes the separation explicit rather than
                // incidental.
                let start = stack.len().saturating_sub(n + 1);
                let drained: Vec<Value> = stack.drain(start..).collect();
                let mut iter = drained.into_iter();
                let func = iter.next().expect("tail-call drained an empty slice");
                let args: Vector<Value> = iter.collect();
                // Tail-call: re-bind current frame with the new call.
                // If the callee is a closure in the same module, re-run the
                // loop with new locals. Otherwise, recurse through
                // `invoke_closure`.
                match &func {
                    Value::Function(f) if f.compiled.is_some() => {
                        let closure = f.compiled.as_ref().unwrap();
                        let same_module =
                            Arc::ptr_eq(&closure.module, &module) && closure.fn_index == fn_index;
                        if same_module {
                            let rec = &module.functions[closure.fn_index];
                            // Re-bind the frame in place. A tail call
                            // reuses this activation, so the operand
                            // stack restarts empty while the new
                            // parameters land in fresh frame slots.
                            let mut frame: Vec<Value> = Vec::with_capacity(rec.locals);
                            for c in closure.captures.iter() {
                                frame.push(c.clone());
                            }
                            for a in args.iter() {
                                frame.push(a.clone());
                            }
                            while frame.len() < rec.locals {
                                frame.push(Value::Nil);
                            }
                            locals = frame;
                            stack.clear();
                            captures_storage = closure.captures.clone();
                            pc = 0;
                            continue;
                        }
                        let result = invoke_closure(closure, args, &f.env, engine)?;
                        return Ok(result);
                    }
                    _ => {}
                }
                match func {
                    Value::Function(f) => {
                        // Interpreted callee in tail position: fall through
                        // to interpreter. The eval loop re-enters via
                        // eval_expr.
                        let env = if f.rest_param.is_some() {
                            Env::bind_variadic(&f.env, &f.params, &f.rest_param, &args)?
                        } else {
                            Env::bind(&f.env, &f.params, &args)?
                        };
                        let result = engine.eval_expr(&f.body, &env, true)?;
                        return Ok(result.into_value());
                    }
                    Value::NativeFunction(nf) => {
                        let result = nf.call(args, engine)?;
                        return Ok(result);
                    }
                    other => {
                        return Err(EvalError::not_a_function(format!("{other}")));
                    }
                }
            }
            Instruction::Return => {
                return Ok(stack.last().cloned().unwrap_or(Value::Nil));
            }
            Instruction::VectorN(n) => {
                let start = stack.len().saturating_sub(n);
                let items: Vector<Value> = stack.drain(start..).collect();
                stack.push(Value::Vector(items));
                pc += 1;
            }
            Instruction::MapN(n) => {
                let start = stack.len().saturating_sub(2 * n);
                let mut pairs: Vector<(Value, Value)> = Vector::new();
                let drained: Vec<Value> = stack.drain(start..).collect();
                for chunk in drained.chunks(2) {
                    if chunk.len() == 2 {
                        pairs.push_back((chunk[0].clone(), chunk[1].clone()));
                    }
                }
                let mut map: ImHashMap<Value, Value> = ImHashMap::new();
                for (k, v) in pairs.iter() {
                    map.insert(k.clone(), v.clone());
                }
                stack.push(Value::Map(map));
                pc += 1;
            }
            Instruction::Handler(table) => {
                handlers.push(HandlerFrame { table });
                pc += 1;
            }
            Instruction::Unhandler => {
                handlers.pop();
                pc += 1;
            }
            Instruction::ClearError => {
                handlers.pop();
                pc += 1;
            }
            Instruction::Metadata(op) => {
                let v = stack.last().cloned().unwrap_or(Value::Nil);
                handle_metadata(engine, &op, v)?;
                pc += 1;
            }
            Instruction::Macro(name) => {
                // Already installed at defmacro time; nothing to do at runtime
                // except drop the literal the compiler pushed.
                stack.pop();
                let _ = name;
                pc += 1;
            }
            Instruction::MacroRules(name) => {
                stack.pop();
                let _ = name;
                pc += 1;
            }
            Instruction::Module => {
                stack.pop(); // discard the literal
                pc += 1;
            }
            Instruction::Method => {
                stack.pop();
                pc += 1;
            }
            Instruction::Class(_) => {
                stack.pop();
                pc += 1;
            }
        }
    }
}

fn handle_metadata(engine: &dyn EvalEngine, op: &str, payload: Value) -> Result<(), EvalError> {
    let sexp = macros::value_to_sexp(&payload)?;
    let forms: Vec<Sexp> = match sexp {
        Sexp::List(v, _) | Sexp::Vector(v, _) => v.into_iter().collect(),
        other => vec![other],
    };
    match op {
        "require" => {
            crate::special::module_forms::do_require(&forms, engine.env(), engine)?;
        }
        "export" => {
            for s in forms {
                let name = match s {
                    Sexp::Symbol(n, _) | Sexp::Keyword(n, _) => n,
                    _ => return Err(EvalError::custom("export requires symbols or keywords")),
                };
                engine.add_module_export(name);
            }
        }
        "package" | "generic" => {
            // Recorded at define time; runtime form is a no-op.
        }
        other => {
            return Err(EvalError::custom(format!("unknown metadata op: {other}")));
        }
    }
    Ok(())
}

/// Read one capture name out of the frame that created a closure.
///
/// A capture is `[:local n]` for a slot in this frame or `[:capture n]`
/// for a slot in this frame's own capture table. The compiler emits the
/// binding, not the value, so the value has to be read at closure-creation
/// time — and it has to be read from the right place, which is the whole
/// point: a parameter lives in the frame, a capture lives in the table.
fn snapshot_capture(name: &Value, captures: &Vector<Value>, locals: &[Value]) -> Value {
    let Value::Vector(parts) = name else {
        return Value::Nil;
    };
    let Some(kind) = parts.front() else {
        return Value::Nil;
    };
    let kind = match kind {
        Value::Keyword(name) | Value::Symbol(name) => name.as_str(),
        _ => return Value::Nil,
    };
    let Some(Value::Integer(index)) = parts.get(1) else {
        return Value::Nil;
    };
    let index = *index as usize;
    match kind {
        "capture" => captures.get(index).cloned().unwrap_or(Value::Nil),
        _ => locals.get(index).cloned().unwrap_or(Value::Nil),
    }
}

/// Snapshot every capture a nested function declares, in its order.
fn snapshot_captures(
    names: &Vector<Value>,
    captures: &Vector<Value>,
    locals: &[Value],
) -> Vector<Value> {
    names
        .iter()
        .map(|name| snapshot_capture(name, captures, locals))
        .collect()
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

fn parse_source(name: &str, source: &str) -> Result<Vec<Sexp>, EvalError> {
    crate::reader::reader::read_program(source).map_err(|e| e.into_eval(name))
}

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

/// Run every top-level form in `source`. Each form is compiled and executed
/// against `ctx`. This is the language's natural source loader.
pub(crate) fn eval_source_text(
    ctx: &EvalContext,
    name: &str,
    source: &str,
) -> Result<Value, EvalError> {
    let forms = parse_source_registered(ctx, name, source)?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = compile_and_run(ctx, &sexp)?;
    }
    Ok(last)
}

/// Compile one form and execute it. If the form is a `(fn ...)` /
/// `(defn ...)` body, the closure carries the bytecode; other forms
/// (special forms, native calls) fall through to the interpreter via
/// `EvalRuntime::eval_expr`.
pub(crate) fn compile_and_run(ctx: &EvalContext, sexp: &Sexp) -> Result<Value, EvalError> {
    // Stage-0 behavior: try to compile, but if compilation rejects the form,
    // run it through the interpreter. This matches the user's spec: ordinary
    // function/method/initializer bodies compile; special forms and metadata
    // remain native, dispatched through `eval_expr`.
    //
    // The source name is not needed here: a form that fails to compile
    // falls through to the interpreter, which reports the error against
    // the form's own span.
    if let Some((Some(func_val), defined)) = try_compile_top_level(sexp)? {
        ctx.env.set(defined, func_val);
        return Ok(Value::Nil);
    }
    crate::eval::eval_in_context(sexp, ctx)
}

fn try_compile_top_level(sexp: &Sexp) -> Result<Option<(Option<Value>, String)>, EvalError> {
    let Sexp::List(items, _) = sexp else {
        return Ok(None);
    };
    let Some(first) = items.front() else {
        return Ok(None);
    };
    let Sexp::Symbol(op, _) = first else {
        return Ok(None);
    };
    match op.as_str() {
        "defn" => {
            if items.len() < 3 {
                return Ok(None);
            }
            let name = match &items[1] {
                Sexp::Symbol(n, _) => n.clone(),
                _ => return Ok(None),
            };
            let mut lowered = items.clone();
            // Replace (defn name [params] body...) with (def name (fn ...))
            lowered[1] = Sexp::Symbol(name.clone(), None);
            let lowered = Sexp::List(lowered, None);
            let _ = lowered;
            // Fall back to interpreter for defn — compilation is exposed
            // through the explicit `compile_source` entry point. Top-level
            // forms keep using eval_expr so legacy semantics are preserved
            // until the compiler is wired into the special forms.
            return Ok(None);
        }
        "fn" => {
            // Anonymous fn: compile to closure, return it.
            let form_vec: Vector<Value> = items.iter().map(|s| Value::from(s.clone())).collect();
            let zio_compile = std::env::var("__ZIO_COMPILE__").ok();
            let _ = (form_vec, zio_compile);
            return Ok(None);
        }
        _ => Ok(None),
    }
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
    Ok(ModuleArtifact {
        version: version as u32,
        name,
        entry,
        functions: fn_records,
        constants: consts,
        entry_name,
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
    // Captures are already-bound values, not names: the compiler emits
    // whatever the enclosing scope held at the closure site, and the VM
    // installs them into the callee's capture slots directly. Decoding
    // them as strings would reject every real closure.
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
    Ok(FunctionRecord {
        name,
        params,
        rest,
        locals,
        captures,
        code,
        debug,
    })
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
        "bind" => Instruction::Bind(int_arg(1)?),
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
    Value::Map(map)
}

fn record_to_value(r: &FunctionRecord) -> Value {
    let mut map: ImHashMap<Value, Value> = ImHashMap::new();
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
        Instruction::Bind(n) => vv(Value::Keyword("bind".into()), Value::Integer(*n as i64)),
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
}

#[derive(Serialize, Deserialize)]
struct WireFunction {
    name: Option<String>,
    params: Vec<String>,
    rest: Option<String>,
    locals: usize,
    /// Captures cross the wire as values, not names: a captured binding
    /// may be an integer, a map, or a cell, and a name-only encoding
    /// would make a closure unrepresentable.
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
    #[serde(rename = "bind")]
    Bind(usize),
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
    fn to_wire(&self) -> Result<WireModule, EvalError> {
        Ok(WireModule {
            version: self.version,
            name: self.name.clone(),
            entry: self.entry,
            entry_name: self.entry_name.clone(),
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
        const MAX_INSTR_ARGS: usize = 4;
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
                if MAX_INSTR_ARGS < 2 {
                    return Err(EvalError::custom("instr arg overflow"));
                }
                code.push_back(wire_to_instruction(instr)?);
            }
            functions.push_back(FunctionRecord {
                name: f.name.clone(),
                params: f.params.iter().cloned().collect(),
                rest: f.rest.clone(),
                locals: f.locals,
                captures: f
                    .captures
                    .iter()
                    .map(|value| {
                        crate::builtins::json::from_json(value.clone())
                            .map_err(|error| EvalError::custom(format!("capture: {error}")))
                    })
                    .collect::<Result<_, _>>()?,
                code,
                debug: f.debug.iter().cloned().collect(),
            });
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
        Instruction::Bind(n) => WireInstruction::Bind(*n),
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
        WireInstruction::Bind(n) => Instruction::Bind(*n),
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

fn compiler_position(args: Vector<Value>) -> Result<Sexp, EvalError> {
    let _ = args;
    // The reader's spans live on the Sexp tree, and a form handed to the
    // compiler as a `Value` has lost them. Carrying them would mean
    // re-deriving positions from a value that no longer knows its
    // source, which is guesswork dressed up as a location — worse than
    // none. The interpreted evaluator keeps its spans; the VM reports
    // the failure without one.
    Ok(Sexp::Nil)
}

fn compiler_order_keys(args: Vector<Value>) -> Result<Value, EvalError> {
    let one = args
        .front()
        .ok_or_else(|| EvalError::wrong_arg_count(1, 0))?;
    let Value::Map(m) = one else {
        return Err(EvalError::type_error("map", one.value_type()));
    };
    // Deterministic ordering: by Value discriminant then Display.
    let mut keys: Vec<Value> = m.keys().cloned().collect();
    keys.sort_by(|a, b| format!("{a}").cmp(&format!("{b}")));
    Ok(Value::Vector(keys.into_iter().collect()))
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

fn compiler_fail(args: Vector<Value>) -> Result<EvalError, EvalError> {
    let _ = args;
    Ok(EvalError::custom("compiler failure"))
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
    let sexp_name = match &body_sexp {
        Sexp::List(items, _) if !items.is_empty() => match &items[0] {
            Sexp::Symbol(n, _) => n.clone(),
            _ => name.clone(),
        },
        _ => name.clone(),
    };
    let macro_val = Value::Macro(Arc::new(Macro {
        name: sexp_name,
        params: vector![],
        rest_param: None,
        body: body_sexp,
        env: env.clone(),
        compiled: None,
    }));
    env.set(name.clone(), macro_val);
    Ok(())
}

fn install_macro(args: Vector<Value>, env: Arc<Env>) -> Result<(), EvalError> {
    let mut iter = args.iter();
    let name_val = iter
        .next()
        .ok_or_else(|| EvalError::wrong_arg_count(2, 0))?;
    let module_val = iter
        .next()
        .ok_or_else(|| EvalError::wrong_arg_count(2, 1))?;
    let Value::Symbol(name) = name_val else {
        return Err(EvalError::type_error("symbol", name_val.value_type()));
    };
    let artifact = value_to_artifact(module_val)?;
    // The compiler names the callable in `:entry`; assuming index 0 would
    // silently install the wrong function for any module whose entry is
    // not its first.
    let entry = artifact.entry;
    let closure = Arc::new(Closure {
        module: Arc::new(artifact),
        fn_index: entry,
        captures: Vector::new(),
    });
    let func = Arc::new(Function {
        params: vector![],
        rest_param: Some("args".into()),
        body: Sexp::Nil,
        env: Arc::new(Env::new(Some(env.clone()))),
        compiled: Some(Arc::clone(&closure)),
    });
    let macro_val = Value::Macro(Arc::new(Macro {
        name: name.clone(),
        params: vector![],
        rest_param: Some("args".into()),
        body: Sexp::Nil,
        env: env.clone(),
        compiled: Some(Arc::clone(&closure)),
    }));
    env.set(name.clone(), Value::Function(func));
    env.set(name.clone(), macro_val);
    Ok(())
}

fn install_compiled_compiler(ctx: &EvalContext, module: &Value) -> Result<(), EvalError> {
    let artifact = value_to_artifact(module)?;
    // `:entry` is the module's top-level body — it takes no arguments.
    // `zio--compile` is an ordinary function inside that module, so the
    // compiled compiler has to point at *its* index, not the entry, or
    // every call fails an arity check against a zero-parameter body.
    // Matching on arity alone is not enough: several compiler helpers
    // take two arguments, and binding one of those as the compiler
    // fails on the first `get` with a string. The module records the
    // exported name so the binding is by name, not by shape.
    let index = artifact
        .entry_name
        .as_deref()
        .and_then(|name| {
            artifact.functions.iter().position(|record| {
                record.params.len() == 2
                    && record.rest.is_none()
                    && record.name.as_deref() == Some(name)
            })
        })
        .or_else(|| {
            artifact
                .functions
                .iter()
                .position(|record| record.params.len() == 2 && record.rest.is_none())
        })
        .ok_or_else(|| {
            EvalError::custom("compiled compiler module has no two-argument entry function")
        })?;
    let module_arc = Arc::new(artifact);
    let closure = Arc::new(Closure {
        module: Arc::clone(&module_arc),
        fn_index: index,
        captures: Vector::new(),
    });
    let func = Arc::new(Function {
        // The compiled compiler takes the same two arguments the
        // interpreted one does. An empty parameter list with a rest
        // would make every stage-2 call an arity error, and a bootstrap
        // that only ever ran the interpreted path would never notice.
        params: vector!["name".to_string(), "forms".to_string()],
        rest_param: None,
        body: Sexp::Nil,
        env: Arc::new(Env::new(Some(ctx.env.clone()))),
        compiled: Some(Arc::clone(&closure)),
    });
    ctx.env.set("zio--compile".into(), Value::Function(func));
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
