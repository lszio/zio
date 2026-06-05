use std::sync::Arc;

use im::Vector;

use crate::core::env::Env;
use crate::core::error::EvalError;
use crate::core::sexp::Sexp;
use crate::core::value::{is_truthy, Function, Macro, Value};

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
        "quote" => Some(do_quote(args)?),
        "def" => Some(do_def(args, env, eval_fn)?),
        "defn" => Some(do_defn(args, env, eval_fn)?),
        "if" => Some(do_if(args, env, tail, eval_fn)?),
        "do" => Some(do_do(args, env, tail, eval_fn)?),
        "fn" => Some(do_fn(args, env)?),
        "let" => Some(do_let(args, env, tail, false, eval_fn)?),
        "let*" => Some(do_let(args, env, tail, true, eval_fn)?),
        "loop" => Some(do_loop(args, env, eval_fn)?),
        "recur" => Some(do_recur(args, env, tail, eval_fn)?),
        "defmacro" => Some(do_defmacro(args, env, eval_fn)?),
        "and" => Some(do_and(args, env, tail, eval_fn)?),
        "or" => Some(do_or(args, env, tail, eval_fn)?),
        "cond" => Some(do_cond(args, env, tail, eval_fn)?),
        _ => None,
    };
    Ok(result)
}

// ── Helpers ────────────────────────────────────────────────────────

/// Parse bindings like `[x 1 y 2]` into (names, init-exprs).
fn parse_bindings(
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
fn parse_params(
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
                        return Err(EvalError::InvalidForm(
                            "only one rest param allowed".into(),
                        ));
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

// ── quote ──────────────────────────────────────────────────────────

fn do_quote(args: &[Sexp]) -> Result<TailResult, EvalError> {
    if args.len() != 1 {
        return Err(EvalError::WrongArgCount {
            expected: 1,
            got: args.len(),
        });
    }
    Ok(TailResult::Value(Value::from(args[0].clone())))
}

// ── def ────────────────────────────────────────────────────────────

fn do_def<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.len() != 2 {
        return Err(EvalError::WrongArgCount {
            expected: 2,
            got: args.len(),
        });
    }
    let name = match &args[0] {
        Sexp::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::TypeError {
                expected: "symbol",
                got: other.kind().to_string(),
            })
        }
    };
    let val = eval_fn(&args[1], env, false)?.into_value();
    env.set(name, val.clone());
    Ok(TailResult::Value(val))
}

// ── defn ──────────────────────────────────────────────────────────

fn do_defn(
    args: &[Sexp],
    env: &Arc<Env>,
    _eval_fn: &EvalFn,
) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::WrongArgCountMin {
            min: 3,
            got: args.len(),
        });
    }
    let name = match &args[0] {
        Sexp::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::TypeError {
                expected: "symbol",
                got: other.kind().to_string(),
            })
        }
    };
    let (params, rest_param) = parse_params(&args[1])?;
    // Body: remaining args as implicit do
    let body = if args.len() == 3 {
        args[2].clone()
    } else {
        Sexp::List(args[2..].iter().cloned().collect())
    };
    let fn_val = Value::Function(Arc::new(Function {
        params,
        rest_param,
        body,
        env: env.clone(),
    }));
    env.set(name, fn_val.clone());
    // Return the function, not nil — eval_fn not needed for the body
    Ok(TailResult::Value(fn_val))
}

// ── if ────────────────────────────────────────────────────────────

fn do_if<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::WrongArgCountMin {
            min: 2,
            got: args.len(),
        });
    }
    let cond = eval_fn(&args[0], env, false)?.into_value();
    if is_truthy(&cond) {
        eval_fn(&args[1], env, tail)
    } else if args.len() == 3 {
        eval_fn(&args[2], env, tail)
    } else {
        Ok(TailResult::Value(Value::Nil))
    }
}

// ── do ────────────────────────────────────────────────────────────

fn do_do<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }
    let mut result = TailResult::Value(Value::Nil);
    for (i, expr) in args.iter().enumerate() {
        let is_last = i == args.len() - 1;
        result = eval_fn(expr, env, tail && is_last)?;
    }
    Ok(result)
}

// ── fn ────────────────────────────────────────────────────────────

fn do_fn(args: &[Sexp], env: &Arc<Env>) -> Result<TailResult, EvalError> {
    if args.len() < 2 {
        return Err(EvalError::WrongArgCountMin {
            min: 2,
            got: args.len(),
        });
    }
    let (params, rest_param) = parse_params(&args[0])?;
    // Body: if multiple exprs, wrap in implicit do
    let body = if args.len() == 2 {
        args[1].clone()
    } else {
        Sexp::List(args[1..].iter().cloned().collect())
    };
    Ok(TailResult::Value(Value::Function(Arc::new(Function {
        params,
        rest_param,
        body,
        env: env.clone(),
    }))))
}

// ── let / let* ────────────────────────────────────────────────────

fn do_let<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    sequential: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("let requires bindings".into()));
    }
    let (names, inits) = parse_bindings(&args[0])?;
    let body_exprs = &args[1..];
    if body_exprs.is_empty() {
        return Err(EvalError::InvalidForm("let requires a body".into()));
    }

    if sequential {
        // let*: each binding sees previous bindings
        let inner_env = Arc::new(Env::new(Some(env.clone())));
        for (i, name) in names.into_iter().enumerate() {
            let val = eval_fn(inits[i], &inner_env, false)?.into_value();
            inner_env.set(name, val);
        }
        eval_last_body(body_exprs, &inner_env, tail, eval_fn)
    } else {
        // let: evaluate all inits in outer env, then bind in new env
        let mut init_vals = Vec::new();
        for init in &inits {
            init_vals.push(eval_fn(init, env, false)?.into_value());
        }
        let inner_env = Arc::new(Env::new(Some(env.clone())));
        for (name, val) in names.into_iter().zip(init_vals) {
            inner_env.set(name, val);
        }
        eval_last_body(body_exprs, &inner_env, tail, eval_fn)
    }
}

/// Evaluate a sequence of body expressions, returning the last value.
fn eval_last_body<'a>(
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

// ── loop / recur ─────────────────────────────────────────────────

fn do_loop<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("loop requires bindings".into()));
    }

    let (names, inits) = parse_bindings(&args[0])?;
    let body_exprs = &args[1..];
    if body_exprs.is_empty() {
        return Err(EvalError::InvalidForm("loop requires a body".into()));
    }

    // Evaluate initial values in current env
    let mut values: Vec<Value> = Vec::new();
    for init in &inits {
        values.push(eval_fn(init, env, false)?.into_value());
    }

    // Body as a single expression (wrap in do if multiple)
    let body: &Sexp = if body_exprs.len() == 1 {
        &body_exprs[0]
    } else {
        // We could create a temporary Sexp::Do, but using implicit do is cleaner
        // by wrapping in a list
        return Err(EvalError::InvalidForm(
            "loop body must be a single expression (use do for multiple)".into(),
        ));
    };

    // Loop
    let num_bindings = names.len();
    loop {
        let loop_env = Arc::new(Env::new(Some(env.clone())));
        for (i, name) in names.iter().enumerate() {
            loop_env.set(name.clone(), values[i].clone());
        }
        match eval_fn(body, &loop_env, true)? {
            TailResult::Value(v) => return Ok(TailResult::Value(v)),
            TailResult::Recur(new_args) => {
                if new_args.len() != num_bindings {
                    return Err(EvalError::WrongArgCount {
                        expected: num_bindings,
                        got: new_args.len(),
                    });
                }
                values = new_args.into_iter().collect();
                // Continue the loop
            }
        }
    }
}

fn do_recur<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if !tail {
        return Err(EvalError::RecurNotTail);
    }
    let mut recur_args = Vector::new();
    for arg in args {
        match eval_fn(arg, env, false)? {
            TailResult::Value(v) => recur_args.push_back(v),
            _ => return Err(EvalError::InvalidForm("recur in recur args".into())),
        }
    }
    Ok(TailResult::Recur(recur_args))
}

// ── defmacro ─────────────────────────────────────────────────────

fn do_defmacro(
    args: &[Sexp],
    env: &Arc<Env>,
    _eval_fn: &EvalFn,
) -> Result<TailResult, EvalError> {
    if args.len() < 3 {
        return Err(EvalError::WrongArgCountMin {
            min: 3,
            got: args.len(),
        });
    }
    let name = match &args[0] {
        Sexp::Symbol(s) => s.clone(),
        other => {
            return Err(EvalError::TypeError {
                expected: "symbol",
                got: other.kind().to_string(),
            })
        }
    };
    let (params, rest_param) = parse_params(&args[1])?;
    let body = if args.len() == 3 {
        args[2].clone()
    } else {
        Sexp::List(args[2..].iter().cloned().collect())
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

// ── and / or (short-circuit) ────────────────────────────────────

fn do_and<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Boolean(true)));
    }
    for (i, expr) in args.iter().enumerate() {
        let is_last = i == args.len() - 1;
        let val = eval_fn(expr, env, tail && is_last)?;
        if !is_last {
            let v = match &val {
                TailResult::Value(v) => v,
                TailResult::Recur(_) => return Ok(val),
            };
            if !is_truthy(v) {
                return Ok(val);
            }
        } else {
            return Ok(val);
        }
    }
    unreachable!()
}

fn do_or<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }
    for (i, expr) in args.iter().enumerate() {
        let is_last = i == args.len() - 1;
        let val = eval_fn(expr, env, tail && is_last)?;
        if !is_last {
            let v = match &val {
                TailResult::Value(v) => v,
                TailResult::Recur(_) => return Ok(val),
            };
            if is_truthy(v) {
                return Ok(val);
            }
        } else {
            return Ok(val);
        }
    }
    unreachable!()
}

// ── cond ─────────────────────────────────────────────────────────

fn do_cond<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }

    // Each clause is (test expr+) or a single Sexp as default
    for (clause_idx, clause) in args.iter().enumerate() {
        let is_last_clause = clause_idx == args.len() - 1;
        match clause {
            Sexp::List(items) if !items.is_empty() => {
                let test = &items[0];
                let body_items: Vec<Sexp> = items.iter().skip(1).cloned().collect();
                let test_val = eval_fn(test, env, false)?.into_value();
                if is_truthy(&test_val) {
                    if body_items.is_empty() {
                        return Ok(TailResult::Value(test_val));
                    }
                    return eval_last_body(&body_items, env, tail && is_last_clause, eval_fn);
                }
                // else: continue to next clause
            }
            // Default clause (not in a list) — must be last
            _ if is_last_clause => {
                return eval_fn(clause, env, tail);
            }
            other => {
                return Err(EvalError::InvalidForm(format!(
                    "cond clause must be a list, got {}",
                    other
                )));
            }
        }
    }
    // No clause matched, no default
    Ok(TailResult::Value(Value::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::env::Env;
    use im::vector;

    /// Helper: create a minimal eval_fn for testing special forms in isolation.
    /// This will panic on any Symbol lookup but handles self-evaluating types.
    fn test_eval(expr: &Sexp, env: &Arc<Env>, _tail: bool) -> Result<TailResult, EvalError> {
        match expr {
            Sexp::Nil => Ok(TailResult::Value(Value::Nil)),
            Sexp::Boolean(b) => Ok(TailResult::Value(Value::Boolean(*b))),
            Sexp::Integer(i) => Ok(TailResult::Value(Value::Integer(*i))),
            Sexp::Float(f) => Ok(TailResult::Value(Value::Float(*f))),
            Sexp::String(s) => Ok(TailResult::Value(Value::String(s.clone()))),
            Sexp::Keyword(k) => Ok(TailResult::Value(Value::Keyword(k.clone()))),
            Sexp::Symbol(s) => env
                .get(s)
                .map(TailResult::Value)
                .ok_or_else(|| EvalError::SymbolNotFound(s.clone())),
            other => Err(EvalError::InvalidForm(format!(
                "test_eval cannot handle {}",
                other
            ))),
        }
    }

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

    #[test]
    fn test_def() {
        let env = Arc::new(Env::new(None));
        let args = [Sexp::Symbol("x".into()), Sexp::Integer(10)];
        let result = do_def(&args, &env, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(10));
        assert_eq!(env.get("x"), Some(Value::Integer(10)));
    }

    #[test]
    fn test_if_true() {
        let env = Arc::new(Env::new(None));
        let args = [
            Sexp::Boolean(true),
            Sexp::Integer(1),
            Sexp::Integer(2),
        ];
        let result = do_if(&args, &env, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(1));
    }

    #[test]
    fn test_if_false() {
        let env = Arc::new(Env::new(None));
        let args = [
            Sexp::Boolean(false),
            Sexp::Integer(1),
            Sexp::Integer(2),
        ];
        let result = do_if(&args, &env, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(2));
    }

    #[test]
    fn test_fn_creation() {
        let env = Arc::new(Env::new(None));
        let params = Sexp::Vector(vector![Sexp::Symbol("x".into())]);
        let body = Sexp::Symbol("x".into());
        let args = [params, body];
        let result = do_fn(&args, &env).unwrap().into_value();
        match result {
            Value::Function(_) => {} // OK
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn test_fn_with_rest() {
        let env = Arc::new(Env::new(None));
        let params = Sexp::Vector(vector![
            Sexp::Symbol("a".into()),
            Sexp::Symbol("&".into()),
            Sexp::Symbol("rest".into()),
        ]);
        let args = [params, Sexp::Integer(1)];
        let result = do_fn(&args, &env).unwrap().into_value();
        match result {
            Value::Function(f) => {
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.rest_param, Some("rest".to_string()));
            }
            _ => panic!("expected function"),
        }
    }

    #[test]
    fn test_let_bindings() {
        let env = Arc::new(Env::new(None));
        let bindings = Sexp::Vector(vector![
            Sexp::Symbol("x".into()),
            Sexp::Integer(10),
            Sexp::Symbol("y".into()),
            Sexp::Integer(20),
        ]);
        let body = Sexp::Symbol("x".into());
        let args = [bindings, body];
        let result = do_let(&args, &env, false, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(10));
    }

    #[test]
    fn test_parse_params() {
        let params = Sexp::Vector(vector![Sexp::Symbol("a".into()), Sexp::Symbol("b".into())]);
        let (names, rest) = parse_params(&params).unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(rest, None);

        let params = Sexp::Vector(vector![
            Sexp::Symbol("a".into()),
            Sexp::Symbol("&".into()),
            Sexp::Symbol("rest".into()),
        ]);
        let (names, rest) = parse_params(&params).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(rest, Some("rest".to_string()));
    }

    #[test]
    fn test_and_or_short_circuit() {
        let env = Arc::new(Env::new(None));
        let args = [Sexp::Boolean(true), Sexp::Integer(42)];
        let result = do_and(&args, &env, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(42));

        let args = [Sexp::Boolean(false), Sexp::Integer(42)];
        let result = do_and(&args, &env, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Boolean(false));

        let args = [Sexp::Boolean(false), Sexp::Integer(42)];
        let result = do_or(&args, &env, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(42));
    }

    #[test]
    fn test_cond() {
        let env = Arc::new(Env::new(None));
        // (cond (false 1) (true 2) (false 3))
        let args = [
            Sexp::List(vector![Sexp::Boolean(false), Sexp::Integer(1)]),
            Sexp::List(vector![Sexp::Boolean(true), Sexp::Integer(2)]),
            Sexp::List(vector![Sexp::Boolean(false), Sexp::Integer(3)]),
        ];
        let result = do_cond(&args, &env, false, &test_eval)
            .unwrap()
            .into_value();
        assert_eq!(result, Value::Integer(2));
    }
}
