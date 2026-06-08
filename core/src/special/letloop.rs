use std::sync::Arc;

use im::Vector;

use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{eval_last_body, parse_bindings, EvalFn, TailResult};
use crate::value::Value;

// ── let / let* ────────────────────────────────────────────────────

pub fn do_let<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    do_let_inner(args, env, tail, false, eval_fn)
}

pub fn do_let_star<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    do_let_inner(args, env, tail, true, eval_fn)
}

fn do_let_inner<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    sequential: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("let requires bindings".into()));
    }
    let (names, inits) = parse_bindings(&args[0])?;
    let body_exprs = &args[1..];
    if body_exprs.is_empty() {
        return Err(EvalError::InvalidForm("let requires a body".into()));
    }

    if sequential {
        // let*: each binding sees previous bindings
        let inner_env = Arc::new(Env::new(Some(env.clone())));
        for (i, name) in names.into_iter().enumerate() {
            let val = eval_fn(inits[i], &inner_env, false)?.into_value();
            inner_env.set(name, val);
        }
        eval_last_body(body_exprs, &inner_env, tail, eval_fn)
    } else {
        // let: evaluate all inits in outer env, then bind in new env
        let mut init_vals = Vec::new();
        for init in &inits {
            init_vals.push(eval_fn(init, env, false)?.into_value());
        }
        let inner_env = Arc::new(Env::new(Some(env.clone())));
        for (name, val) in names.into_iter().zip(init_vals) {
            inner_env.set(name, val);
        }
        eval_last_body(body_exprs, &inner_env, tail, eval_fn)
    }
}

// ── loop / recur ─────────────────────────────────────────────────

pub fn do_loop<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("loop requires bindings".into()));
    }

    let (names, inits) = parse_bindings(&args[0])?;
    let body_exprs = &args[1..];
    if body_exprs.is_empty() {
        return Err(EvalError::InvalidForm("loop requires a body".into()));
    }

    let mut values: Vec<Value> = Vec::new();
    for init in &inits {
        values.push(eval_fn(init, env, false)?.into_value());
    }

    let body: &Sexp = if body_exprs.len() == 1 {
        &body_exprs[0]
    } else {
        return Err(EvalError::InvalidForm(
            "loop body must be a single expression (use do for multiple)".into(),
        ));
    };

    let num_bindings = names.len();
    loop {
        let loop_env = Arc::new(Env::new(Some(env.clone())));
        for (i, name) in names.iter().enumerate() {
            loop_env.set(name.clone(), values[i].clone());
        }
        match eval_fn(body, &loop_env, true)? {
            TailResult::Value(v) => return Ok(TailResult::Value(v)),
            TailResult::Recur(new_args) => {
                if new_args.len() != num_bindings {
                    return Err(EvalError::WrongArgCount {
                        expected: num_bindings,
                        got: new_args.len(),
                    });
                }
                values = new_args.into_iter().collect();
            }
        }
    }
}

pub fn do_recur<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if !tail {
        return Err(EvalError::RecurNotTail);
    }
    let mut recur_args = Vector::new();
    for arg in args {
        match eval_fn(arg, env, false)? {
            TailResult::Value(v) => recur_args.push_back(v),
            _ => return Err(EvalError::InvalidForm("recur in recur args".into())),
        }
    }
    Ok(TailResult::Recur(recur_args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;

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
    fn test_let_bindings() {
        let env = Arc::new(Env::new(None));
        let bindings = Sexp::Vector(Vector::from(vec![
            Sexp::Symbol("x".into()),
            Sexp::Integer(10),
            Sexp::Symbol("y".into()),
            Sexp::Integer(20),
        ]));
        let body = Sexp::Symbol("x".into());
        let args = [bindings, body];
        let result = do_let(&args, &env, false, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(10));
    }
}