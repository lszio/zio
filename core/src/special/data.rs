use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::value::Value;

// ── quote ──────────────────────────────────────────────────────────

pub fn do_quote(args: &[Sexp]) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::WrongArgCount {
            expected: 1,
            got: args.len(),
        });
    }
    Ok(TailResult::Value(Value::from(args[0].clone())))
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