use crate::core::value::Value;
use im::Vector;
use std::sync::Arc;
use crate::core::env::Env;

pub fn add(args: Vector<Value>) -> Result<Value, String> {
    let mut sum = 0;
    for arg in args {
        match arg {
            Value::Integer(i) => sum += i,
            _ => return Err("add requires integers".to_string()),
        }
    }
    Ok(Value::Integer(sum))
}

pub fn sub(args: Vector<Value>) -> Result<Value, String> {
    if args.is_empty() {
        return Err("sub requires at least one argument".to_string());
    }
    let mut iter = args.into_iter();
    let mut res = match iter.next().unwrap() {
        Value::Integer(i) => i,
        _ => return Err("sub requires integers".to_string()),
    };
    if iter.len() == 0 {
        return Ok(Value::Integer(-res));
    }
    for arg in iter {
        match arg {
            Value::Integer(i) => res -= i,
            _ => return Err("sub requires integers".to_string()),
        }
    }
    Ok(Value::Integer(res))
}

pub fn mul(args: Vector<Value>) -> Result<Value, String> {
    let mut res = 1;
    for arg in args {
        match arg {
            Value::Integer(i) => res *= i,
            _ => return Err("mul requires integers".to_string()),
        }
    }
    Ok(Value::Integer(res))
}

pub fn div(args: Vector<Value>) -> Result<Value, String> {
    if args.is_empty() {
        return Err("div requires at least one argument".to_string());
    }
    let mut iter = args.into_iter();
    let mut res = match iter.next().unwrap() {
        Value::Integer(i) => i,
        _ => return Err("div requires integers".to_string()),
    };
    if iter.len() == 0 {
        return Ok(Value::Integer(1 / res));
    }
    for arg in iter {
        match arg {
            Value::Integer(i) => {
                if i == 0 {
                    return Err("division by zero".to_string());
                }
                res /= i;
            }
            _ => return Err("div requires integers".to_string()),
        }
    }
    Ok(Value::Integer(res))
}

pub fn eq(args: Vector<Value>) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    let first = &args[0];
    for arg in args.iter().skip(1) {
        if arg != first {
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
}

pub fn lt(args: Vector<Value>) -> Result<Value, String> {
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    for i in 0..args.len() - 1 {
        match (&args[i], &args[i + 1]) {
            (Value::Integer(a), Value::Integer(b)) => {
                if a >= b {
                    return Ok(Value::Boolean(false));
                }
            }
            _ => return Err("< requires integers".to_string()),
        }
    }
    Ok(Value::Boolean(true))
}

pub fn setup_env(env: Arc<Env>) {
    env.set("+".to_string(), Value::NativeFunction(add));
    env.set("-".to_string(), Value::NativeFunction(sub));
    env.set("*".to_string(), Value::NativeFunction(mul));
    env.set("/".to_string(), Value::NativeFunction(div));
    env.set("=".to_string(), Value::NativeFunction(eq));
    env.set("<".to_string(), Value::NativeFunction(lt));
}
