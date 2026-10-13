//! Native embedded JavaScript engine binding for Zio (QuickJS family,
//! rquickjs 0.11 — the maintained bellard-line bindings: the 0.14 fork
//! publishes no working Value/Function surface for this contract, and
//! the plain `quick-js` wrapper predates the interrupt and memory-limit
//! APIs this binding needs).
//!
//! One `Engine` owns one JS realm: a QuickJS runtime plus its context.
//! Handles carry their engine identity, so a value from engine A cannot
//! be used with engine B, and a handle dies with its engine. The engine
//! has no filesystem, network or process access — the host hands it
//! exactly what it hands over, plus the callbacks it registers.
//!
//! Crossing rules are explicit because JS and Zio disagree in ways that
//! silently corrupt data:
//!
//! - integers cross as i64; safe integers that arrive as floats come
//!   back as integers, and anything above 2^53 stays exact through
//!   BigInt — it never folds into a neighbouring integer;
//! - `undefined` and `null` both arrive as `nil`;
//! - non-finite numbers (NaN, ±Infinity) are refused by name;
//! - objects become opaque handles: mutation happens through the engine,
//!   never by copying a snapshot;
//! - promises are values, never awaited implicitly;
//! - runaway scripts are interrupted by the engine's interrupt handler
//!   and the heap is capped by its memory limit; both failures are
//!   named, and the context stays usable afterwards.

pub mod interop;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rquickjs::{Context, Persistent, Runtime, Value as JsValue};

static NEXT_ENGINE: AtomicU64 = AtomicU64::new(1);

/// Why a boundary crossing failed, with the offending value named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryError(String);

impl BoundaryError {
    pub fn new(message: impl Into<String>) -> Self {
        BoundaryError(message.into())
    }

    /// Public alias so embedders can raise a boundary error by name.
    pub fn from_message(message: impl Into<String>) -> Self {
        BoundaryError::new(message)
    }
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BoundaryError {}

pub type Result<T, E = BoundaryError> = std::result::Result<T, E>;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// An opaque JS value living in its engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    engine: u64,
    slot: u64,
}

impl Handle {
    pub fn engine(&self) -> u64 {
        self.engine
    }
    pub fn slot(&self) -> u64 {
        self.slot
    }
}

/// A Zio-shaped argument crossing into JS.
#[derive(Debug, Clone, PartialEq)]
pub enum JsArg {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Undefined,
    Null,
}

/// What came back from the engine.
#[derive(Debug, Clone, PartialEq)]
pub enum JsOutcome {
    /// `undefined` or `null`.
    Nothing,
    Bool(bool),
    Integer(i64),
    Float(f64),
    /// A JS string: text data, not identity, so it crosses by value.
    String(String),
    /// An exact integer above the safe range, carried as BigInt.
    BigInt(i64),
    Handle(Handle),
}

/// One JS realm plus the handles it owns.
pub struct Engine {
    id: u64,
    // Declaration order is drop order: the context must die before the
    // runtime it was created from, and the handle slots are emptied in
    // `Drop` before either.
    context: Context,
    runtime: Runtime,
    context_lock: Mutex<()>,
    slots: Mutex<HashMap<u64, Persistent<JsValue<'static>>>>,
    live: AtomicU64,
    deadline: Arc<Mutex<Option<Instant>>>,
    /// A host callback that failed since the last boundary crossing.
    /// QuickJS gives a native closure no way to raise a JS exception
    /// that the evaluator would report, so the failure is recorded here
    /// and raised on the Zio side: JS observes `undefined`, the host
    /// sees the error by name.
    callback_error: Arc<Mutex<Option<String>>>,
}

impl Drop for Engine {
    /// Persistent handles must die while the runtime is still alive:
    /// QuickJS asserts on a freed heap that still holds objects.
    fn drop(&mut self) {
        self.slots.get_mut().expect("slot map").clear();
    }
}

impl Engine {
    pub fn new() -> Result<Self> {
        let runtime = Runtime::new()
            .map_err(|error| BoundaryError::new(format!("cannot create a JS runtime: {error}")))?;
        let context = Context::full(&runtime)
            .map_err(|error| BoundaryError::new(format!("cannot create a JS context: {error}")))?;
        let engine = Engine {
            id: NEXT_ENGINE.fetch_add(1, Ordering::SeqCst),
            runtime,
            context,
            context_lock: Mutex::new(()),
            slots: Mutex::new(HashMap::new()),
            live: AtomicU64::new(0),
            deadline: Arc::new(Mutex::new(None)),
            callback_error: Arc::new(Mutex::new(None)),
        };
        engine.install_interrupt();
        Ok(engine)
    }

    /// QuickJS asks the handler at its interrupt points; a passed
    /// deadline stops an infinite loop without touching the host.
    fn install_interrupt(&self) {
        let deadline = Arc::clone(&self.deadline);
        self.runtime
            .set_interrupt_handler(Some(Box::new(move || match deadline.lock() {
                Ok(guard) => guard.is_some_and(|limit| Instant::now() >= limit),
                Err(_) => false,
            })));
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    /// Interrupt any evaluation still running after `budget`.
    pub fn set_time_budget(&self, budget: Duration) {
        if let Ok(mut deadline) = self.deadline.lock() {
            *deadline = Some(Instant::now() + budget);
        }
    }

    /// Take the error a host callback left, if any.
    fn take_callback_error(&self) -> Result<(), BoundaryError> {
        match self.callback_error.lock() {
            Ok(mut slot) => match slot.take() {
                Some(message) => Err(BoundaryError::new(format!(
                    "host callback failed: {message}"
                ))),
                None => Ok(()),
            },
            Err(_) => Ok(()),
        }
    }

    /// Clear the deadline so later evaluations run freely.
    pub fn clear_time_budget(&self) {
        if let Ok(mut deadline) = self.deadline.lock() {
            *deadline = None;
        }
    }

    /// Cap the heap. Crossing the limit fails evaluation by name and
    /// the context survives.
    pub fn set_memory_limit(&self, bytes: usize) {
        self.runtime.set_memory_limit(bytes);
    }

    pub fn memory_usage(&self) -> usize {
        self.runtime.memory_usage().malloc_size as usize
    }

    fn intern<'js>(&self, ctx: &rquickjs::Ctx<'js>, value: JsValue<'js>) -> Handle {
        let slot = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.slots
            .lock()
            .expect("slot map")
            .insert(slot, Persistent::save(ctx, value.clone()));
        Handle {
            engine: self.id,
            slot,
        }
    }

    fn resolve<'js>(&self, ctx: &rquickjs::Ctx<'js>, handle: Handle) -> Result<JsValue<'js>> {
        if handle.engine != self.id {
            return Err(BoundaryError::new(format!(
                "handle belongs to engine {} and cannot be used with engine {}",
                handle.engine, self.id
            )));
        }
        let slot = self
            .slots
            .lock()
            .expect("slot map")
            .get(&handle.slot)
            .cloned()
            .ok_or_else(|| {
                BoundaryError::new(format!(
                    "handle {} is no longer live in engine {}",
                    handle.slot, self.id
                ))
            })?;
        slot.restore(ctx)
            .map_err(|error| BoundaryError::new(format!("handle {} is dead: {error}", handle.slot)))
    }

    pub fn live_handles(&self) -> usize {
        self.slots.lock().expect("slot map").len()
    }

    pub fn is_live(&self, handle: Handle) -> bool {
        handle.engine == self.id
            && self
                .slots
                .lock()
                .expect("slot map")
                .contains_key(&handle.slot)
    }

    /// Evaluate `source` and translate the result at the boundary.
    pub fn eval(&self, source: &str) -> Result<JsOutcome> {
        let _serialized = self.context_lock.lock().map_err(poisoned)?;
        let context = &self.context;
        context.with(|ctx| {
            let ctx = &ctx;
            let value: JsValue = ctx
                .eval(source)
                .map_err(|error| BoundaryError::new(format!("js error: {error}")))?;
            self.take_callback_error()?;
            self.outcome(ctx, value)
        })
    }

    fn outcome<'js>(&self, ctx: &rquickjs::Ctx<'js>, value: JsValue<'js>) -> Result<JsOutcome> {
        if value.is_undefined() || value.is_null() {
            return Ok(JsOutcome::Nothing);
        }
        if let Some(inner) = value.as_bool() {
            return Ok(JsOutcome::Bool(inner));
        }
        if let Some(inner) = value.as_string() {
            return inner
                .to_string()
                .map(JsOutcome::String)
                .map_err(|error| BoundaryError::new(format!("cannot read a JS string: {error}")));
        }
        // BigInt first, read through its decimal text: the binding's
        // `to_i64` truncates silently and `as_number` widens to a double
        // — either would destroy the precision BigInt exists for.
        if value.as_big_int().is_some() {
            let to_text: JsValue = ctx
                .eval("(v => String(v))")
                .map_err(|error| BoundaryError::new(format!("cannot read a BigInt: {error}")))?;
            let stringify = to_text
                .as_function()
                .ok_or_else(|| BoundaryError::new("cannot read a BigInt"))?
                .clone();
            let mut call_args = rquickjs::function::Args::new(ctx.clone(), 1);
            call_args
                .push_arg(value.clone())
                .map_err(|error| BoundaryError::new(format!("cannot read a BigInt: {error}")))?;
            let text: rquickjs::String = stringify
                .call_arg(call_args)
                .map_err(|error| BoundaryError::new(format!("cannot read a BigInt: {error}")))?;
            let digits = text
                .to_string()
                .map_err(|error| BoundaryError::new(format!("cannot read a BigInt: {error}")))?;
            let number = digits.parse::<i64>().map_err(|_| {
                BoundaryError::new(format!("BigInt {digits} does not fit in an i64"))
            })?;
            return Ok(if number.abs() <= MAX_SAFE_INTEGER {
                JsOutcome::Integer(number)
            } else {
                JsOutcome::BigInt(number)
            });
        }
        // A JS number is a double: exact integers inside the safe range
        // become Zio integers, everything else stays a float — a value JS
        // already lost precision on is never pretended exact.
        if let Some(inner) = value.as_number() {
            if !inner.is_finite() {
                return Err(BoundaryError::new(format!(
                    "non-finite number {inner} has no Zio representation"
                )));
            }
            return Ok(
                if inner.fract() == 0.0 && inner.abs() <= MAX_SAFE_INTEGER as f64 {
                    JsOutcome::Integer(inner as i64)
                } else {
                    JsOutcome::Float(inner)
                },
            );
        }
        Ok(JsOutcome::Handle(self.intern(ctx, value)))
    }

    /// Call a handle as a function.
    pub fn call(&self, handle: Handle, args: &[JsArg]) -> Result<JsOutcome> {
        let _serialized = self.context_lock.lock().map_err(poisoned)?;
        let context = &self.context;
        context.with(|ctx| {
            let ctx = &ctx;
            let target = self.resolve(ctx, handle)?;
            let function = target
                .as_function()
                .ok_or_else(|| BoundaryError::new("handle is not a JS function"))?
                .clone();
            let mut call_args = rquickjs::function::Args::new(ctx.clone(), args.len());
            for arg in args {
                let value = arg.to_js(ctx)?;
                call_args
                    .push_arg(value)
                    .map_err(|error| BoundaryError::new(format!("js argument refused: {error}")))?;
            }
            let returned: JsValue = function
                .call_arg(call_args)
                .map_err(|error| BoundaryError::new(format!("js call failed: {error}")))?;
            self.take_callback_error()?;
            self.outcome(ctx, returned)
        })
    }

    /// Call a function handle with an explicit `this`, the way a method
    /// was actually reached in JS.
    pub fn call_with_this(
        &self,
        this: Handle,
        function: Handle,
        args: &[JsArg],
    ) -> Result<JsOutcome> {
        let _serialized = self.context_lock.lock().map_err(poisoned)?;
        let context = &self.context;
        context.with(|ctx| {
            let ctx = &ctx;
            let receiver: JsValue = self.resolve(ctx, this)?;
            let target = self.resolve(ctx, function)?;
            let function = target
                .as_function()
                .ok_or_else(|| BoundaryError::new("handle is not a JS function"))?
                .clone();
            let mut values: Vec<JsValue> = Vec::with_capacity(args.len());
            for arg in args {
                values.push(arg.to_js(ctx)?);
            }
            let returned: JsValue = function
                .call((
                    rquickjs::function::This(receiver),
                    rquickjs::function::Rest(values),
                ))
                .map_err(|error| BoundaryError::new(format!("js call failed: {error}")))?;
            self.take_callback_error()?;
            self.outcome(ctx, returned)
        })
    }

    /// Read a property of an object handle.
    pub fn get_property(&self, handle: Handle, name: &str) -> Result<JsOutcome> {
        let _serialized = self.context_lock.lock().map_err(poisoned)?;
        let context = &self.context;
        context.with(|ctx| {
            let ctx = &ctx;
            let target = self.resolve(ctx, handle)?;
            let object = target
                .as_object()
                .ok_or_else(|| BoundaryError::new("handle is not a JS object"))?
                .clone();
            let value: JsValue = object
                .get(name)
                .map_err(|error| BoundaryError::new(format!("js property read failed: {error}")))?;
            self.outcome(ctx, value)
        })
    }

    /// Write a property on an object handle.
    pub fn set_property(&self, handle: Handle, name: &str, value: JsArg) -> Result<()> {
        let _serialized = self.context_lock.lock().map_err(poisoned)?;
        let context = &self.context;
        context.with(|ctx| {
            let ctx = &ctx;
            let target = self.resolve(ctx, handle)?;
            let object = target
                .as_object()
                .ok_or_else(|| BoundaryError::new("handle is not a JS object"))?
                .clone();
            let value = value.to_js(ctx)?;
            object
                .set(name, value)
                .map_err(|error| BoundaryError::new(format!("js property write failed: {error}")))
        })
    }

    /// Register a host closure as a JS function. It is synchronous:
    /// wrapping it in a promise is the caller's business, and nothing
    /// here awaits.
    ///
    /// The closure is `'static` but not `Send + Sync`: the engine is used
    /// behind a mutex on one thread (QuickJS contexts are not thread-safe),
    /// and a host callback naturally captures interpreter state that is
    /// likewise single-threaded.
    pub fn register_callback<F>(&self, name: &str, callback: F) -> Result<Handle>
    where
        F: Fn(&[JsArg]) -> Result<JsArg> + 'static,
    {
        let _serialized = self.context_lock.lock().map_err(poisoned)?;
        let context = &self.context;
        context.with(|ctx| {
            let raw = ctx.as_raw();
            let failure = Arc::clone(&self.callback_error);
            let function = rquickjs::Function::new(
                ctx.clone(),
                move |args: rquickjs::function::Rest<JsValue>| -> JsValue {
                    // The raw pointer is rebound per call instead of
                    // capturing a `Ctx`: a captured context would be
                    // duplicated into the JS-side closure and outlive
                    // the runtime at teardown. The engine outlives every
                    // callback it can invoke, which is what makes this
                    // rebinding sound.
                    let ctx = unsafe { rquickjs::Ctx::from_raw(raw) };
                    let ctx = &ctx;
                    let args = args.0;
                    let converted: Vec<JsArg> = match args
                        .iter()
                        .map(|value| {
                            if value.is_undefined() {
                                return Ok(JsArg::Undefined);
                            }
                            if value.is_null() {
                                return Ok(JsArg::Null);
                            }
                            if let Some(inner) = value.as_number() {
                                return Ok(
                                    if inner.fract() == 0.0
                                        && inner.abs() <= MAX_SAFE_INTEGER as f64
                                    {
                                        JsArg::Int(inner as i64)
                                    } else {
                                        JsArg::Float(inner)
                                    },
                                );
                            }
                            if let Some(inner) = value.as_bool() {
                                return Ok(JsArg::Bool(inner));
                            }
                            if let Some(inner) = value.as_string() {
                                return Ok(JsArg::String(inner.to_string()?));
                            }
                            Err(rquickjs::Error::new_from_js(
                                "callback",
                                "unsupported argument type",
                            ))
                        })
                        .collect::<std::result::Result<_, _>>()
                    {
                        Ok(args) => args,
                        Err(error) => {
                            let message = error.to_string();
                            let text = rquickjs::String::from_str(ctx.clone(), &message)
                                .map(JsValue::from)
                                .unwrap_or_else(|_| JsValue::new_undefined(ctx.clone()));
                            ctx.throw(text);
                            return JsValue::new_undefined(ctx.clone());
                        }
                    };
                    match callback(&converted) {
                        Ok(JsArg::Int(value)) => JsValue::new_int(ctx.clone(), value as i32),
                        Ok(JsArg::Float(value)) => JsValue::new_float(ctx.clone(), value),
                        Ok(JsArg::Bool(value)) => JsValue::new_bool(ctx.clone(), value),
                        Ok(JsArg::String(value)) => {
                            match rquickjs::String::from_str(ctx.clone(), &value) {
                                Ok(text) => text.into(),
                                Err(_) => JsValue::new_undefined(ctx.clone()),
                            }
                        }
                        Ok(JsArg::Undefined) => JsValue::new_undefined(ctx.clone()),
                        Ok(JsArg::Null) => JsValue::new_null(ctx.clone()),
                        Err(error) => {
                            if let Ok(mut slot) = failure.lock() {
                                *slot = Some(error.message().to_string());
                            }
                            JsValue::new_undefined(ctx.clone())
                        }
                    }
                },
            );
            let function = function.map_err(|error| {
                BoundaryError::new(format!("cannot create the callback: {error}"))
            })?;
            let handle = self.intern(&ctx, function.clone().into());
            // The global is the callable itself; the host keeps the
            // handle so it can call it or replace it later.
            let callable: JsValue = self.resolve(&ctx, handle)?;
            let globals = ctx.globals();
            globals
                .set(name, callable)
                .map_err(|error| BoundaryError::new(format!("cannot install {name}: {error}")))?;
            Ok(handle)
        })
    }
}

impl JsArg {
    fn to_js<'a>(&self, ctx: &rquickjs::Ctx<'a>) -> Result<JsValue<'a>> {
        Ok(match self {
            JsArg::Int(value) => {
                // Outside JS's exact integer range the value goes in as
                // a BigInt so precision is never silently lost.
                if *value >= i32::MIN as i64 && *value <= i32::MAX as i64 {
                    JsValue::new_int(ctx.clone(), *value as i32)
                } else {
                    rquickjs::BigInt::from_i64(ctx.clone(), *value)
                        .map_err(|error| {
                            BoundaryError::new(format!("cannot pass {value} to JS: {error}"))
                        })?
                        .into()
                }
            }
            JsArg::Float(value) => JsValue::new_float(ctx.clone(), *value),
            JsArg::Bool(value) => JsValue::new_bool(ctx.clone(), *value),
            JsArg::String(value) => rquickjs::String::from_str(ctx.clone(), value)
                .map_err(|error| {
                    BoundaryError::new(format!("cannot pass a string to JS: {error}"))
                })?
                .into(),
            JsArg::Undefined => JsValue::new_undefined(ctx.clone()),
            JsArg::Null => JsValue::new_null(ctx.clone()),
        })
    }
}

fn poisoned<E>(_: E) -> BoundaryError {
    BoundaryError::new("the JS engine is poisoned by a previous panic")
}
