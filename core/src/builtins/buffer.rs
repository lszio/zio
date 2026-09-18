//! Mutable byte buffer builtins.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

pub fn bytes_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let data = match &args[0] {
        Value::Integer(n) => vec![0u8; (*n).max(0) as usize],
        Value::String(s) => s.as_bytes().to_vec(),
        Value::Vector(vec) | Value::List(vec) => {
            let mut buf = Vec::with_capacity(vec.len());
            for item in vec {
                match item {
                    Value::Integer(b) => buf.push(*b as u8),
                    other => return Err(EvalError::type_error("byte integer", other.value_type())),
                }
            }
            buf
        }
        other => return Err(EvalError::type_error("size, list, or string", other.value_type())),
    };
    Ok(Value::Buffer(Arc::new(std::sync::Mutex::new(data))))
}

pub fn buffer_length_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::Buffer(b) => Ok(Value::Integer(b.lock().unwrap().len() as i64)),
        other => Err(EvalError::type_error("buffer", other.value_type())),
    }
}

pub fn buffer_get_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let buf = match &args[0] {
        Value::Buffer(b) => b,
        other => return Err(EvalError::type_error("buffer", other.value_type())),
    };
    let idx = match &args[1] {
        Value::Integer(i) => *i as usize,
        other => return Err(EvalError::type_error("integer index", other.value_type())),
    };
    let lock = buf.lock().unwrap();
    if idx < lock.len() {
        Ok(Value::Integer(lock[idx] as i64))
    } else {
        Err(EvalError::custom(format!("buffer index out of bounds: {idx} (len {})", lock.len())))
    }
}

pub fn buffer_set_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::wrong_arg_count(3, args.len()));
    }
    let buf = match &args[0] {
        Value::Buffer(b) => b,
        other => return Err(EvalError::type_error("buffer", other.value_type())),
    };
    let idx = match &args[1] {
        Value::Integer(i) => *i as usize,
        other => return Err(EvalError::type_error("integer index", other.value_type())),
    };
    let val = match &args[2] {
        Value::Integer(b) => *b as u8,
        other => return Err(EvalError::type_error("byte integer", other.value_type())),
    };
    let mut lock = buf.lock().unwrap();
    if idx < lock.len() {
        lock[idx] = val;
        Ok(Value::Nil)
    } else {
        Err(EvalError::custom(format!("buffer index out of bounds: {idx} (len {})", lock.len())))
    }
}

pub fn register(env: &Arc<Env>) {
    env.set("bytes".into(), Value::NativeFunction(NativeFn::new("bytes", bytes_fn)));
    env.set("buffer-length".into(), Value::NativeFunction(NativeFn::new("buffer-length", buffer_length_fn)));
    env.set("buffer-get".into(), Value::NativeFunction(NativeFn::new("buffer-get", buffer_get_fn)));
    env.set("buffer-set!".into(), Value::NativeFunction(NativeFn::new("buffer-set!", buffer_set_fn)));
}
