use std::io::Write;
use std::sync::Arc;

use im::{vector, Vector};

use crate::env::Env;
use crate::error::EvalError;
use crate::eval;
use crate::value::{is_truthy, Value};

// ── Arithmetic ─────────────────────────────────────────────────────

/// Helper: run an integer op or float op depending on args.
/// If any arg is Float, all are cast to f64 and the float op is used.
fn with_numeric<F, G>(args: &Vector<Value>, int_op: F, float_op: G) -> Result<Value, EvalError>
where
    F: Fn(&[i64]) -> i64,
    G: Fn(&[f64]) -> f64,
{
    let has_float = args.iter().any(|v| matches!(v, Value::Float(_)));
    if has_float {
        let mut nums = Vec::with_capacity(args.len());
        for arg in args {
            match arg {
                Value::Integer(i) => nums.push(*i as f64),
                Value::Float(f) => nums.push(*f),
                other => return Err(EvalError::type_error("number", other.value_type())),
            }
        }
        Ok(Value::Float(float_op(&nums)))
    } else {
        let mut nums = Vec::with_capacity(args.len());
        for arg in args {
            match arg {
                Value::Integer(i) => nums.push(*i),
                other => return Err(EvalError::type_error("integer", other.value_type())),
            }
        }
        Ok(Value::Integer(int_op(&nums)))
    }
}

pub fn add(args: Vector<Value>) -> Result<Value, EvalError> {
    with_numeric(&args,
        |nums| nums.iter().sum(),
        |nums| nums.iter().sum(),
    )
}

pub fn sub(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::WrongArgCountMin { min: 1, got: 0 });
    }
    with_numeric(&args,
        |nums| {
            if nums.len() == 1 { -nums[0] } else {
                let mut r = nums[0];
                for n in &nums[1..] { r -= n; }
                r
            }
        },
        |nums| {
            if nums.len() == 1 { -nums[0] } else {
                let mut r = nums[0];
                for n in &nums[1..] { r -= n; }
                r
            }
        },
    )
}

pub fn mul(args: Vector<Value>) -> Result<Value, EvalError> {
    with_numeric(&args,
        |nums| nums.iter().product(),
        |nums| nums.iter().product(),
    )
}

pub fn div(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.is_empty() {
        return Err(EvalError::WrongArgCountMin { min: 1, got: 0 });
    }
    let has_float = args.iter().any(|v| matches!(v, Value::Float(_)));
    if has_float {
        let first = match &args[0] {
            Value::Integer(i) => *i as f64,
            Value::Float(f) => *f,
            other => return Err(EvalError::type_error("number", other.value_type())),
        };
        if args.len() == 1 {
            return Ok(Value::Float(1.0 / first));
        }
        let mut res = first;
        for arg in args.iter().skip(1) {
            match arg {
                Value::Integer(i) => {
                    if *i == 0 { return Err(EvalError::DivisionByZero); }
                    res /= *i as f64;
                }
                Value::Float(f) => {
                    if *f == 0.0 { return Err(EvalError::DivisionByZero); }
                    res /= f;
                }
                other => return Err(EvalError::type_error("number", other.value_type())),
            }
        }
        Ok(Value::Float(res))
    } else {
        let first = match &args[0] {
            Value::Integer(i) => *i,
            other => return Err(EvalError::type_error("integer", other.value_type())),
        };
        if args.len() == 1 {
            return Ok(Value::Integer(1 / first));
        }
        let mut res = first;
        for arg in args.iter().skip(1) {
            match arg {
                Value::Integer(i) => {
                    if *i == 0 { return Err(EvalError::DivisionByZero); }
                    res /= i;
                }
                other => return Err(EvalError::type_error("integer", other.value_type())),
            }
        }
        Ok(Value::Integer(res))
    }
}

// ── Comparison ─────────────────────────────────────────────────────

pub fn eq(args: Vector<Value>) -> Result<Value, EvalError> {
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

fn cmp_int<F>(args: &Vector<Value>, op: F) -> Result<Value, EvalError>
where
    F: Fn(i64, i64) -> bool,
{
    if args.len() < 2 {
        return Ok(Value::Boolean(true));
    }
    for i in 0..args.len() - 1 {
        match (&args[i], &args[i + 1]) {
            (Value::Integer(a), Value::Integer(b)) => {
                if !op(*a, *b) {
                    return Ok(Value::Boolean(false));
                }
            }
            (a, b) => {
                return Err(EvalError::TypeError {
                    expected: "integer",
                    got: format!("{} and {}", a.value_type(), b.value_type()),
                })
            }
        }
    }
    Ok(Value::Boolean(true))
}

pub fn lt(args: Vector<Value>) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a < b)
}

pub fn gt(args: Vector<Value>) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a > b)
}

pub fn le(args: Vector<Value>) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a <= b)
}

pub fn ge(args: Vector<Value>) -> Result<Value, EvalError> {
    cmp_int(&args, |a, b| a >= b)
}

// ── Type Predicates ────────────────────────────────────────────────

fn unary_pred<P>(args: &Vector<Value>, pred: P) -> Result<Value, EvalError>
where
    P: Fn(&Value) -> bool,
{
    if args.len() != 1 {
        return Err(EvalError::WrongArgCount {
            expected: 1,
            got: args.len(),
        });
    }
    Ok(Value::Boolean(pred(&args[0])))
}

pub fn is_nil(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Nil))
}

pub fn is_boolean(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Boolean(_)))
}

pub fn is_number(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Integer(_) | Value::Float(_)))
}

pub fn is_string(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::String(_)))
}

pub fn is_symbol(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Symbol(_)))
}

pub fn is_keyword(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Keyword(_)))
}

pub fn is_list(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::List(_)))
}

pub fn is_vector(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Vector(_)))
}

pub fn is_map(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| matches!(v, Value::Map(_)))
}

pub fn is_fn(args: Vector<Value>) -> Result<Value, EvalError> {
    unary_pred(&args, |v| {
        matches!(v, Value::Function(_) | Value::NativeFunction(_) | Value::Macro(_))
    })
}

// ── Cons / List Operations ─────────────────────────────────────────

pub fn cons(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::WrongArgCount {
            expected: 2,
            got: args.len(),
        });
    }
    let x = args[0].clone();
    let y = args[1].clone();
    match y {
        Value::List(v) => {
            let mut new = v;
            new.push_front(x);
            Ok(Value::List(new))
        }
        _ => Ok(Value::List(vector![x, y])),
    }
}

pub fn car(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::WrongArgCount {
            expected: 1,
            got: args.len(),
        });
    }
    match &args[0] {
        Value::List(v) => v
            .front()
            .cloned()
            .ok_or_else(|| EvalError::InvalidForm("car of empty list".into())),
        other => Err(EvalError::type_error("list", other.value_type())),
    }
}

pub fn cdr(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::WrongArgCount {
            expected: 1,
            got: args.len(),
        });
    }
    match &args[0] {
        Value::List(v) => {
            let mut rest = v.clone();
            rest.pop_front();
            Ok(Value::List(rest))
        }
        other => Err(EvalError::type_error("list", other.value_type())),
    }
}

pub fn list(args: Vector<Value>) -> Result<Value, EvalError> {
    Ok(Value::List(args))
}

// ── Sequence Operations ────────────────────────────────────────────

pub fn map_fn(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::WrongArgCount {
            expected: 2,
            got: args.len(),
        });
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
        let r = eval::apply(func.clone(), vector![item.clone()])?;
        result.push_back(match r {
            crate::special::TailResult::Value(v) => v,
            _ => return Err(EvalError::Custom("unexpected recur in map".into())),
        });
    }
    Ok(Value::List(result))
}

pub fn filter_fn(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::WrongArgCount {
            expected: 2,
            got: args.len(),
        });
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
        let r = eval::apply(pred.clone(), vector![item.clone()])?;
        let v = match r {
            crate::special::TailResult::Value(v) => v,
            _ => return Err(EvalError::Custom("unexpected recur in filter".into())),
        };
        if is_truthy(&v) {
            result.push_back(item.clone());
        }
    }
    Ok(Value::List(result))
}

pub fn reduce_fn(args: Vector<Value>) -> Result<Value, EvalError> {
    if args.len() != 3 {
        return Err(EvalError::WrongArgCount {
            expected: 3,
            got: args.len(),
        });
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
        let r = eval::apply(func.clone(), vector![acc, item.clone()])?;
        acc = match r {
            crate::special::TailResult::Value(v) => v,
            _ => return Err(EvalError::Custom("unexpected recur in reduce".into())),
        };
    }
    Ok(acc)
}

// ── I/O ────────────────────────────────────────────────────────────

pub fn println(args: Vector<Value>) -> Result<Value, EvalError> {
    let s: String = args
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(" ");
    println!("{s}");
    Ok(Value::Nil)
}

pub fn prn(args: Vector<Value>) -> Result<Value, EvalError> {
    let s: String = args
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(" ");
    print!("{s}");
    std::io::stdout().flush().ok();
    Ok(Value::Nil)
}

pub fn read_line(args: Vector<Value>) -> Result<Value, EvalError> {
    if !args.is_empty() {
        let s: String = args
            .iter()
            .map(|v| format!("{v}"))
            .collect::<Vec<_>>()
            .join(" ");
        print!("{s}");
        std::io::stdout().flush().ok();
    }
    let mut input = String::new();
    match std::io::stdin().read_line(&mut input) {
        Ok(0) => Ok(Value::Nil),
        Ok(_) => {
            if input.ends_with('\n') {
                input.pop();
            }
            Ok(Value::String(input))
        }
        Err(e) => Err(EvalError::Custom(format!("IO error: {e}"))),
    }
}

// ── Macroexpand ────────────────────────────────────────────────────

pub fn macroexpand_fn(_args: Vector<Value>) -> Result<Value, EvalError> {
    Err(EvalError::Custom(
        "macroexpand is not available as a native function; \
         use it directly in the REPL as a special form"
            .into(),
    ))
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

    // Cons / List
    env.set("cons".into(), Value::NativeFunction(NativeFn::new("cons", cons)));
    env.set("car".into(), Value::NativeFunction(NativeFn::new("car", car)));
    env.set("cdr".into(), Value::NativeFunction(NativeFn::new("cdr", cdr)));
    env.set("list".into(), Value::NativeFunction(NativeFn::new("list", list)));

    // Sequence operations
    env.set("map".into(), Value::NativeFunction(NativeFn::new("map", map_fn)));
    env.set("filter".into(), Value::NativeFunction(NativeFn::new("filter", filter_fn)));
    env.set("reduce".into(), Value::NativeFunction(NativeFn::new("reduce", reduce_fn)));

    // I/O
    env.set("println".into(), Value::NativeFunction(NativeFn::new("println", println)));
    env.set("prn".into(), Value::NativeFunction(NativeFn::new("prn", prn)));
    env.set("read-line".into(), Value::NativeFunction(NativeFn::new("read-line", read_line)));

    // Macro
    env.set("macroexpand".into(), Value::NativeFunction(NativeFn::new("macroexpand", macroexpand_fn)));
}

pub use crate::value::NativeFn;
