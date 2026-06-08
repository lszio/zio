use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{parse_params, EvalFn, TailResult};
use crate::value::{Function, Macro, Value};

// ── def ────────────────────────────────────────────────────────────

pub fn do_def<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::WrongArgCount {
            expected: 2,
            got: args.len(),
        });
    }
    let name = match &args[0] {
        Sexp::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::TypeError {
                expected: "symbol",
                got: other.kind().to_string(),
            })
        }
    };
    let val = eval_fn(&args[1], env, false)?.into_value();
    env.set(name, val.clone());
    Ok(TailResult::Value(val))
}

// ── defn ──────────────────────────────────────────────────────────

pub fn do_defn(args: &[Sexp], env: &Arc<Env>, _eval_fn: &EvalFn) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::WrongArgCountMin {
            min: 3,
            got: args.len(),
        });
    }
    let name = match &args[0] {
        Sexp::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::TypeError {
                expected: "symbol",
                got: other.kind().to_string(),
            })
        }
    };
    let (params, rest_param) = parse_params(&args[1])?;
    let body = if args.len() == 3 {
        args[2].clone()
    } else {
        Sexp::List(args[2..].iter().cloned().collect())
    };
    let fn_val = Value::Function(Arc::new(Function {
        params,
        rest_param,
        body,
        env: env.clone(),
    }));
    env.set(name, fn_val.clone());
    Ok(TailResult::Value(fn_val))
}

// ── fn ────────────────────────────────────────────────────────────

pub fn do_fn(args: &[Sexp], env: &Arc<Env>) -> Result<TailResult, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::WrongArgCountMin {
            min: 2,
            got: args.len(),
        });
    }
    let (params, rest_param) = parse_params(&args[0])?;
    let body = if args.len() == 2 {
        args[1].clone()
    } else {
        Sexp::List(args[1..].iter().cloned().collect())
    };
    Ok(TailResult::Value(Value::Function(Arc::new(Function {
        params,
        rest_param,
        body,
        env: env.clone(),
    }))))
}

// ── defmacro ─────────────────────────────────────────────────────

pub fn do_defmacro(args: &[Sexp], env: &Arc<Env>, _eval_fn: &EvalFn) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::WrongArgCountMin {
            min: 3,
            got: args.len(),
        });
    }
    let name = match &args[0] {
        Sexp::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::TypeError {
                expected: "symbol",
                got: other.kind().to_string(),
            })
        }
    };
    let (params, rest_param) = parse_params(&args[1])?;
    let body = if args.len() == 3 {
        args[2].clone()
    } else {
        Sexp::List(args[2..].iter().cloned().collect())
    };

    let macro_val = Value::Macro(Arc::new(Macro {
        name: name.clone(),
        params,
        rest_param,
        body,
        env: env.clone(),
    }));
    env.set(name, macro_val.clone());
    Ok(TailResult::Value(macro_val))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn test_def() {
        let env = Arc::new(Env::new(None));
        let args = [Sexp::Symbol("x".into()), Sexp::Integer(10)];
        let result = do_def(&args, &env, &test_eval).unwrap().into_value();
        assert_eq!(result, Value::Integer(10));
        assert_eq!(env.get("x"), Some(Value::Integer(10)));
    }

    #[test]
    fn test_fn_creation() {
        let env = Arc::new(Env::new(None));
        let params = Sexp::Vector(Vector::from(vec![Sexp::Symbol("x".into())]));
        let body = Sexp::Symbol("x".into());
        let args = [params, body];
        let result = do_fn(&args, &env).unwrap().into_value();
        match result {
            Value::Function(_) => {} // OK
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn test_fn_with_rest() {
        let env = Arc::new(Env::new(None));
        let params = Sexp::Vector(Vector::from(vec![
            Sexp::Symbol("a".into()),
            Sexp::Symbol("&".into()),
            Sexp::Symbol("rest".into()),
        ]));
        let args = [params, Sexp::Integer(1)];
        let result = do_fn(&args, &env).unwrap().into_value();
        match result {
            Value::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.rest_param, Some("rest".to_string()));
            }
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn test_parse_params() {
        let params = Sexp::Vector(Vector::from(vec![Sexp::Symbol("a".into()), Sexp::Symbol("b".into())]));
        let (names, rest) = parse_params(&params).unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(rest, None);

        let params = Sexp::Vector(Vector::from(vec![
            Sexp::Symbol("a".into()),
            Sexp::Symbol("&".into()),
            Sexp::Symbol("rest".into()),
        ]));
        let (names, rest) = parse_params(&params).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(rest, Some("rest".to_string()));
    }
}