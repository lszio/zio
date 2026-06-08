use std::sync::Arc;

use im::Vector;

use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::value::{Value};

mod bindings;
mod control;
mod letloop;
mod data;

/// Type for the recursive eval function passed to special forms.
pub type EvalFn<'a> = dyn Fn(&Sexp, &Arc<Env>, bool) -> Result<TailResult, EvalError> + 'a;

/// Result of evaluating an expression, possibly a recur target.
#[derive(Debug, Clone)]
pub enum TailResult {
    Value(Value),
    Recur(Vector<Value>),
}

impl TailResult {
    /// Unwrap into Value, panicking if it's a Recur.
    pub fn into_value(self) -> Value {
        match self {
            TailResult::Value(v) => v,
            TailResult::Recur(_) => panic!("unexpected recur result"),
        }
    }
}

/// Dispatch to the appropriate special form handler.
/// Returns `None` if `name` is not a special form.
pub fn eval_special_form<'a>(
    name: &str,
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<Option<TailResult>, EvalError> {
    let result = match name {
        "quote" => Some(data::do_quote(args)?),
        "def" => Some(bindings::do_def(args, env, eval_fn)?),
        "defn" => Some(bindings::do_defn(args, env, eval_fn)?),
        "if" => Some(control::do_if(args, env, tail, eval_fn)?),
        "do" => Some(control::do_do(args, env, tail, eval_fn)?),
        "fn" => Some(bindings::do_fn(args, env)?),
        "let" => Some(letloop::do_let(args, env, tail, eval_fn)?),
        "let*" => Some(letloop::do_let_star(args, env, tail, eval_fn)?),
        "loop" => Some(letloop::do_loop(args, env, eval_fn)?),
        "recur" => Some(letloop::do_recur(args, env, tail, eval_fn)?),
        "defmacro" => Some(bindings::do_defmacro(args, env, eval_fn)?),
        "and" => Some(control::do_and(args, env, tail, eval_fn)?),
        "or" => Some(control::do_or(args, env, tail, eval_fn)?),
        "cond" => Some(control::do_cond(args, env, tail, eval_fn)?),
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
        Sexp::Vector(v) => v,
        Sexp::List(l) => l,
        other => {
            return Err(EvalError::TypeError {
                expected: "vector or list",
                got: other.kind().to_string(),
            })
        }
    };
    if items.len() % 2 != 0 {
        return Err(EvalError::InvalidForm(
            "bindings must have an even number of elements".into(),
        ));
    }
    let mut names = Vector::new();
    let mut inits = Vec::new();
    let mut iter = items.iter();
    while let Some(key) = iter.next() {
        let name = match key {
            Sexp::Symbol(s) => s.clone(),
            other => {
                return Err(EvalError::TypeError {
                    expected: "symbol",
                    got: other.kind().to_string(),
                })
            }
        };
        let init = iter.next().unwrap();
        names.push_back(name);
        inits.push(init);
    }
    Ok((names, inits))
}

/// Parse fn-style params: `[a b & rest]` or `[a b]`.
pub fn parse_params(
    param_sexp: &Sexp,
) -> Result<(Vector<String>, Option<String>), EvalError> {
    let items = match param_sexp {
        Sexp::Vector(v) => v,
        Sexp::List(l) => l,
        other => {
            return Err(EvalError::TypeError {
                expected: "vector or list",
                got: other.kind().to_string(),
            })
        }
    };
    let mut params = Vector::new();
    let mut rest_param = None;
    let mut after_ampersand = false;

    for item in items {
        match item {
            Sexp::Symbol(s) if s == "&" => {
                if after_ampersand {
                    return Err(EvalError::InvalidForm("multiple & in params".into()));
                }
                after_ampersand = true;
                continue;
            }
            Sexp::Symbol(s) => {
                if after_ampersand {
                    if rest_param.is_some() {
                        return Err(EvalError::InvalidForm("only one rest param allowed".into()));
                    }
                    rest_param = Some(s.clone());
                } else {
                    params.push_back(s.clone());
                }
            }
            other => {
                return Err(EvalError::TypeError {
                    expected: "symbol",
                    got: other.kind().to_string().to_string(),
                })
            }
        }
    }

    Ok((params, rest_param))
}

/// Evaluate a sequence of body expressions, returning the last value.
pub fn eval_last_body<'a>(
    exprs: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if exprs.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }
    let mut result = TailResult::Value(Value::Nil);
    for (i, expr) in exprs.iter().enumerate() {
        let is_last = i == exprs.len() - 1;
        result = eval_fn(expr, env, tail && is_last)?;
    }
    Ok(result)
}