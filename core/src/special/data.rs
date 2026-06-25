use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::value::Value;

use crate::context::EvalEngine;
use crate::env::Env;
use std::sync::Arc;
use crate::macros;

// ── quote ──────────────────────────────────────────────────────────

pub fn do_quote(args: &[Sexp]) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len(),
        ));
    }
    Ok(TailResult::Value(Value::from(args[0].clone())))
}

// ── macroexpand (special form — does NOT evaluate args) ────────────

pub fn do_macroexpand(args: &[Sexp], env: &Arc<Env>, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }

    let form = &args[0];
    match form {
        Sexp::List(list) if !list.is_empty() => {
            if let Sexp::Symbol(name) = &list[0] {
                let expanded_args: Vec<Sexp> = list.iter().skip(1).cloned().collect();
                if let Some(expanded) = macros::try_expand_by_name(name, &expanded_args, env, engine)? {
                    return Ok(TailResult::Value(Value::from(expanded)));
                }
            }
            // Not a macro call — return form unchanged
            Ok(TailResult::Value(Value::from(form.clone())))
        }
        _ => Ok(TailResult::Value(Value::from(form.clone()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Env;
    use std::sync::Arc;

    #[test]
    fn test_quote() {
        let _env = Arc::new(Env::new(None));
        let args = [Sexp::Integer(42)];
        let result = do_quote(&args).unwrap().into_value();
        assert_eq!(result, Value::Integer(42));
    }

    #[test]
    fn test_quote_wrong_arg_count() {
        let result = do_quote(&[]);
        assert!(result.is_err());
    }
}