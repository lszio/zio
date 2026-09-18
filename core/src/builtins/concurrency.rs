//! Future/promise and CSP channel builtins.
//!
//! ⚠ Synchronous placeholder semantics (ADR-012): `EvalContext` is `!Send`
//! (its `Env` chain uses `RefCell`), so nothing here spawns a thread today.
//! `future-call` evaluates eagerly on the calling thread and wraps the
//! finished value; `deref` on such a future never blocks in practice.
//! Channels work for same-thread buffering and for hosts (JS, native
//! embedders) that drive the other end. Real concurrency is a deliberate
//! future decision — see docs/adrs.md ADR-012.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

/// (future-call f) → future — SYNCHRONOUS PLACEHOLDER (ADR-012).
/// Evaluates f immediately on the calling thread and wraps the result in a
/// Future value. There is no background thread.
pub fn future_call_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let func = match &args[0] {
        Value::Function(f) => f.clone(),
        other => return Err(EvalError::type_error("function", other.value_type())),
    };

    let pair = Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
    let env = Env::bind(&func.env, &func.params, &im::vector![])?;
    let res = engine.eval_expr(&func.body, &env, false);
    let val = match res {
        Ok(tr) => tr.into_value(),
        Err(_) => Value::Nil,
    };
    {
        let (lock, cvar) = &*pair;
        let mut guard = lock.lock().unwrap();
        *guard = Some(val);
        cvar.notify_all();
    }
    Ok(Value::Future(pair))
}

pub fn promise_fn(_args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let pair = Arc::new((std::sync::Mutex::new(None), std::sync::Condvar::new()));
    Ok(Value::Future(pair))
}

pub fn deliver_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let pair = match &args[0] {
        Value::Future(p) => p.clone(),
        other => return Err(EvalError::type_error("future or promise", other.value_type())),
    };
    let val = args[1].clone();
    let (lock, cvar) = &*pair;
    {
        let mut guard = lock.lock().unwrap();
        if guard.is_none() {
            *guard = Some(val);
            cvar.notify_all();
        }
    }
    Ok(Value::Future(pair))
}

pub fn deref_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::Future(pair) => {
            let (lock, cvar) = &**pair;
            let mut guard = lock.lock().unwrap();
            while guard.is_none() {
                guard = cvar.wait(guard).unwrap();
            }
            Ok(guard.as_ref().unwrap().clone())
        }
        other => Err(EvalError::type_error("future or promise", other.value_type())),
    }
}

pub fn chan_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let (tx, rx) = if args.len() == 1 {
        let cap = match &args[0] {
            Value::Integer(n) => (*n).max(1) as usize,
            other => return Err(EvalError::type_error("integer capacity", other.value_type())),
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(cap);
        (crate::value::ChannelTx::Sync(tx), rx)
    } else {
        let (tx, rx) = std::sync::mpsc::channel();
        (crate::value::ChannelTx::Async(tx), rx)
    };
    let pair = crate::value::ChannelPair {
        tx: std::sync::Mutex::new(tx),
        rx: std::sync::Mutex::new(rx),
    };
    Ok(Value::Channel(Arc::new(pair)))
}

pub fn send_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let chan = match &args[0] {
        Value::Channel(c) => c,
        other => return Err(EvalError::type_error("channel", other.value_type())),
    };
    let val = args[1].clone();
    let tx = chan.tx.lock().unwrap();
    tx.send(val).map_err(|e| EvalError::custom(format!("send! error: {e}")))?;
    Ok(Value::Nil)
}

pub fn recv_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let chan = match &args[0] {
        Value::Channel(c) => c,
        other => return Err(EvalError::type_error("channel", other.value_type())),
    };
    let rx = chan.rx.lock().unwrap();
    match rx.recv() {
        Ok(v) => Ok(v),
        Err(_) => Ok(Value::Nil),
    }
}

pub fn register(env: &Arc<Env>) {
    env.set("future-call".into(), Value::NativeFunction(NativeFn::new("future-call", future_call_fn)));
    env.set("promise".into(), Value::NativeFunction(NativeFn::new("promise", promise_fn)));
    env.set("deliver".into(), Value::NativeFunction(NativeFn::new("deliver", deliver_fn)));
    env.set("deref".into(), Value::NativeFunction(NativeFn::new("deref", deref_fn)));
    env.set("chan".into(), Value::NativeFunction(NativeFn::new("chan", chan_fn)));
    env.set("send!".into(), Value::NativeFunction(NativeFn::new("send!", send_fn)));
    env.set("recv!".into(), Value::NativeFunction(NativeFn::new("recv!", recv_fn)));
}
