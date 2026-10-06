//! Macroexpand and eval builtins — bridge runtime Values back to Sexp code.

use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::macros;
use crate::sexp::Sexp;
use crate::value::{NativeFn, Value};

/// (eval form) — evaluate code-as-data at runtime.
///
/// This is the homoiconicity primitive: programs are data, and data can
/// become running code. Like Common Lisp's `eval`, the form is evaluated
/// in the global environment — lexical bindings of the call site are not
/// visible; bind what you need inside the form (e.g. via `let`).
pub fn eval_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    let sexp = macros::value_to_sexp(&args[0])?;
    let env = engine.env();
    engine.eval_expr(&sexp, env, false).map(|r| r.into_value())
}

pub fn macroexpand_1_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }
    // Convert the Value argument back to Sexp for macro expansion
    let sexp = macros::value_to_sexp(&args[0])?;

    match &sexp {
        Sexp::List(list, _) if !list.is_empty() => {
            if let Sexp::Symbol(name, _) = &list[0] {
                let macro_args: Vec<Sexp> = list.iter().skip(1).cloned().collect();
                let env = engine.env();
                if let Some(expanded) = macros::try_expand_by_name(name, &macro_args, env, engine)? {
                    return Ok(Value::from(expanded));
                }
            }
            Ok(args[0].clone())
        }
        _ => Ok(args[0].clone()),
    }
}

/// Fully expand a form: recursively expand until no more macro calls remain.
pub fn macroexpand_fn(args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::wrong_arg_count(1, args.len()));
    }

    let mut current = macros::value_to_sexp(&args[0])?;
    let env = engine.env();

    loop {
        let next = match &current {
            Sexp::List(list, _) if !list.is_empty() => {
                if let Sexp::Symbol(name, _) = &list[0] {
                    let macro_args: Vec<Sexp> = list.iter().skip(1).cloned().collect();
                    macros::try_expand_by_name(name, &macro_args, env, engine)?
                } else {
                    None
                }
            }
            _ => None,
        };
        match next {
            Some(expanded) => current = expanded,
            None => break,
        }
    }

    Ok(Value::from(current))
}

pub fn register(env: &Arc<Env>) {
    env.set("eval".into(), Value::NativeFunction(NativeFn::new("eval", eval_fn)));
    env.set("macroexpand-1".into(), Value::NativeFunction(NativeFn::new("macroexpand-1", macroexpand_1_fn)));
    env.set("macroexpand".into(), Value::NativeFunction(NativeFn::new("macroexpand", macroexpand_fn)));
}
