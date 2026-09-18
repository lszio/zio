//! Type predicates, character operations, and type conversions.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

fn unary_pred<P>(args: &Vector<Value>, pred: P) -> Result<Value, EvalError>
where
    P: Fn(&Value) -> bool,
{
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    Ok(Value::Boolean(pred(&args[0])))
}

pub fn is_nil(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Nil))
}

pub fn is_boolean(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Boolean(_)))
}

pub fn is_number(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Integer(_) | Value::Float(_)))
}

pub fn is_string(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::String(_)))
}

pub fn is_symbol(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Symbol(_)))
}

pub fn is_keyword(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Keyword(_)))
}

pub fn is_list(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::List(_)))
}

pub fn is_vector(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Vector(_)))
}

pub fn is_map(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Map(_)))
}

pub fn is_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| {
        matches!(v, Value::Function(_) | Value::NativeFunction(_) | Value::Macro(_))
    })
}

pub fn is_char(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Char(_)))
}

// ── Character Operations ──────────────────────────────────────────

pub fn char_to_integer(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::Char(c) => Ok(Value::Integer(*c as i64)),
        other => Err(EvalError::type_error("character", other.value_type())),
    }
}

pub fn integer_to_char(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::Integer(i) => {
            if let Some(c) = char::from_u32(*i as u32) {
                Ok(Value::Char(c))
            } else {
                Err(EvalError::custom(format!("invalid character codepoint: {i}")))
            }
        }
        other => Err(EvalError::type_error("integer", other.value_type())),
    }
}

pub fn char_eq(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    match (&args[0], &args[1]) {
        (Value::Char(a), Value::Char(b)) => Ok(Value::Boolean(a == b)),
        _ => Err(EvalError::type_error("character", "non-character")),
    }
}

// ── Type Reflection ────────────────────────────────────────────────

pub fn type_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let kw = match &args[0] {
        Value::Nil => "nil",
        Value::Boolean(_) => "boolean",
        Value::Integer(_) => "integer",
        Value::Float(_) => "float",
        Value::String(_) => "string",
        Value::Symbol(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::List(_) => "list",
        Value::Vector(_) => "vector",
        Value::Map(_) => "map",
        Value::Function(_) => "fn",
        Value::NativeFunction(_) => "native-fn",
        Value::Macro(_) => "macro",
        Value::Char(_) => "character",
        Value::Object(_) => "object",
        Value::Buffer(_) => "buffer",
        Value::Future(_) => "future",
        Value::Channel(_) => "channel",
    };
    Ok(Value::Keyword(kw.to_string()))
}

// ── Type Conversion ────────────────────────────────────────────────

/// (str ...) → string — concatenate args. Does not add quotes to strings.
pub fn str_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let result: String = args.iter().map(|v| match v {
        Value::String(s) => s.clone(),
        other => format!("{other}"),
    }).collect();
    Ok(Value::String(result))
}

/// (keyword x) → keyword — convert string or symbol to keyword.
pub fn keyword_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.clone(),
        Value::Symbol(s) => s.clone(),
        Value::Keyword(k) => return Ok(Value::Keyword(k.clone())),
        other => return Err(EvalError::type_error("string, symbol, or keyword", other.value_type())),
    };
    Ok(Value::Keyword(s))
}

/// (symbol x) → symbol — convert string or keyword to symbol.
pub fn symbol_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let s = match &args[0] {
        Value::String(s) => s.clone(),
        Value::Symbol(s) => return Ok(Value::Symbol(s.clone())),
        Value::Keyword(k) => k.clone(),
        other => return Err(EvalError::type_error("string, symbol, or keyword", other.value_type())),
    };
    Ok(Value::Symbol(s))
}

pub fn register(env: &Arc<Env>) {
    // Type predicates
    env.set("nil?".into(), Value::NativeFunction(NativeFn::new("nil?", is_nil)));
    env.set("boolean?".into(), Value::NativeFunction(NativeFn::new("boolean?", is_boolean)));
    env.set("number?".into(), Value::NativeFunction(NativeFn::new("number?", is_number)));
    env.set("string?".into(), Value::NativeFunction(NativeFn::new("string?", is_string)));
    env.set("symbol?".into(), Value::NativeFunction(NativeFn::new("symbol?", is_symbol)));
    env.set("keyword?".into(), Value::NativeFunction(NativeFn::new("keyword?", is_keyword)));
    env.set("list?".into(), Value::NativeFunction(NativeFn::new("list?", is_list)));
    env.set("vector?".into(), Value::NativeFunction(NativeFn::new("vector?", is_vector)));
    env.set("map?".into(), Value::NativeFunction(NativeFn::new("map?", is_map)));
    env.set("fn?".into(), Value::NativeFunction(NativeFn::new("fn?", is_fn)));
    env.set("char?".into(), Value::NativeFunction(NativeFn::new("char?", is_char)));

    // Characters
    env.set("char->integer".into(), Value::NativeFunction(NativeFn::new("char->integer", char_to_integer)));
    env.set("integer->char".into(), Value::NativeFunction(NativeFn::new("integer->char", integer_to_char)));
    env.set("char=?".into(), Value::NativeFunction(NativeFn::new("char=?", char_eq)));

    // Reflection
    env.set("type".into(), Value::NativeFunction(NativeFn::new("type", type_fn)));

    // Conversion
    env.set("str".into(), Value::NativeFunction(NativeFn::new("str", str_fn)));
    env.set("keyword".into(), Value::NativeFunction(NativeFn::new("keyword", keyword_fn)));
    env.set("symbol".into(), Value::NativeFunction(NativeFn::new("symbol", symbol_fn)));
}
