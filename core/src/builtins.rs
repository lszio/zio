use std::sync::Arc;
use im::Vector;
use im::vector;
use crate::context::EvalEngine;
use crate::env::Env;
use crate::sexp::Sexp;
use crate::error::EvalError;
use crate::macros;
use crate::special::TailResult;
use crate::value::{is_truthy, Value};

// ── Arithmetic ─────────────────────────────────────────────────────

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

// ── Type Predicates ────────────────────────────────────────────────

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


// ── Character Operations ──────────────────────────────────────────

pub fn is_char(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Char(_)))
}

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

// ── Cons / List Operations ─────────────────────────────────────────

pub fn cons(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let mut list = match &args[1] {
        Value::List(l) => l.clone(),
        other => return Err(EvalError::type_error("list", other.value_type())),
    };
    list.push_front(args[0].clone());
    Ok(Value::List(list))
}

pub fn car(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::List(l) => l.front().cloned().ok_or_else(|| EvalError::custom("cannot take car of empty list")),
        other => Err(EvalError::type_error("list", other.value_type())),
    }
}

pub fn cdr(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    match &args[0] {
        Value::List(l) => {
            if l.is_empty() {
                return Ok(Value::Nil);
            }
            let mut rest = l.clone();
            rest.pop_front();
            if rest.is_empty() {
                Ok(Value::Nil) // (cdr '(x)) → nil
            } else {
                Ok(Value::List(rest))
            }
        }
        Value::Nil => Ok(Value::Nil), // (cdr nil) → nil
        other => Err(EvalError::type_error("list", other.value_type())),
    }
}

pub fn list(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    Ok(Value::List(args))
}

// ── Sequence Operations ────────────────────────────────────────────

pub fn map_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let func = args[0].clone();
    let coll = args[1].clone();
    let items = match &coll {
        Value::List(v) => v,
        Value::Vector(v) => v,
        other => return Err(EvalError::type_error("sequential", other.value_type())),
    };
    let mut result = Vector::new();
    for item in items {
        let mut r = crate::eval::apply(func.clone(), vector![item.clone()], engine)?;
        let v = loop {
            match r {
                TailResult::Value(v) => break v,
                TailResult::TailCall(f, a) => r = crate::eval::apply(f, a, engine)?,
                _ => return Err(EvalError::custom("unexpected recur in map")),
            }
        };
        result.push_back(v);
    }
    Ok(Value::List(result))
}

pub fn filter_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let pred = args[0].clone();
    let coll = args[1].clone();
    let items = match &coll {
        Value::List(v) => v,
        Value::Vector(v) => v,
        other => return Err(EvalError::type_error("sequential", other.value_type())),
    };
    let mut result = Vector::new();
    for item in items {
        let mut r = crate::eval::apply(pred.clone(), vector![item.clone()], engine)?;
        let v = loop {
            match r {
                TailResult::Value(v) => break v,
                TailResult::TailCall(f, a) => r = crate::eval::apply(f, a, engine)?,
                _ => return Err(EvalError::custom("unexpected recur in filter")),
            }
        };
        if is_truthy(&v) {
            result.push_back(item.clone());
        }
    }
    Ok(Value::List(result))
}

pub fn reduce_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::wrong_arg_count(3, args.len()));
    }
    let func = args[0].clone();
    let init = args[1].clone();
    let coll = args[2].clone();
    let items = match &coll {
        Value::List(v) => v,
        Value::Vector(v) => v,
        other => return Err(EvalError::type_error("sequential", other.value_type())),
    };
    let mut acc = init;
    for item in items {
        let mut r = crate::eval::apply(func.clone(), vector![acc, item.clone()], engine)?;
        acc = loop {
            match r {
                TailResult::Value(v) => break v,
                TailResult::TailCall(f, a) => r = crate::eval::apply(f, a, engine)?,
                _ => return Err(EvalError::custom("unexpected recur in reduce")),
            }
        };
    }
    Ok(acc)
}

// ── apply ───────────────────────────────────────────────────────────

pub fn apply_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.is_empty() || args.len() > 2 {
        return Err(EvalError::wrong_arg_count_range(1, 2, args.len()));
    }
    let func = args[0].clone();
    let arg_list = if args.len() == 2 {
        match &args[1] {
            Value::List(v) => v.clone(),
            Value::Vector(v) => v.clone(),
            other => return Err(EvalError::type_error("list or vector", other.value_type())),
        }
    } else {
        Vector::new()
    };
    let mut result = crate::eval::apply(func, arg_list, engine)?;
    loop {
        match result {
            TailResult::Value(v) => return Ok(v),
            TailResult::TailCall(f, a) => result = crate::eval::apply(f, a, engine)?,
            _ => return Err(EvalError::custom("unexpected recur in apply")),
        }
    }
}

// ── Collection Access ──────────────────────────────────────────────

pub fn get_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::wrong_arg_count_range(2, 3, args.len()));
    }
    let coll = &args[0];
    let key = &args[1];
    let default = if args.len() == 3 {
        Some(&args[2])
    } else {
        None
    };
    match coll {
        Value::List(v) | Value::Vector(v) => {
            let idx = match key {
                Value::Integer(i) => *i as usize,
                _ => return Err(EvalError::type_error("integer index", key.value_type())),
            };
            if let Some(val) = v.get(idx) {
                Ok(val.clone())
            } else {
                default.cloned().ok_or_else(|| EvalError::index_out_of_bounds(idx, v.len()))
            }
        }
        Value::Map(m) => {
            if let Some(val) = m.get(key) {
                Ok(val.clone())
            } else {
                default.cloned().ok_or_else(|| {
                    EvalError::custom(format!("key not found in map: {key}"))
                })
            }
        }
        other => Err(EvalError::type_error("sequential or map", other.value_type())),
    }
}

pub fn count_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let result = match &args[0] {
        Value::List(v) | Value::Vector(v) => v.len() as i64,
        Value::Map(m) => m.len() as i64,
        Value::String(s) => s.chars().count() as i64,
        other => return Err(EvalError::type_error("countable", other.value_type())),
    };
    Ok(Value::Integer(result))
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
    };
    Ok(Value::Keyword(kw.to_string()))
}

// ── Arithmetic Extras ──────────────────────────────────────────────

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


// ── Map Operations ────────────────────────────────────────────────

/// (put map key val) → new map — assoc key-value.
pub fn put_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::wrong_arg_count(3, args.len()));
    }
    let m = match &args[0] {
        Value::Map(m) => m.clone(),
        other => return Err(EvalError::type_error("map", other.value_type())),
    };
    Ok(Value::Map(m.update(args[1].clone(), args[2].clone())))
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

/// (vector ...) → vector — create a vector from arguments.
pub fn vector_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    Ok(Value::Vector(args))
}
// ── I/O ────────────────────────────────────────────────────────────
pub fn println(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let s: String = args
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{s}");
    Ok(Value::Nil)
}

pub fn prn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let s: String = args
        .iter()
        .map(|v| format!("{v:?}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{s}");
    Ok(Value::Nil)
}

pub fn read_line(_args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) | Err(_) => Ok(Value::Nil),
        Ok(_) => {
            // Trim trailing newline
            let trimmed = input.trim_end_matches('\n').trim_end_matches('\r');
            Ok(Value::String(trimmed.to_string()))
        }
    }
}

// ── File I/O ──────────────────────────────────────────────────────

/// (slurp path) → string — read entire file into a string.
pub fn slurp(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => Ok(Value::String(content)),
        Err(e) => Err(EvalError::custom(format!("slurp error: {e}"))),
    }
}

/// (spit path content) → nil — write string to a file.
pub fn spit(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let path = match &args[0] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    let content = match &args[1] {
        Value::String(s) => s.clone(),
        other => return Err(EvalError::type_error("string",
        &format!("{}", other),)),
    };
    match std::fs::write(&path, &content) {
        Ok(()) => Ok(Value::Nil),
        Err(e) => Err(EvalError::custom(format!("spit error: {e}"))),
    }
}

// ── String Operations ─────────────────────────────────────────────

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

// ── Macroexpand ────────────────────────────────────────────────────
pub fn macroexpand_1_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    // Convert the Value argument back to Sexp for macro expansion
    let sexp = macros::value_to_sexp(&args[0])?;

    match &sexp {
        Sexp::List(list, _) if !list.is_empty() => {
            if let Sexp::Symbol(name, _) = &list[0] {
                let macro_args: Vec<Sexp> = list.iter().skip(1).cloned().collect();
                let env = engine.env();
                if let Some(expanded) = macros::try_expand_by_name(name, &macro_args, env, engine)? {
                    return Ok(Value::from(expanded));
                }
            }
            Ok(args[0].clone())
        }
        _ => Ok(args[0].clone()),
    }
}

/// Fully expand a form: recursively expand until no more macro calls remain.
pub fn macroexpand_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }

    let mut current = macros::value_to_sexp(&args[0])?;
    let env = engine.env();

    loop {
        let next = match &current {
            Sexp::List(list, _) if !list.is_empty() => {
                if let Sexp::Symbol(name, _) = &list[0] {
                    let macro_args: Vec<Sexp> = list.iter().skip(1).cloned().collect();
                    macros::try_expand_by_name(name, &macro_args, env, engine)?
                } else {
                    None
                }
            }
            _ => None,
        };
        match next {
            Some(expanded) => current = expanded,
            None => break,
        }
    }

    Ok(Value::from(current))
}

// ── Registration ───────────────────────────────────────────────────

pub fn setup_env(env: &Arc<Env>) {
    // Arithmetic
    env.set("+".into(), Value::NativeFunction(NativeFn::new("+", add)));
    env.set("-".into(), Value::NativeFunction(NativeFn::new("-", sub)));
    env.set("*".into(), Value::NativeFunction(NativeFn::new("*", mul)));
    env.set("/".into(), Value::NativeFunction(NativeFn::new("/", div)));

    // Comparison
    env.set("=".into(), Value::NativeFunction(NativeFn::new("=", eq)));
    env.set("<".into(), Value::NativeFunction(NativeFn::new("<", lt)));
    env.set(">".into(), Value::NativeFunction(NativeFn::new(">", gt)));
    env.set("<=".into(), Value::NativeFunction(NativeFn::new("<=", le)));
    env.set(">=".into(), Value::NativeFunction(NativeFn::new(">=", ge)));

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

    // Cons / List
    env.set("cons".into(), Value::NativeFunction(NativeFn::new("cons", cons)));
    env.set("car".into(), Value::NativeFunction(NativeFn::new("car", car)));
    env.set("cdr".into(), Value::NativeFunction(NativeFn::new("cdr", cdr)));
    env.set("list".into(), Value::NativeFunction(NativeFn::new("list", list)));

    // Sequence operations
    env.set("map".into(), Value::NativeFunction(NativeFn::new("map", map_fn)));
    env.set("filter".into(), Value::NativeFunction(NativeFn::new("filter", filter_fn)));
    env.set("reduce".into(), Value::NativeFunction(NativeFn::new("reduce", reduce_fn)));


    // New builtins (v0.2 additions)
    env.set("apply".into(), Value::NativeFunction(NativeFn::new("apply", apply_fn)));
    env.set("get".into(), Value::NativeFunction(NativeFn::new("get", get_fn)));
    env.set("count".into(), Value::NativeFunction(NativeFn::new("count", count_fn)));
    env.set("type".into(), Value::NativeFunction(NativeFn::new("type", type_fn)));
    env.set("mod".into(), Value::NativeFunction(NativeFn::new("mod", mod_fn)));
    env.set("char->integer".into(), Value::NativeFunction(NativeFn::new("char->integer", char_to_integer)));
    env.set("integer->char".into(), Value::NativeFunction(NativeFn::new("integer->char", integer_to_char)));
    env.set("char=?".into(), Value::NativeFunction(NativeFn::new("char=?", char_eq)));

    // Map operations
    env.set("put".into(), Value::NativeFunction(NativeFn::new("put", put_fn)));

    // Type conversion
    env.set("str".into(), Value::NativeFunction(NativeFn::new("str", str_fn)));
    env.set("keyword".into(), Value::NativeFunction(NativeFn::new("keyword", keyword_fn)));
    env.set("symbol".into(), Value::NativeFunction(NativeFn::new("symbol", symbol_fn)));
    env.set("vector".into(), Value::NativeFunction(NativeFn::new("vector", vector_fn)));

    // Macroexpand
    env.set("macroexpand-1".into(), Value::NativeFunction(NativeFn::new("macroexpand-1", macroexpand_1_fn)));
    env.set("macroexpand".into(), Value::NativeFunction(NativeFn::new("macroexpand", macroexpand_fn)));
    // I/O
    env.set("println".into(), Value::NativeFunction(NativeFn::new("println", println)));
    env.set("prn".into(), Value::NativeFunction(NativeFn::new("prn", prn)));
    env.set("read-line".into(), Value::NativeFunction(NativeFn::new("read-line", read_line)));
    env.set("slurp".into(), Value::NativeFunction(NativeFn::new("slurp", slurp)));
    env.set("spit".into(), Value::NativeFunction(NativeFn::new("spit", spit)));

    // String operations
    env.set("str-join".into(), Value::NativeFunction(NativeFn::new("str-join", str_join)));
    env.set("str-split".into(), Value::NativeFunction(NativeFn::new("str-split", str_split)));
    env.set("str-trim".into(), Value::NativeFunction(NativeFn::new("str-trim", str_trim)));
    env.set("str-contains?".into(), Value::NativeFunction(NativeFn::new("str-contains?", str_contains)));
    env.set("str-starts-with?".into(), Value::NativeFunction(NativeFn::new("str-starts-with?", str_starts_with)));
    env.set("str-ends-with?".into(), Value::NativeFunction(NativeFn::new("str-ends-with?", str_ends_with)));
}

pub use crate::value::NativeFn;