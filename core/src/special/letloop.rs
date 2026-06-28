use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{eval_last_body, parse_bindings, TailResult};
use crate::value::Value;

// ── let / let* ────────────────────────────────────────────────────

pub fn do_let(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    do_let_inner(args, env, tail, false, engine)
}

pub fn do_let_star(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    do_let_inner(args, env, tail, true, engine)
}

fn do_let_inner(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    sequential: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::invalid_form("let requires bindings"));
    }
    let (names, inits) = parse_bindings(&args[0])?;
    let body_exprs = &args[1..];
    if body_exprs.is_empty() {
        return Err(EvalError::invalid_form("let requires a body"));
    }

    if sequential {
        // let*: each binding sees previous bindings
        let inner_env = Arc::new(Env::new(Some(env.clone())));
        for (i, name) in names.into_iter().enumerate() {
            let val = engine.eval_expr(inits[i], &inner_env, false)?.into_value();
            inner_env.set(name, val);
        }
        eval_last_body(body_exprs, &inner_env, tail, engine)
    } else {
        // let: evaluate all inits in outer env, then bind in new env
        let mut init_vals = Vec::new();
        for init in &inits {
            init_vals.push(engine.eval_expr(init, env, false)?.into_value());
        }
        let inner_env = Arc::new(Env::new(Some(env.clone())));
        for (name, val) in names.into_iter().zip(init_vals) {
            inner_env.set(name, val);
        }
        eval_last_body(body_exprs, &inner_env, tail, engine)
    }
}

// ── loop / recur ─────────────────────────────────────────────────

pub fn do_loop(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::invalid_form("loop requires bindings"));
    }

    let (names, inits) = parse_bindings(&args[0])?;
    let body_exprs = &args[1..];
    if body_exprs.is_empty() {
        return Err(EvalError::invalid_form("loop requires a body"));
    }

    let mut values: Vec<Value> = Vec::new();
    for init in &inits {
        values.push(engine.eval_expr(init, env, false)?.into_value());
    }

    let body: &Sexp = if body_exprs.len() == 1 {
        &body_exprs[0]
    } else {
        return Err(EvalError::invalid_form(
            "loop body must be a single expression (use do for multiple)",
        ));
    };

    let num_bindings = names.len();
    loop {
        let loop_env = Arc::new(Env::new(Some(env.clone())));
        for (i, name) in names.iter().enumerate() {
            loop_env.set(name.clone(), values[i].clone());
        }
        match engine.eval_expr(body, &loop_env, true)? {
            TailResult::Value(v) => return Ok(TailResult::Value(v)),
            TailResult::Recur(new_args) => {
                if new_args.len() != num_bindings {
                    return Err(EvalError::wrong_arg_count(num_bindings, new_args.len()));
                }
                values = new_args.into_iter().collect();
            }
            TailResult::TailCall(func, args) => {
                return Ok(TailResult::TailCall(func, args));
            }
        }
    }
}

pub fn do_recur(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if !tail {
        return Err(EvalError::recur_not_tail());
    }
    let mut recur_args = Vector::new();
    for arg in args {
        match engine.eval_expr(arg, env, false)? {
            TailResult::Value(v) => recur_args.push_back(v),
            _ => return Err(EvalError::invalid_form("recur in recur args")),
        }
    }
    Ok(TailResult::Recur(recur_args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins;
    use crate::context::EvalContext;
    use crate::env::Env;
    use crate::eval;
    use crate::sexp::Sexp;

    fn make_ctx() -> EvalContext {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        EvalContext::new(env)
    }

    fn parse(s: &str) -> Sexp {
        // Minimal inline parser for tests — duplicates from eval/tests
        fn test_tokenize(input: &str) -> Vec<String> {
            let mut tokens = Vec::new();
            let mut chars = input.chars().peekable();
            while let Some(&c) = chars.peek() {
                match c {
                    '(' | ')' | '[' | ']' | '{' | '}' | '\'' => { tokens.push(c.to_string()); chars.next(); }
                    '"' => { let mut s = String::new(); s.push(chars.next().unwrap()); while let Some(&c) = chars.peek() { match c { '\\' => { s.push(chars.next().unwrap()); if let Some(nc) = chars.next() { s.push(nc); } } '"' => { s.push(chars.next().unwrap()); break; } _ => { s.push(chars.next().unwrap()); } } } tokens.push(s); }
                    _ if c.is_whitespace() => { chars.next(); }
                    ';' => { while let Some(c) = chars.next() { if c == '\n' { break; } } }
                    _ => { let mut atom = String::new(); while let Some(&c) = chars.peek() { if c.is_whitespace() || "()[]{}'\"".contains(c) || c == ';' { break; } atom.push(chars.next().unwrap()); } if !atom.is_empty() { tokens.push(atom); } }
                }
            }
            tokens
        }
        fn test_parse_atom(token: &str) -> Sexp {
            if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
                let s = &token[1..token.len() - 1];
                let mut unescaped = String::new();
                let mut chars = s.chars();
                while let Some(c) = chars.next() {
                    if c == '\\' { if let Some(nc) = chars.next() { match nc { 'n' => unescaped.push('\n'), 'r' => unescaped.push('\r'), 't' => unescaped.push('\t'), '\\' => unescaped.push('\\'), '"' => unescaped.push('"'), _ => unescaped.push(nc), } } }
                    else { unescaped.push(c); }
                }
                Sexp::String(unescaped)
            } else if token.starts_with(':') { Sexp::Keyword(token[1..].to_string()) }
            else { match token { "nil" => Sexp::Nil, "true" => Sexp::Boolean(true), "false" => Sexp::Boolean(false), _ => { if let Ok(i) = token.parse::<i64>() { Sexp::Integer(i) } else if let Ok(f) = token.parse::<f64>() { Sexp::Float(f) } else { Sexp::Symbol(token.to_string()) } } } }
        }
        fn read_sexp(tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>) -> Result<Sexp, ()> {
            let mut stack: Vec<(String, im::Vector<Sexp>)> = Vec::new();
            while let Some(token) = tokens.next() {
                match token.as_str() {
                    "'" => { let inner = read_sexp(tokens)?; let quoted = Sexp::List(im::vector![Sexp::Symbol("quote".into()), inner]); if let Some(parent) = stack.last_mut() { parent.1.push_back(quoted); } else { return Ok(quoted); } }
                    "(" | "[" | "{" => { stack.push((token, im::Vector::new())); }
                    ")" => { let (_, items) = stack.pop().ok_or(())?; let val = Sexp::List(items); if let Some(parent) = stack.last_mut() { parent.1.push_back(val); } else { return Ok(val); } }
                    "]" => { let (_, items) = stack.pop().ok_or(())?; let val = Sexp::Vector(items); if let Some(parent) = stack.last_mut() { parent.1.push_back(val); } else { return Ok(val); } }
                    "}" => { let (_, items) = stack.pop().ok_or(())?; return Err(()); }
                    _ => { let val = test_parse_atom(&token); if let Some(parent) = stack.last_mut() { parent.1.push_back(val); } else { return Ok(val); } }
                }
            }
            Err(())
        }
        let tokens = test_tokenize(s);
        let mut tokens = tokens.into_iter().peekable();
        read_sexp(&mut tokens).unwrap()
    }

    #[test]
    fn test_let() {
        let ctx = make_ctx();
        let expr = parse("(let [x 10 y 20] (+ x y))");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(30));
    }

    #[test]
    fn test_let_star() {
        let ctx = make_ctx();
        let expr = parse("(let* [x 1 y (+ x 1)] y)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(2));
    }

    #[test]
    fn test_loop_recur() {
        let ctx = make_ctx();
        let expr = parse("(loop [i 0 acc 0] (if (< i 5) (recur (+ i 1) (+ acc i)) acc))");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(10));
    }
}