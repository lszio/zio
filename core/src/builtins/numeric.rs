//! Arithmetic and comparison builtins.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

fn extract_numeric(args: &Vector<Value>) -> Result<(Vec<i64>, Vec<f64>, bool), EvalError> {
    let mut ints = Vec::new();
    let mut floats = Vec::new();
    let mut has_float = false;
    for arg in args {
        match arg {
            Value::Integer(i) => {
                ints.push(*i);
                floats.push(*i as f64);
            }
            Value::Float(f) => {
                ints.push(*f as i64);
                floats.push(*f);
                has_float = true;
            }
            other => {
                return Err(EvalError::type_error("number", other.value_type()));
            }
        }
    }
    Ok((ints, floats, has_float))
}

pub fn add(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let (ints, floats, has_float) = extract_numeric(&args)?;
    if has_float {
        Ok(Value::Float(floats.iter().sum()))
    } else {
        Ok(Value::Integer(ints.iter().sum()))
    }
}

pub fn sub(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let (ints, floats, has_float) = extract_numeric(&args)?;
    if args.is_empty() {
        return Err(EvalError::wrong_arg_count_min(1, 0));
    }
    if has_float {
        let init = floats[0];
        let rest = &floats[1..];
        Ok(Value::Float(if rest.is_empty() { -init } else { rest.iter().fold(init, |a, b| a - b) }))
    } else {
        let init = ints[0];
        let rest = &ints[1..];
        Ok(Value::Integer(if rest.is_empty() { -init } else { rest.iter().fold(init, |a, b| a - b) }))
    }
}

pub fn mul(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let (ints, floats, has_float) = extract_numeric(&args)?;
    if has_float {
        Ok(Value::Float(floats.iter().product()))
    } else {
        Ok(Value::Integer(ints.iter().product()))
    }
}

pub fn div(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let (ints, floats, has_float) = extract_numeric(&args)?;
    if args.is_empty() {
        return Err(EvalError::wrong_arg_count_min(1, 0));
    }
    if has_float {
        let init = floats[0];
        let rest = &floats[1..];
        if rest.is_empty() {
            Ok(Value::Float(1.0 / init))
        } else {
            rest.iter().try_fold(init, |a, b| {
                if *b == 0.0 {
                    Err(EvalError::division_by_zero())
                } else {
                    Ok(a / b)
                }
            })
            .map(Value::Float)
        }
    } else {
        let init = ints[0];
        let rest = &ints[1..];
        if rest.is_empty() {
            if init == 0 {
                Err(EvalError::division_by_zero())
            } else {
                Ok(Value::Float(1.0 / init as f64))
            }
        } else {
            rest.iter().try_fold(init, |a, b| {
                if *b == 0 {
                    Err(EvalError::division_by_zero())
                } else {
                    Ok(a / b)
                }
            })
            .map(Value::Integer)
        }
    }
}

/// (mod a b) → integer remainder.
pub fn mod_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let a = match &args[0] {
        Value::Integer(i) => *i,
        other => return Err(EvalError::type_error("integer", other.value_type())),
    };
    let b = match &args[1] {
        Value::Integer(i) => *i,
        other => return Err(EvalError::type_error("integer", other.value_type())),
    };
    if b == 0 {
        return Err(EvalError::custom("division by zero in mod"));
    }
    Ok(Value::Integer(a % b))
}

// ── Comparison ─────────────────────────────────────────────────────

pub fn eq(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let first = &args[0];
    for other in args.iter().skip(1) {
        if first != other {
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
}

fn cmp_int<F>(args: &Vector<Value>, op: F) -> Result<Value, EvalError>
where
    F: Fn(i64, i64) -> bool,
{
    if args.len() < 2 {
        return Err(EvalError::wrong_arg_count_min(2, args.len()));
    }
    let first = match &args[0] {
        Value::Integer(i) => *i,
        other => return Err(EvalError::type_error("integer", other.value_type())),
    };
    let second = match &args[1] {
        Value::Integer(i) => *i,
        other => return Err(EvalError::type_error("integer", other.value_type())),
    };
    Ok(Value::Boolean(op(first, second)))
}

pub fn lt(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a < b)
}

pub fn gt(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a > b)
}

pub fn le(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a <= b)
}

pub fn ge(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a >= b)
}

pub fn register(env: &Arc<Env>) {
    // Arithmetic
    env.set("+".into(), Value::NativeFunction(NativeFn::new("+", add)));
    env.set("-".into(), Value::NativeFunction(NativeFn::new("-", sub)));
    env.set("*".into(), Value::NativeFunction(NativeFn::new("*", mul)));
    env.set("/".into(), Value::NativeFunction(NativeFn::new("/", div)));
    env.set("mod".into(), Value::NativeFunction(NativeFn::new("mod", mod_fn)));

    // Comparison
    env.set("=".into(), Value::NativeFunction(NativeFn::new("=", eq)));
    env.set("<".into(), Value::NativeFunction(NativeFn::new("<", lt)));
    env.set(">".into(), Value::NativeFunction(NativeFn::new(">", gt)));
    env.set("<=".into(), Value::NativeFunction(NativeFn::new("<=", le)));
    env.set(">=".into(), Value::NativeFunction(NativeFn::new(">=", ge)));
}
