use std::sync::Arc;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{parse_params, TailResult};
use crate::value::{Function, Macro, Value};

// ── def ────────────────────────────────────────────────────────────

pub fn do_def(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::invalid_form(
            format!("def requires a symbol, got {}", other.kind()),
        )),
    };
    let val = engine.eval_expr(&args[1], env, false)?.into_value();
    env.set(name, val.clone());
    Ok(TailResult::Value(val))
}

// ── defn ──────────────────────────────────────────────────────────

pub fn do_defn(args: &[Sexp], env: &Arc<Env>, _engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::wrong_arg_count_min(3, args.len()));
    }
    let name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::invalid_form(
            format!("defn requires a symbol, got {}", other.kind()),
        )),
    };
    let (params, rest_param) = parse_params(&args[1])?;
    let body = if args.len() == 3 {
        args[2].clone()
    } else {
        Sexp::List(args[2..].iter().cloned().collect(), None)
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
    if args.is_empty() {
        return Err(EvalError::invalid_form("fn requires at least params"));
    }
    let (params, rest_param) = parse_params(&args[0])?;
    let body = if args.len() == 1 {
        // (fn []) with no body is allowed, body is nil
        Sexp::Nil
    } else if args.len() == 2 {
        args[1].clone()
    } else {
        Sexp::List(args[1..].iter().cloned().collect(), None)
    };
    Ok(TailResult::Value(Value::Function(Arc::new(Function {
        params,
        rest_param,
        body,
        env: env.clone(),
    }))))
}

// ── defmacro ─────────────────────────────────────────────────────

pub fn do_defmacro(args: &[Sexp], env: &Arc<Env>, _engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::wrong_arg_count_min(3, args.len()));
    }
    let name = match &args[0] {
        Sexp::Symbol(s, _) => s.clone(),
        other => return Err(EvalError::invalid_form(
            format!("defmacro requires a symbol, got {}", other.kind()),
        )),
    };
    let (params, rest_param) = parse_params(&args[1])?;
    let body = if args.len() == 3 {
        args[2].clone()
    } else {
        Sexp::List(args[2..].iter().cloned().collect(), None)
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