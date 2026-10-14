//! Zio-facing bindings for the native JS engine. The engine stays in
//! `zio_js`; this module is the language surface: `js/eval`, `js/call`,
//! `js/get`, `js/set` and `js/on`. Everything crosses through the same
//! boundary rules the engine enforces, and a handle is opaque — the only
//! way to touch a JS object is through these operations.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::{BoundaryError, Engine, Handle, JsArg, JsOutcome};
use zio_core::context::EvalContext;
use zio_core::error::EvalError;
use zio_core::eval::apply;
use zio_core::im::Vector;
use zio_core::value::{NativeFn, Value};

/// Live engines plus every handle handed to Zio. QuickJS contexts are
/// single-threaded (`Rc`-backed, not `Send`), so this table is an `Rc`
/// side table shared by the `js/*` closures rather than anything
/// cross-thread. Handles are namespaced by engine id, so two engines can
/// never hand each other's values back.
#[derive(Clone, Default)]
struct Engines {
    inner: Rc<RefCell<HashMap<u64, Rc<Engine>>>>,
}

impl Engines {
    fn engine(&self, id: i64) -> Result<Rc<Engine>, EvalError> {
        self.inner
            .borrow()
            .get(&(id as u64))
            .cloned()
            .ok_or_else(|| EvalError::custom(format!("no JS engine with id {id}")))
    }

    fn add(&self, engine: Rc<Engine>) -> Result<i64, EvalError> {
        let id = engine.id();
        self.inner.borrow_mut().insert(id, engine);
        Ok(id as i64)
    }
}

/// Install the `js/*` bindings over `ctx`. One engine per context is
/// created lazily, so a program that never touches JS pays nothing.
///
/// `ctx` is an `Rc` because a `js/on` callback re-enters the evaluator
/// from inside a QuickJS native call and must *own* the context it calls
/// back into — QuickJS contexts are single-threaded, so an `Rc` is the
/// right handle and no reference is borrowed for the callback's
/// `'static` lifetime. The caller keeps its own `Rc` and evaluates
/// through it.
pub fn register(ctx: Rc<EvalContext>) {
    let engines = Engines::default();

    let engines_for_new = engines.clone();
    ctx.env.set(
        "js/new".into(),
        native("js/new", move |args, _| {
            expect(args, 0)?;
            let engine = Rc::new(Engine::new().map_err(js_error)?);
            Ok(Value::Integer(engines_for_new.add(engine)?))
        }),
    );

    let engines_for_eval = engines.clone();
    ctx.env.set(
        "js/eval".into(),
        native("js/eval", move |args, _| {
            expect(args, 2)?;
            let engine = engines_for_eval.engine(integer(&args[0])?)?;
            let outcome = engine
                .eval(&text(&args[1])?)
                .map_err(|error| js_error(error))?;
            outcome_value(&outcome)
        }),
    );

    let engines_for_call = engines.clone();
    ctx.env.set(
        "js/call".into(),
        native("js/call", move |args, _engine| {
            expect(args, 2)?;
            let js = engines_for_call.engine(integer(&args[0])?)?;
            let function = handle(&args[1])?;
            let call_args = match args.get(2) {
                Some(Value::List(items)) | Some(Value::Vector(items)) => {
                    items.iter().map(js_arg).collect::<Result<Vec<_>, _>>()?
                }
                _ => Vec::new(),
            };
            let outcome = js.call(function, &call_args).map_err(js_error)?;
            outcome_value(&outcome)
        }),
    );

    let engines_for_call_this = engines.clone();
    ctx.env.set(
        "js/call-this".into(),
        native("js/call-this", move |args, _engine| {
            expect(args, 4)?;
            let js = engines_for_call_this.engine(integer(&args[0])?)?;
            let receiver = handle(&args[1])?;
            let function = handle(&args[2])?;
            let call_args = match args.get(3) {
                Some(Value::List(items)) | Some(Value::Vector(items)) => {
                    items.iter().map(js_arg).collect::<Result<Vec<_>, _>>()?
                }
                _ => Vec::new(),
            };
            let outcome = js
                .call_with_this(receiver, function, &call_args)
                .map_err(js_error)?;
            outcome_value(&outcome)
        }),
    );

    let engines_for_get = engines.clone();
    ctx.env.set(
        "js/get".into(),
        native("js/get", move |args, _| {
            expect(args, 3)?;
            let js = engines_for_get.engine(integer(&args[0])?)?;
            let outcome = js
                .get_property(handle(&args[1])?, &text(&args[2])?)
                .map_err(js_error)?;
            outcome_value(&outcome)
        }),
    );

    let engines_for_set = engines.clone();
    ctx.env.set(
        "js/set".into(),
        native("js/set", move |args, _| {
            expect(args, 4)?;
            let js = engines_for_set.engine(integer(&args[0])?)?;
            let target = handle(&args[1])?;
            let value = js_arg(&args[3])?;
            js.set_property(target, &text(&args[2])?, value)
                .map_err(js_error)?;
            Ok(Value::Nil)
        }),
    );

    // js/on registers a Zio closure as a JS function. The callback is
    // synchronous: a JS promise around it is the caller's business.
    let engines_for_on = engines.clone();
    let ctx_for_callbacks = Rc::clone(&ctx);
    ctx.env.set(
        "js/on".into(),
        native("js/on", move |args, _engine| {
            expect(args, 3)?;
            let js = engines_for_on.engine(integer(&args[0])?)?;
            let name = text(&args[1])?;
            let callback = args[2].clone();
            // The callback owns the context it re-enters.
            let ctx_engine = Rc::clone(&ctx_for_callbacks);
            js.register_callback(&name, move |call_args| {
                let mut zio_args = Vector::new();
                for arg in call_args {
                    zio_args.push_back(zio_value(arg));
                }
                let engine: &dyn zio_core::context::EvalEngine = &*ctx_engine;
                let value = match apply(callback.clone(), zio_args, engine)
                    .map_err(|error| BoundaryError::new(error.to_string()))?
                {
                    zio_core::special::TailResult::Value(value) => value,
                    _ => Value::Nil,
                };
                match value {
                    Value::Integer(inner) => Ok(JsArg::Int(inner)),
                    Value::Float(inner) => Ok(JsArg::Float(inner)),
                    Value::Boolean(inner) => Ok(JsArg::Bool(inner)),
                    Value::String(inner) => Ok(JsArg::String(inner)),
                    Value::Nil => Ok(JsArg::Undefined),
                    other => Err(BoundaryError::new(format!(
                        "callback returned {}, which has no JS form",
                        other.value_type()
                    ))),
                }
            })
            .map_err(js_error)?;
            Ok(Value::Integer(js.id() as i64))
        }),
    );
}

/// `js/*` bindings are interpreter-local (they capture a `Rc` engine
/// table), so their native closures are `'static` but single-threaded.
fn native(
    name: &str,
    body: impl Fn(&Vector<Value>, &dyn zio_core::context::EvalEngine) -> Result<Value, EvalError>
    + 'static,
) -> Value {
    Value::NativeFunction(NativeFn::new(name, move |args, engine| body(&args, engine)))
}

fn expect(args: &Vector<Value>, minimum: usize) -> Result<(), EvalError> {
    if args.len() < minimum {
        return Err(EvalError::wrong_arg_count(minimum, args.len()));
    }
    Ok(())
}

fn integer(value: &Value) -> Result<i64, EvalError> {
    match value {
        Value::Integer(inner) => Ok(*inner),
        other => Err(EvalError::type_error("integer", other.value_type())),
    }
}

fn text(value: &Value) -> Result<String, EvalError> {
    match value {
        Value::String(inner) => Ok(inner.clone()),
        other => Err(EvalError::type_error("string", other.value_type())),
    }
}

/// Handles travel as `#js<engine>:<slot>` strings. The engine id in the
/// handle is what makes cross-engine use fail loudly instead of quietly.
fn handle(value: &Value) -> Result<Handle, EvalError> {
    let raw = text(value)?;
    let rest = raw
        .strip_prefix("#js")
        .ok_or_else(|| EvalError::invalid_form(format!("{raw} is not a JS handle")))?;
    let (engine, slot) = rest
        .split_once(':')
        .ok_or_else(|| EvalError::invalid_form(format!("{raw} is not a JS handle")))?;
    Ok(Handle {
        engine: engine
            .parse::<u64>()
            .map_err(|_| EvalError::invalid_form(format!("{raw} has no engine id")))?,
        slot: slot
            .parse::<u64>()
            .map_err(|_| EvalError::invalid_form(format!("{raw} has no slot")))?,
    })
}

fn js_error(error: BoundaryError) -> EvalError {
    EvalError::custom(error.message().to_string())
}

fn outcome_value(outcome: &JsOutcome) -> Result<Value, EvalError> {
    Ok(match outcome {
        JsOutcome::Nothing => Value::Nil,
        JsOutcome::Bool(inner) => Value::Boolean(*inner),
        JsOutcome::Integer(inner) | JsOutcome::BigInt(inner) => Value::Integer(*inner),
        JsOutcome::Float(inner) => Value::Float(*inner),
        JsOutcome::String(inner) => Value::String(inner.clone()),
        JsOutcome::Handle(handle) => {
            Value::String(format!("#js{}:{}", handle.engine(), handle.slot()))
        }
    })
}

fn js_arg(value: &Value) -> Result<JsArg, EvalError> {
    Ok(match value {
        Value::Integer(inner) => JsArg::Int(*inner),
        Value::Float(inner) => JsArg::Float(*inner),
        Value::Boolean(inner) => JsArg::Bool(*inner),
        Value::String(inner) => JsArg::String(inner.clone()),
        Value::Nil => JsArg::Null,
        other => {
            return Err(EvalError::invalid_form(format!(
                "{} has no JS argument form",
                other.value_type()
            )));
        }
    })
}

fn zio_value(arg: &JsArg) -> Value {
    match arg {
        JsArg::Int(inner) => Value::Integer(*inner),
        JsArg::Float(inner) => Value::Float(*inner),
        JsArg::Bool(inner) => Value::Boolean(*inner),
        JsArg::String(inner) => Value::String(inner.clone()),
        JsArg::Undefined => Value::Nil,
        JsArg::Null => Value::Nil,
    }
}
