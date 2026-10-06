//! Arithmetic and comparison builtins.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::value::{NativeFn, Value};

/// A numeric argument, kept in whichever type it was written as. The
/// fold below holds one of these, so an all-integer call never allocates a
/// float and a mixed call widens once instead of per operand.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn of(v: &Value) -> Result<Self, EvalError> {
        match v {
            Value::Integer(i) => Ok(Num::Int(*i)),
            Value::Float(f) => Ok(Num::Float(*f)),
            other => Err(EvalError::type_error("number", other.value_type())),
        }
    }
}

/// Left-fold numeric arguments. Integer/Integer stays integral, so
/// `(+ 9007199254740993 1)` is 9007199254740994, not 9007199254740992.
fn fold<F>(args: &Vector<Value>, op: F) -> Result<Value, EvalError>
where
    F: Fn(Num, Num) -> Result<Num, EvalError>,
{
    let first = args.front().ok_or_else(|| EvalError::wrong_arg_count_min(1, 0))?;
    let mut acc = Num::of(first)?;
    for arg in args.iter().skip(1) {
        acc = op(acc, Num::of(arg)?)?;
    }
    Ok(match acc {
        Num::Int(i) => Value::Integer(i),
        Num::Float(f) => Value::Float(f),
    })
}

impl Num {
    fn as_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

pub fn add(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    fold(&args, |a, b| match (a, b) {
        (Num::Int(x), Num::Int(y)) => Ok(Num::Int(x.wrapping_add(y))),
        (x, y) => Ok(Num::Float(x.as_f64() + y.as_f64())),
    })
}

pub fn mul(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    fold(&args, |a, b| match (a, b) {
        (Num::Int(x), Num::Int(y)) => Ok(Num::Int(x.wrapping_mul(y))),
        (x, y) => Ok(Num::Float(x.as_f64() * y.as_f64())),
    })
}

pub fn sub(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    // Unary minus keeps the operand's own type.
    if args.len() == 1 {
        return match Num::of(&args[0])? {
            Num::Int(i) => Ok(Value::Integer(-i)),
            Num::Float(f) => Ok(Value::Float(-f)),
        };
    }
    fold(&args, |a, b| match (a, b) {
        (Num::Int(x), Num::Int(y)) => Ok(Num::Int(x.wrapping_sub(y))),
        (x, y) => Ok(Num::Float(x.as_f64() - y.as_f64())),
    })
}

pub fn div(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() == 1 {
        let operand = Num::of(args.front().expect("length checked"))?;
        if let Num::Int(0) = operand {
            return Err(EvalError::division_by_zero());
        }
        // Unary division always widens: (/ 1) is 1.0.
        return Ok(Value::Float(1.0 / operand.as_f64()));
    }
    fold(&args, |a, b| match (a, b) {
        (Num::Int(_), Num::Int(0)) => Err(EvalError::division_by_zero()),
        // Integer division truncates — existing contract, unchanged.
        (Num::Int(x), Num::Int(y)) => Ok(Num::Int(x.wrapping_div(y))),
        (Num::Float(_), Num::Float(0.0)) => Err(EvalError::division_by_zero()),
        (x, y) => {
            let rhs = y.as_f64();
            if rhs == 0.0 {
                return Err(EvalError::division_by_zero());
            }
            Ok(Num::Float(x.as_f64() / rhs))
        }
    })
}

/// (sqrt x) → square root as a Float. A plain scalar numeric primitive:
/// one finite, non-negative argument, no shape or tensor argument.
pub fn sqrt(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let input = match &args[0] {
        Value::Integer(i) => *i as f64,
        Value::Float(f) => *f,
        other => return Err(EvalError::type_error("number", other.value_type())),
    };
    if !input.is_finite() {
        return Err(EvalError::custom("sqrt requires a finite number"));
    }
    if input < 0.0 {
        return Err(EvalError::custom("sqrt requires a non-negative number"));
    }
    Ok(Value::Float(input.sqrt()))
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

/// The total order `a b` satisfies, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Order {
    Less,
    Equal,
    Greater,
}

impl Order {
    fn from_cmp(a: f64, b: f64) -> Self {
        if a < b {
            Order::Less
        } else if a > b {
            Order::Greater
        } else {
            Order::Equal
        }
    }
}

/// Compare two numbers. Integer/Integer compares exactly; anything with a
/// Float widens — after checking the f64 side is finite, so an infinity
/// produced by `(/ 0.0)` or an overflow can never be ordered silently.
///
/// The mixed case goes through i128 rather than straight to f64:
/// `9007199254740993` and `9007199254740992.0` are different values, and
/// rounding both to f64 would make them compare equal.
fn compare(a: &Value, b: &Value) -> Result<Order, EvalError> {
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => Ok(match x.cmp(y) {
            std::cmp::Ordering::Less => Order::Less,
            std::cmp::Ordering::Equal => Order::Equal,
            std::cmp::Ordering::Greater => Order::Greater,
        }),
        (Value::Float(_), Value::Integer(_)) => compare(b, a).map(|order| match order {
            Order::Less => Order::Greater,
            Order::Equal => Order::Equal,
            Order::Greater => Order::Less,
        }),
        (Value::Integer(x), Value::Float(y)) => {
            if !y.is_finite() {
                return Err(EvalError::custom(
                    "cannot order against a non-finite number; use a finite number",
                ));
            }
            // Never convert `x` to f64. Any float at or beyond 2^53 is
            // exactly an integer, so those compare exactly too; below 2^53
            // the float's fractional part decides the tie.
            let y = *y;
            let magnitude = y.abs();
            if magnitude >= 9007199254740992.0 {
                // -2^63 is an i64; +2^63 and floats below -2^63 are not.
                if y >= 9223372036854775808.0 || y < -9223372036854775808.0 {
                    return Ok(if y > 0.0 { Order::Less } else { Order::Greater });
                }
                return Ok(match (*x as i128).cmp(&(y as i128)) {
                    std::cmp::Ordering::Less => Order::Less,
                    std::cmp::Ordering::Greater => Order::Greater,
                    std::cmp::Ordering::Equal => Order::Equal,
                });
            }
            let floor = y.floor();
            match (*x as i128).cmp(&(floor as i128)) {
                std::cmp::Ordering::Equal => Ok(if y > floor { Order::Less } else { Order::Equal }),
                std::cmp::Ordering::Less => Ok(Order::Less),
                std::cmp::Ordering::Greater => Ok(Order::Greater),
            }
        }
        (Value::Float(x), Value::Float(y)) => {
            if !x.is_finite() || !y.is_finite() {
                return Err(EvalError::custom(
                    "cannot order a non-finite number; use a finite number",
                ));
            }
            Ok(Order::from_cmp(*x, *y))
        }
        (other, _) => Err(EvalError::type_error("number", other.value_type())),
    }
}

/// N-ary ordering: `(< 1 2 3)` asserts every consecutive pair. Same shape
/// as arithmetic folding, so the old two-argument calls are unchanged.
fn cmp<F>(args: &Vector<Value>, op: F) -> Result<Value, EvalError>
where
    F: Fn(Order) -> bool,
{
    if args.len() < 2 {
        return Err(EvalError::wrong_arg_count_min(2, args.len()));
    }
    for pair in args.iter().zip(args.iter().skip(1)) {
        if !op(compare(pair.0, pair.1)?) {
            return Ok(Value::Boolean(false));
        }
    }
    Ok(Value::Boolean(true))
}

pub fn lt(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp(&args, |o| o == Order::Less)
}

pub fn gt(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp(&args, |o| o == Order::Greater)
}

pub fn le(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp(&args, |o| o != Order::Greater)
}

pub fn ge(args: Vector<Value>, _engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    cmp(&args, |o| o != Order::Less)
}

pub fn register(env: &Arc<Env>) {
    // Arithmetic
    env.set("+".into(), Value::NativeFunction(NativeFn::new("+", add)));
    env.set("-".into(), Value::NativeFunction(NativeFn::new("-", sub)));
    env.set("*".into(), Value::NativeFunction(NativeFn::new("*", mul)));
    env.set("/".into(), Value::NativeFunction(NativeFn::new("/", div)));
    env.set("mod".into(), Value::NativeFunction(NativeFn::new("mod", mod_fn)));
    env.set("sqrt".into(), Value::NativeFunction(NativeFn::new("sqrt", sqrt)));

    // Comparison
    env.set("=".into(), Value::NativeFunction(NativeFn::new("=", eq)));
    env.set("<".into(), Value::NativeFunction(NativeFn::new("<", lt)));
    env.set(">".into(), Value::NativeFunction(NativeFn::new(">", gt)));
    env.set("<=".into(), Value::NativeFunction(NativeFn::new("<=", le)));
    env.set(">=".into(), Value::NativeFunction(NativeFn::new(">=", ge)));
}
