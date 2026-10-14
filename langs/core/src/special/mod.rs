use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::value::Value;

mod bindings;
mod control;
mod data;
mod letloop;
pub(crate) mod module_forms;
pub(crate) mod zos_forms;
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
    // Select before calling: one return slot, not one per handler in debug
    // builds. This dispatcher sits on every recursive AST evaluation path.
    let handler: fn(&[Sexp], &Arc<Env>, bool, &dyn EvalEngine) -> Result<TailResult, EvalError> =
        match name {
            "quote" => |args, _, _, _| data::do_quote(args),
            "quasiquote" => |args, env, _, engine| data::do_quasiquote(args, env, engine),
            "unquote" | "unquote-splicing" => {
                return Err(EvalError::invalid_form(format!(
                    "{name} (~ / ~@) used outside quasiquote (`)"
                )));
            }
            "set!" => |args, env, _, engine| data::do_set(args, env, engine),
            "def" => |args, env, _, engine| bindings::do_def(args, env, engine),
            "defn" => |args, env, _, engine| bindings::do_defn(args, env, engine),
            "if" => control::do_if,
            "do" => control::do_do,
            "fn" => |args, env, _, _| bindings::do_fn(args, env),
            "let" => letloop::do_let,
            "let*" => letloop::do_let_star,
            "loop" => |args, env, _, engine| letloop::do_loop(args, env, engine),
            "recur" => letloop::do_recur,
            "defmacro" => |args, env, _, engine| bindings::do_defmacro(args, env, engine),
            "and" => control::do_and,
            "or" => control::do_or,
            "cond" => control::do_cond,
            "module" => |args, env, _, engine| module_forms::do_module(args, env, engine),
            "require" => |args, env, _, engine| module_forms::do_require(args, env, engine),
            "export" => |args, env, _, engine| module_forms::do_export(args, env, engine),
            "defclass" => |args, env, _, engine| zos_forms::do_defclass(args, env, engine),
            "defgeneric" => |args, env, _, engine| zos_forms::do_defgeneric(args, env, engine),
            "defmethod" => |args, env, _, engine| zos_forms::do_defmethod(args, env, engine),
            "call-next-method" => {
                |args, env, _, engine| zos_forms::do_call_next_method(args, env, engine)
            }
            "defpackage" => |args, env, _, engine| zos_forms::do_defpackage(args, env, engine),
            "try" => zos_forms::do_try,
            _ => return Ok(None),
        };
    handler(args, env, tail, engine).map(Some)
}
// ── Shared Helpers ─────────────────────────────────────────────────

/// Parse bindings like `[x 1 y 2]` into (names, init-exprs).
pub fn parse_bindings(binding_sexp: &Sexp) -> Result<(Vector<String>, Vec<&Sexp>), EvalError> {
    let items = match binding_sexp {
        Sexp::Vector(v, _) => v.iter().collect::<Vec<_>>(),
        Sexp::List(l, _) => l.iter().collect::<Vec<_>>(),
        other => {
            return Err(EvalError::invalid_form(format!(
                "bindings must be a vector or list, got {}",
                other.kind()
            )));
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
                return Err(EvalError::invalid_form(format!(
                    "binding name must be a symbol, got {}",
                    other.kind()
                )));
            }
        };
        names.push_back(name);
        inits.push(*init);
    }
    Ok((names, inits))
}

/// Parse fn-style params: `[a b & rest]` or `[a b]`.
pub fn parse_params(param_sexp: &Sexp) -> Result<(Vector<String>, Option<String>), EvalError> {
    let items = match param_sexp {
        Sexp::Vector(v, _) => v.clone(),
        Sexp::List(l, _) => l.clone(),
        other => {
            return Err(EvalError::invalid_form(format!(
                "params must be a vector or list, got {}",
                other.kind()
            )));
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
                return Err(EvalError::invalid_form(format!(
                    "param name must be a symbol, got {}",
                    other.kind()
                )));
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
        let sexp = Sexp::Vector(
            vector![
                Sexp::Symbol("x".into(), None),
                Sexp::Integer(1, None),
                Sexp::Symbol("y".into(), None),
                Sexp::Integer(2, None),
            ],
            None,
        );
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
        let sexp = Sexp::Vector(
            vector![
                Sexp::Symbol("a".into(), None),
                Sexp::Symbol("b".into(), None),
            ],
            None,
        );
        let (names, rest) = parse_params(&sexp).unwrap();
        assert_eq!(names, vector!["a".to_string(), "b".to_string()]);
        assert!(rest.is_none());
    }

    #[test]
    fn test_parse_params_variadic() {
        let sexp = Sexp::Vector(
            vector![
                Sexp::Symbol("a".into(), None),
                Sexp::Symbol("&".into(), None),
                Sexp::Symbol("rest".into(), None),
            ],
            None,
        );
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
        let result = eval_last_body(&body, &env, false, &ctx)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(2));
    }
}
