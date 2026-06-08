use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{EvalFn, TailResult};
use crate::value::{is_truthy, Value};

// ── if ────────────────────────────────────────────────────────────

pub fn do_if<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::WrongArgCountMin {
            min: 2,
            got: args.len(),
        });
    }
    let cond = eval_fn(&args[0], env, false)?.into_value();
    if is_truthy(&cond) {
        eval_fn(&args[1], env, tail)
    } else if args.len() == 3 {
        eval_fn(&args[2], env, tail)
    } else {
        Ok(TailResult::Value(Value::Nil))
    }
}

// ── do ────────────────────────────────────────────────────────────

pub fn do_do<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }
    let mut result = TailResult::Value(Value::Nil);
    for (i, expr) in args.iter().enumerate() {
        let is_last = i == args.len() - 1;
        result = eval_fn(expr, env, tail && is_last)?;
    }
    Ok(result)
}

// ── and / or (short-circuit) ────────────────────────────────────

pub fn do_and<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Boolean(true)));
    }
    for (i, expr) in args.iter().enumerate() {
        let is_last = i == args.len() - 1;
        let val = eval_fn(expr, env, tail && is_last)?;
        if !is_last {
            let v = match &val {
                TailResult::Value(v) => v,
                TailResult::Recur(_) => return Ok(val),
            };
            if !is_truthy(v) {
                return Ok(val);
            }
        } else {
            return Ok(val);
        }
    }
    unreachable!()
}

pub fn do_or<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }
    for (i, expr) in args.iter().enumerate() {
        let is_last = i == args.len() - 1;
        let val = eval_fn(expr, env, tail && is_last)?;
        if !is_last {
            let v = match &val {
                TailResult::Value(v) => v,
                TailResult::Recur(_) => return Ok(val),
            };
            if is_truthy(v) {
                return Ok(val);
            }
        } else {
            return Ok(val);
        }
    }
    unreachable!()
}

// ── cond ─────────────────────────────────────────────────────────

pub fn do_cond<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    use crate::special::eval_last_body;

    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }

    for (clause_idx, clause) in args.iter().enumerate() {
        let is_last_clause = clause_idx == args.len() - 1;
        match clause {
            Sexp::List(items) if !items.is_empty() => {
                let test = &items[0];
                let body_items: Vec<Sexp> = items.iter().skip(1).cloned().collect();
                let test_val = eval_fn(test, env, false)?.into_value();
                if is_truthy(&test_val) {
                    if body_items.is_empty() {
                        return Ok(TailResult::Value(test_val));
                    }
                    return eval_last_body(&body_items, env, tail && is_last_clause, eval_fn);
                }
            }
            _ if is_last_clause => {
                return eval_fn(clause, env, tail);
            }
            other => {
                return Err(EvalError::InvalidForm(format!(
                    "cond clause must be a list, got {}",
                    other
                )));
            }
        }
    }
    Ok(TailResult::Value(Value::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use im::Vector;

    fn test_eval(expr: &Sexp, env: &Arc<Env>, _tail: bool) -> Result<TailResult, EvalError> {
        match expr {
            Sexp::Nil => Ok(TailResult::Value(Value::Nil)),
            Sexp::Boolean(b) => Ok(TailResult::Value(Value::Boolean(*b))),
            Sexp::Integer(i) => Ok(TailResult::Value(Value::Integer(*i))),
            Sexp::Float(f) => Ok(TailResult::Value(Value::Float(*f))),
            Sexp::String(s) => Ok(TailResult::Value(Value::String(s.clone()))),
            Sexp::Keyword(k) => Ok(TailResult::Value(Value::Keyword(k.clone()))),
            Sexp::Symbol(s) => env
                .get(s)
                .map(TailResult::Value)
                .ok_or_else(|| EvalError::SymbolNotFound(s.clone())),
            other => Err(EvalError::InvalidForm(format!(
                "test_eval cannot handle {}",
                other
            ))),
        }
    }

    #[test]
    fn test_if_true() {
        let env = Arc::new(Env::new(None));
        let args = [Sexp::Boolean(true), Sexp::Integer(1), Sexp::Integer(2)];
        let result = do_if(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(1));
    }

    #[test]
    fn test_if_false() {
        let env = Arc::new(Env::new(None));
        let args = [Sexp::Boolean(false), Sexp::Integer(1), Sexp::Integer(2)];
        let result = do_if(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(2));
    }

    #[test]
    fn test_and_or_short_circuit() {
        let env = Arc::new(Env::new(None));
        let args = [Sexp::Boolean(true), Sexp::Integer(42)];
        let result = do_and(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(42));

        let args = [Sexp::Boolean(false), Sexp::Integer(42)];
        let result = do_and(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Boolean(false));

        let args = [Sexp::Boolean(false), Sexp::Integer(42)];
        let result = do_or(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(42));
    }

    #[test]
    fn test_cond() {
        let env = Arc::new(Env::new(None));
        // (cond (false 1) (true 2) (false 3))
        let args = [
            Sexp::List(Vector::from(vec![Sexp::Boolean(false), Sexp::Integer(1)])),
            Sexp::List(Vector::from(vec![Sexp::Boolean(true), Sexp::Integer(2)])),
            Sexp::List(Vector::from(vec![Sexp::Boolean(false), Sexp::Integer(3)])),
        ];
        let result = do_cond(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(2));
    }
}