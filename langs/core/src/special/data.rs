use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
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

// ── macroexpand lives in builtins/macroexpand.rs as a native function:
// the quoted form's quote is stripped by evaluation before it runs.
// A special-form variant here used to shadow it and returned the
// (quote ...) wrapper unexpanded — see book/09 documented semantics.

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
