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

// ── quasiquote ─────────────────────────────────────────────────────

/// (quasiquote form) — template written as `` `form ``. A `~expr` inside the
/// template evaluates; a `~@expr` splices a list or vector into the enclosing
/// list or vector. A nested backquote increases depth: only the `~` at the
/// matching depth evaluates, deeper ones are rebuilt literally.
pub fn do_quasiquote(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    Ok(TailResult::Value(quasi(&args[0], 1, env, engine)?))
}

/// If `s` is a two-element list headed by `unquote`, `unquote-splicing` or
/// `quasiquote`, return the head name and the wrapped form.
fn wrapping_form(s: &Sexp) -> Option<(&str, &Sexp)> {
    let Sexp::List(items, _) = s else {
        return None;
    };
    if items.len() != 2 {
        return None;
    }
    let Sexp::Symbol(name, _) = items.front()? else {
        return None;
    };
    match name.as_str() {
        n @ ("unquote" | "unquote-splicing" | "quasiquote") => Some((n, &items[1])),
        _ => None,
    }
}

fn quasi(
    expr: &Sexp,
    depth: u32,
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Value, EvalError> {
    if let Some((name, inner)) = wrapping_form(expr) {
        return match (name, depth) {
            ("unquote", 1) => Ok(engine.eval_expr(inner, env, false)?.into_value()),
            ("unquote-splicing", 1) => Err(EvalError::invalid_form(
                "unquote-splicing (~@) outside of a list or vector template",
            )),
            ("quasiquote", _) => {
                let mut out = im::Vector::new();
                out.push_back(Value::Symbol("quasiquote".into()));
                out.push_back(quasi(inner, depth + 1, env, engine)?);
                Ok(Value::List(out))
            }
            _ => {
                let mut out = im::Vector::new();
                out.push_back(Value::Symbol(name.into()));
                out.push_back(quasi(inner, depth - 1, env, engine)?);
                Ok(Value::List(out))
            }
        };
    }
    match expr {
        Sexp::List(items, _) => {
            let mut out = im::Vector::new();
            for item in items.iter() {
                if depth == 1 {
                    if let Some(("unquote-splicing", inner)) = wrapping_form(item) {
                        let spliced = engine.eval_expr(inner, env, false)?.into_value();
                        match spliced {
                            Value::List(vs) | Value::Vector(vs) => out.extend(vs),
                            other => {
                                return Err(EvalError::type_error(
                                    "sequential",
                                    other.value_type(),
                                ));
                            }
                        }
                        continue;
                    }
                }
                out.push_back(quasi(item, depth, env, engine)?);
            }
            Ok(Value::List(out))
        }
        Sexp::Vector(items, _) => {
            let mut out = im::Vector::new();
            for item in items.iter() {
                if depth == 1 {
                    if let Some(("unquote-splicing", inner)) = wrapping_form(item) {
                        let spliced = engine.eval_expr(inner, env, false)?.into_value();
                        match spliced {
                            Value::List(vs) | Value::Vector(vs) => out.extend(vs),
                            other => {
                                return Err(EvalError::type_error(
                                    "sequential",
                                    other.value_type(),
                                ));
                            }
                        }
                        continue;
                    }
                }
                out.push_back(quasi(item, depth, env, engine)?);
            }
            Ok(Value::Vector(out))
        }
        Sexp::Map(m, _) => {
            let mut out = im::HashMap::new();
            for (k, v) in m.iter() {
                out.insert(quasi(k, depth, env, engine)?, quasi(v, depth, env, engine)?);
            }
            Ok(Value::Map(out))
        }
        other => Ok(Value::from(other.clone())),
    }
}

// ── set! ──────────────────────────────────────────────────────────

/// (set! symbol expr) — mutate the binding of `symbol` in the nearest scope
/// where it is defined, or create a new binding in the current scope if none
/// exists.
pub fn do_set(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => {
            return Err(EvalError::invalid_form(format!(
                "set! requires a symbol, got {}",
                other.kind()
            )));
        }
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
