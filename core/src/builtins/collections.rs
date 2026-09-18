//! List, vector, and map collection builtins.

use std::sync::Arc;

use im::Vector;
use im::vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::special::TailResult;
use crate::value::{is_truthy, NativeFn, Value};

// ── Cons / List Operations ─────────────────────────────────────────

pub fn cons(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    // nil is the empty list: (cons x nil) → (x)
    let mut list = match &args[1] {
        Value::List(l) => l.clone(),
        Value::Nil => Vector::new(),
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

/// (keys map) → vector of keys. Iteration order is not defined.
pub fn keys_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let m = match &args[0] {
        Value::Map(m) => m,
        other => return Err(EvalError::type_error("map", other.value_type())),
    };
    Ok(Value::Vector(m.keys().cloned().collect()))
}

/// (vals map) → vector of values. Iteration order is not defined.
pub fn vals_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let m = match &args[0] {
        Value::Map(m) => m,
        other => return Err(EvalError::type_error("map", other.value_type())),
    };
    Ok(Value::Vector(m.values().cloned().collect()))
}

/// (dissoc map key ...) → new map without the given keys.
pub fn dissoc_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() < 1 {
        return Err(EvalError::wrong_arg_count_min(1, args.len()));
    }
    let mut m = match &args[0] {
        Value::Map(m) => m.clone(),
        other => return Err(EvalError::type_error("map", other.value_type())),
    };
    for key in args.iter().skip(1) {
        m = m.without(key);
    }
    Ok(Value::Map(m))
}

// ── Vector Operations ─────────────────────────────────────────────

/// (vector ...) → vector — create a vector from arguments.
pub fn vector_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    Ok(Value::Vector(args))
}

pub fn vector_conj_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    match &args[0] {
        Value::Vector(v) => {
            let mut new_v = v.clone();
            new_v.push_back(args[1].clone());
            Ok(Value::Vector(new_v))
        }
        other => Err(EvalError::type_error("vector", other.value_type())),
    }
}

// ── Sequence Generation ───────────────────────────────────────────

/// (range end) → vector [0 1 ... end-1]
/// (range start end) → vector [start ... end-1]
pub fn range_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    let (start, end) = match args.len() {
        1 => {
            let e = match &args[0] {
                Value::Integer(i) => *i,
                other => return Err(EvalError::type_error("integer", other.value_type())),
            };
            (0i64, e)
        }
        2 => {
            let s = match &args[0] {
                Value::Integer(i) => *i,
                other => return Err(EvalError::type_error("integer", other.value_type())),
            };
            let e = match &args[1] {
                Value::Integer(i) => *i,
                other => return Err(EvalError::type_error("integer", other.value_type())),
            };
            (s, e)
        }
        _ => return Err(EvalError::wrong_arg_count_range(1, 2, args.len())),
    };
    let mut v = Vector::new();
    // Limit to reasonable size to prevent OOM
    let count = (end - start).max(0).min(10_000_000);
    for i in start..start + count {
        v.push_back(Value::Integer(i));
    }
    Ok(Value::Vector(v))
}

// ── Slicing ────────────────────────────────────────────────────────

/// (slice coll start end) → sub-collection of the same kind.
/// start inclusive, end exclusive; both clamped to [0, count].
pub fn slice_fn(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::wrong_arg_count(3, args.len()));
    }
    let start = match &args[1] {
        Value::Integer(i) => (*i).max(0) as usize,
        other => return Err(EvalError::type_error("integer start", other.value_type())),
    };
    let end = match &args[2] {
        Value::Integer(i) => (*i).max(0) as usize,
        other => return Err(EvalError::type_error("integer end", other.value_type())),
    };
    match &args[0] {
        Value::Vector(v) => {
            let s = start.min(v.len());
            let e = end.min(v.len()).max(s);
            Ok(Value::Vector(v.clone().into_iter().skip(s).take(e - s).collect()))
        }
        Value::List(l) => {
            let s = start.min(l.len());
            let e = end.min(l.len()).max(s);
            Ok(Value::List(l.clone().into_iter().skip(s).take(e - s).collect()))
        }
        other => Err(EvalError::type_error("sequential", other.value_type())),
    }
}

// ── Sorting ────────────────────────────────────────────────────────

/// (sort cmp coll) → sorted vector — stable sort using comparator.
/// Comparator is a function (fn [a b] ...) returning boolean (true if a < b).
pub fn sort_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let cmp = args[0].clone();
    let coll = match &args[1] {
        Value::List(v) | Value::Vector(v) => v.clone(),
        other => return Err(EvalError::type_error("sequential", other.value_type())),
    };
    let mut items: Vec<Value> = coll.into_iter().collect();
    // Insertion sort — stable, O(n²) but fine for typical collection sizes.
    // The comparator may be a user function whose body is a tail call; its
    // TailResult must be trampolined, not dropped (same hazard as and/or).
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 {
            let b = items[j].clone();
            let a = items[j - 1].clone();
            let args_vec = vector![b, a];
            let mut r = crate::eval::apply(cmp.clone(), args_vec, engine)?;
            let verdict = loop {
                match r {
                    TailResult::Value(v) => break v,
                    TailResult::TailCall(f, a) => r = crate::eval::apply(f, a, engine)?,
                    _ => return Err(EvalError::custom("unexpected recur in sort")),
                }
            };
            if is_truthy(&verdict) {
                items.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
    Ok(Value::Vector(items.into_iter().collect()))
}

pub fn register(env: &Arc<Env>) {
    // Cons / List
    env.set("cons".into(), Value::NativeFunction(NativeFn::new("cons", cons)));
    env.set("car".into(), Value::NativeFunction(NativeFn::new("car", car)));
    env.set("cdr".into(), Value::NativeFunction(NativeFn::new("cdr", cdr)));
    env.set("list".into(), Value::NativeFunction(NativeFn::new("list", list)));

    // Sequence operations
    env.set("map".into(), Value::NativeFunction(NativeFn::new("map", map_fn)));
    env.set("filter".into(), Value::NativeFunction(NativeFn::new("filter", filter_fn)));
    env.set("reduce".into(), Value::NativeFunction(NativeFn::new("reduce", reduce_fn)));
    env.set("apply".into(), Value::NativeFunction(NativeFn::new("apply", apply_fn)));
    env.set("get".into(), Value::NativeFunction(NativeFn::new("get", get_fn)));
    env.set("count".into(), Value::NativeFunction(NativeFn::new("count", count_fn)));

// ── Map operations ─────────────────────────────────────────────────
    env.set("put".into(), Value::NativeFunction(NativeFn::new("put", put_fn)));
    env.set("keys".into(), Value::NativeFunction(NativeFn::new("keys", keys_fn)));
    env.set("vals".into(), Value::NativeFunction(NativeFn::new("vals", vals_fn)));
    env.set("dissoc".into(), Value::NativeFunction(NativeFn::new("dissoc", dissoc_fn)));

    // Vector operations
    env.set("vector".into(), Value::NativeFunction(NativeFn::new("vector", vector_fn)));
    env.set("vector-conj".into(), Value::NativeFunction(NativeFn::new("vector-conj", vector_conj_fn)));

    // Sequence generation and sorting
    env.set("range".into(), Value::NativeFunction(NativeFn::new("range", range_fn)));
    env.set("slice".into(), Value::NativeFunction(NativeFn::new("slice", slice_fn)));
    env.set("sort".into(), Value::NativeFunction(NativeFn::new("sort", sort_fn)));
}
