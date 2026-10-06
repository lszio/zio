//! String operation builtins.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

/// (str-join separator strings...) — join strings with separator.
pub fn str_join(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() < 1 {
        return Err(EvalError::wrong_arg_count_min(1, 0));
    }
    let sep = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let parts: Vec<String> = args.iter().skip(1).map(|v| match v {
        Value::String(s) => s.clone(),
        other => format!("{other}"),
    }).collect();
    Ok(Value::String(parts.join(&sep)))
}

/// (str-split separator string) → vector — split string on separator.
pub fn str_split(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let sep = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let s = match &args[1] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let parts: Vector<Value> = s.split(&sep).map(|p| Value::String(p.to_string())).collect();
    Ok(Value::Vector(parts))
}

/// (str-trim string) → string — trim whitespace.
pub fn str_trim(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::String(s) => Ok(Value::String(s.trim().to_string())),
        other => Err(EvalError::type_error("string",
        &format!("{}", other),)),
    }
}

/// (str-contains? haystack needle) → boolean — substring check.
pub fn str_contains(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let haystack = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let needle = match &args[1] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    Ok(Value::Boolean(haystack.contains(&needle)))
}

/// (str-starts-with? s prefix) → boolean.
pub fn str_starts_with(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let s = match &args[0] {
        Value::String(s_val) => s_val.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let prefix = match &args[1] {
        Value::String(p) => p.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    Ok(Value::Boolean(s.starts_with(&prefix)))
}

/// (str-ends-with? s suffix) → boolean.
pub fn str_ends_with(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let s = match &args[0] {
        Value::String(s_val) => s_val.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let suffix = match &args[1] {
        Value::String(suf) => suf.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    Ok(Value::Boolean(s.ends_with(&suffix)))
}

pub fn register(env: &Arc<Env>) {
    env.set("str-join".into(), Value::NativeFunction(NativeFn::new("str-join", str_join)));
    env.set("str-split".into(), Value::NativeFunction(NativeFn::new("str-split", str_split)));
    env.set("str-trim".into(), Value::NativeFunction(NativeFn::new("str-trim", str_trim)));
    env.set("str-contains?".into(), Value::NativeFunction(NativeFn::new("str-contains?", str_contains)));
    env.set("str-starts-with?".into(), Value::NativeFunction(NativeFn::new("str-starts-with?", str_starts_with)));
    env.set("str-ends-with?".into(), Value::NativeFunction(NativeFn::new("str-ends-with?", str_ends_with)));
}
