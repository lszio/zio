use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::macros;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::value::Value;

use std::sync::Arc;

// ── quote ──────────────────────────────────────────────────────────

pub fn do_quote(args: &[Sexp]) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    Ok(TailResult::Value(Value::from(args[0].clone())))
}

// ── set! ──────────────────────────────────────────────────────────

/// (set! symbol expr) — mutate the binding of `symbol` in the nearest scope
/// where it is defined, or create a new binding in the current scope if none
/// exists.
pub fn do_set(args: &[Sexp], env: &Arc<Env>, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::invalid_form(
            format!("set! requires a symbol, got {}", other.kind()),
        )),
    };
    let val = engine.eval_expr(&args[1], env, false)?.into_value();
    if !env.set_global(&name, val.clone()) {
        env.set(name, val.clone());
    }
    Ok(TailResult::Value(val))
}

// ── macroexpand (special form — does NOT evaluate args) ────────────

pub fn do_macroexpand(args: &[Sexp], env: &Arc<Env>, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }

    let form = &args[0];
    match form {
        Sexp::List(list, _) if !list.is_empty() => {
            if let Sexp::Symbol(name, _) = &list[0] {
                let expanded_args: Vec<Sexp> = list.iter().skip(1).cloned().collect();
                if let Some(expanded) = macros::try_expand_by_name(name, &expanded_args, env, engine)? {
                    return Ok(TailResult::Value(Value::from(expanded)));
                }
            }
            Ok(TailResult::Value(Value::from(form.clone())))
        }
        _ => Ok(TailResult::Value(Value::from(form.clone()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote() {
        let args = [Sexp::Integer(42, None)];
        let result = do_quote(&args).unwrap().into_value();
        assert_eq!(result, Value::Integer(42));
    }

    #[test]
    fn test_quote_wrong_arg_count() {
        let result = do_quote(&[]);
        assert!(result.is_err());
    }
}
