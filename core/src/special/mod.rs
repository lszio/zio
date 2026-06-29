use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::value::{Value};

mod bindings;
mod control;
mod letloop;
mod data;
mod module_forms;
mod zos_forms;
/// Result of evaluating an expression, possibly a recur or tail call.
#[derive(Debug, Clone)]
pub enum TailResult {
    /// Normal return value.
    Value(Value),
    /// Loop recur: re-bind loop args and continue.
    Recur(Vector<Value>),
    /// Tail call: trampoline back through eval loop.
    TailCall(Value, Vector<Value>),
}

impl TailResult {
    /// Unwrap into Value, panicking if Recur or TailCall.
    pub fn into_value(self) -> Value {
        match self {
            TailResult::Value(v) => v,
            other => panic!("cannot convert {other:?} to Value"),
        }
    }
}

/// Dispatch to the appropriate special form handler.
/// Returns `None` if `name` is not a special form.
pub fn eval_special_form(
    name: &str,
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<Option<TailResult>, EvalError> {
    let result = match name {
        "quote" => Some(data::do_quote(args)?),
        "macroexpand" => Some(data::do_macroexpand(args, env, engine)?),
        "def" => Some(bindings::do_def(args, env, engine)?),
        "defn" => Some(bindings::do_defn(args, env, engine)?),
        "if" => Some(control::do_if(args, env, tail, engine)?),
        "do" => Some(control::do_do(args, env, tail, engine)?),
        "fn" => Some(bindings::do_fn(args, env)?),
        "let" => Some(letloop::do_let(args, env, tail, engine)?),
        "let*" => Some(letloop::do_let_star(args, env, tail, engine)?),
        "loop" => Some(letloop::do_loop(args, env, engine)?),
        "recur" => Some(letloop::do_recur(args, env, tail, engine)?),
        "defmacro" => Some(bindings::do_defmacro(args, env, engine)?),
        "and" => Some(control::do_and(args, env, tail, engine)?),
        "or" => Some(control::do_or(args, env, tail, engine)?),
        "cond" => Some(control::do_cond(args, env, tail, engine)?),
        "module" => Some(module_forms::do_module(args, env, engine)?),
        "require" => Some(module_forms::do_require(args, env, engine)?),
        "defclass" => Some(zos_forms::do_defclass(args, env, engine)?),
        _ => None,
    };
    Ok(result)
}
// ── Shared Helpers ─────────────────────────────────────────────────

/// Parse bindings like `[x 1 y 2]` into (names, init-exprs).
pub fn parse_bindings(
    binding_sexp: &Sexp,
) -> Result<(Vector<String>, Vec<&Sexp>), EvalError> {
    let items = match binding_sexp {
        Sexp::Vector(v, _) => v.iter().collect::<Vec<_>>(),
        Sexp::List(l, _) => l.iter().collect::<Vec<_>>(),
        other => {
            return Err(EvalError::invalid_form(
                format!("bindings must be a vector or list, got {}", other.kind()),
            ));
        }
    };
    if items.len() % 2 != 0 {
        return Err(EvalError::invalid_form(
            "bindings must have an even number of elements",
        ));
    }
    let mut names = Vector::new();
    let mut inits = Vec::new();
    let mut iter = items.iter();
    while let (Some(name_sexp), Some(init)) = (iter.next(), iter.next()) {
        let name = match name_sexp {
            Sexp::Symbol(s, _) => s.clone(),
            other => {
                return Err(EvalError::invalid_form(
                    format!("binding name must be a symbol, got {}", other.kind()),
                ));
            }
        };
        names.push_back(name);
        inits.push(*init);
    }
    Ok((names, inits))
}

/// Parse fn-style params: `[a b & rest]` or `[a b]`.
pub fn parse_params(
    param_sexp: &Sexp,
) -> Result<(Vector<String>, Option<String>), EvalError> {
    let items = match param_sexp {
        Sexp::Vector(v, _) => v.clone(),
        Sexp::List(l, _) => l.clone(),
        other => {
            return Err(EvalError::invalid_form(
                format!("params must be a vector or list, got {}", other.kind()),
            ));
        }
    };
    let mut names = Vector::new();
    let mut rest_param = None;
    let mut rest_seen = false;
    for item in items.iter() {
        match item {
            Sexp::Symbol(s, _) if s == "&" => {
                rest_seen = true;
            }
            Sexp::Symbol(s, _) if rest_seen => {
                rest_param = Some(s.clone());
            }
            Sexp::Symbol(s, _) => {
                names.push_back(s.clone());
            }
            other => {
                return Err(EvalError::invalid_form(
                    format!("param name must be a symbol, got {}", other.kind()),
                ));
            }
        }
    }
    Ok((names, rest_param))
}

/// Evaluate a sequence of body expressions, returning the last value.
pub fn eval_last_body(
    exprs: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    let len = exprs.len();
    for (i, expr) in exprs.iter().enumerate() {
        let is_last = i == len - 1;
        let result = engine.eval_expr(expr, env, tail && is_last)?;
        if is_last {
            return Ok(result);
        }
    }
    // Empty body
    Ok(TailResult::Value(Value::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::vector;
    #[test]
    fn test_parse_bindings() {
        let sexp = Sexp::Vector(vector![
            Sexp::Symbol("x".into(), None),
            Sexp::Integer(1, None),
            Sexp::Symbol("y".into(), None),
            Sexp::Integer(2, None),
        ], None);
        let (names, inits) = parse_bindings(&sexp).unwrap();
        assert_eq!(names, vector!["x".to_string(), "y".to_string()]);
        assert_eq!(inits.len(), 2);
    }

    #[test]
    fn test_parse_bindings_odd() {
        let sexp = Sexp::Vector(vector![Sexp::Symbol("x".into(), None)], None);
        assert!(parse_bindings(&sexp).is_err());
    }

    #[test]
    fn test_parse_params() {
        let sexp = Sexp::Vector(vector![
            Sexp::Symbol("a".into(), None),
            Sexp::Symbol("b".into(), None),
        ], None);
        let (names, rest) = parse_params(&sexp).unwrap();
        assert_eq!(names, vector!["a".to_string(), "b".to_string()]);
        assert!(rest.is_none());
    }

    #[test]
    fn test_parse_params_variadic() {
        let sexp = Sexp::Vector(vector![
            Sexp::Symbol("a".into(), None),
            Sexp::Symbol("&".into(), None),
            Sexp::Symbol("rest".into(), None),
        ], None);
        let (names, rest) = parse_params(&sexp).unwrap();
        assert_eq!(names, vector!["a".to_string()]);
        assert_eq!(rest, Some("rest".to_string()));
    }

    #[test]
    fn test_eval_last_body() {
        use crate::context::EvalContext;
        use crate::env::Env;

        let env = Arc::new(Env::new(None));
        let ctx = EvalContext::new(env.clone());
        let body = [Sexp::Integer(1, None), Sexp::Integer(2, None)];
        let result = eval_last_body(&body, &env, false, &ctx).unwrap().into_value();
        assert_eq!(result, Value::Integer(2));
    }
}