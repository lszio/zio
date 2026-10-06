//! JSON conversion builtins.
//!
//! `json-parse` reuses the reader (JSON is a subset of the map/vector/
//! literal syntax), so it accepts Zio literals beyond strict JSON.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

fn value_to_json_string(val: &Value) -> String {
    match val {
        Value::Nil => "null".into(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => format!("{s:?}"),
        Value::Keyword(k) | Value::Symbol(k) => format!("{k:?}"),
        Value::List(l) | Value::Vector(l) => {
            let items: Vec<String> = l.iter().map(value_to_json_string).collect();
            format!("[{}]", items.join(","))
        }
        Value::Map(m) => {
            let pairs: Vec<String> = m
                .iter()
                .map(|(k, v)| {
                    let k_str = match k {
                        Value::String(s) | Value::Symbol(s) | Value::Keyword(s) => format!("{s:?}"),
                        _ => format!("{:?}", k.to_string()),
                    };
                    format!("{k_str}:{}", value_to_json_string(v))
                })
                .collect();
            format!("{{{}}}", pairs.join(","))
        }
        Value::Char(c) => format!("{c:?}"),
        _ => "null".into(),
    }
}

pub fn json_stringify_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    Ok(Value::String(value_to_json_string(&args[0])))
}

/// Insert whitespace around JSON structural `:` and `,` (outside strings)
/// so the reader tokenizes values cleanly — e.g. `"count":4,"total":40`
/// must not absorb `,4` into a keyword token.
fn json_to_reader_syntax(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut in_string = false;
    let mut escaped = false;
    for c in s.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else {
            match c {
                '"' => {
                    in_string = true;
                    out.push(c);
                }
                ':' | ',' => {
                    out.push(' ');
                    out.push(c);
                    out.push(' ');
                }
                other => out.push(other),
            }
        }
    }
    out
}

pub fn json_parse_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s,
        other => return Err(EvalError::type_error("string", other.value_type())),
    };
    let sexp = crate::reader::reader::read(&json_to_reader_syntax(s))
        .map_err(|e| EvalError::custom(format!("JSON parse error: {e}")))?;
    Ok(Value::from(sexp))
}

pub fn register(env: &Arc<Env>) {
    env.set("json-stringify".into(), Value::NativeFunction(NativeFn::new("json-stringify", json_stringify_fn)));
    env.set("json-parse".into(), Value::NativeFunction(NativeFn::new("json-parse", json_parse_fn)));
}
