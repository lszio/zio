use std::sync::Arc;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{eval_last_body, TailResult};
use crate::value::{is_truthy, Value};

// ── if ────────────────────────────────────────────────────────────

pub fn do_if(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.len() < 2 || args.len() > 3 {
        return Err(EvalError::wrong_arg_count(2, args.len()));
    }
    let test = engine.eval_expr(&args[0], env, false)?;
    if is_truthy(&test.into_value()) {
        engine.eval_expr(&args[1], env, tail)
    } else if args.len() == 3 {
        engine.eval_expr(&args[2], env, tail)
    } else {
        Ok(TailResult::Value(Value::Nil))
    }
}

// ── do ────────────────────────────────────────────────────────────

pub fn do_do(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    eval_last_body(args, env, tail, engine)
}

// ── and / or (short-circuit) ────────────────────────────────────

pub fn do_and(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Boolean(true)));
    }
    for (i, arg) in args.iter().enumerate() {
        let result = engine.eval_expr(arg, env, tail && i == args.len() - 1)?;
        let val = result.into_value();
        if !is_truthy(&val) {
            return Ok(TailResult::Value(val));
        }
        if i == args.len() - 1 {
            return Ok(TailResult::Value(val));
        }
    }
    Ok(TailResult::Value(Value::Boolean(true)))
}

pub fn do_or(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Ok(TailResult::Value(Value::Nil));
    }
    for (i, arg) in args.iter().enumerate() {
        let result = engine.eval_expr(arg, env, tail && i == args.len() - 1)?;
        let val = result.into_value();
        if is_truthy(&val) {
            return Ok(TailResult::Value(val));
        }
        if i == args.len() - 1 {
            return Ok(TailResult::Value(val));
        }
    }
    Ok(TailResult::Value(Value::Nil))
}

// ── cond ─────────────────────────────────────────────────────────

pub fn do_cond(
    args: &[Sexp],
    env: &Arc<Env>,
    tail: bool,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    for clause in args {
        let list = match clause {
            Sexp::List(list, _) => list,
            other => {
                return Err(EvalError::invalid_form(
                    format!("cond clause must be a list, got {}", other.kind()),
                ));
            }
        };
        if list.is_empty() {
            return Err(EvalError::invalid_form("cond clause cannot be empty"));
        }
        let test = &list[0];
        if let Sexp::Symbol(s, _) = test {
            if s == "else" {
                let body_slice = list.clone().into_iter().skip(1).collect::<Vec<Sexp>>();
                return eval_last_body(&body_slice, env, tail, engine);
            }
        }
        let test_result = engine.eval_expr(test, env, false)?.into_value();
        if is_truthy(&test_result) {
            let body_slice = list.clone().into_iter().skip(1).collect::<Vec<Sexp>>();
            return eval_last_body(&body_slice, env, tail, engine);
        }
    }
    Ok(TailResult::Value(Value::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::EvalContext;
    use crate::env::Env;
    use crate::builtins;
    use crate::eval;
    use crate::sexp::Sexp;

    fn make_ctx() -> EvalContext {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        EvalContext::new(env)
    }

    fn parse(s: &str) -> Sexp {
        // Minimal inline parser for tests — duplicates test_parse_atom from eval tests
        fn test_tokenize(input: &str) -> Vec<String> {
            let mut tokens = Vec::new();
            let mut chars = input.chars().peekable();
            while let Some(&c) = chars.peek() {
                match c {
                    '(' | ')' | '[' | ']' | '{' | '}' | '\'' => {
                        tokens.push(c.to_string());
                        chars.next();
                    }
                    '"' => {
                        let mut s = String::new();
                        s.push(chars.next().unwrap());
                        while let Some(&c) = chars.peek() {
                            match c { '\\' => { s.push(chars.next().unwrap()); if let Some(nc) = chars.next() { s.push(nc); } } '"' => { s.push(chars.next().unwrap()); break; } _ => { s.push(chars.next().unwrap()); } }
                        }
                        tokens.push(s);
                    }
                    _ if c.is_whitespace() => { chars.next(); }
                    ';' => { while let Some(c) = chars.next() { if c == '\n' { break; } } }
                    _ => {
                        let mut atom = String::new();
                        while let Some(&c) = chars.peek() {
                            if c.is_whitespace() || "()[]{}'\"".contains(c) || c == ';' { break; }
                            atom.push(chars.next().unwrap());
                        }
                        if !atom.is_empty() { tokens.push(atom); }
                    }
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
                    if c == '\\' {
                        if let Some(nc) = chars.next() {
                            match nc { 'n' => unescaped.push('\n'), 'r' => unescaped.push('\r'), 't' => unescaped.push('\t'), '\\' => unescaped.push('\\'), '"' => unescaped.push('"'), _ => unescaped.push(nc), }
                        }
                    } else { unescaped.push(c); }
                }
                Sexp::String(unescaped, None)
            } else if token.starts_with(':') { Sexp::Keyword(token[1..].to_string(), None) }
            else { match token { "nil" => Sexp::Nil, "true" => Sexp::Boolean(true), "false" => Sexp::Boolean(false), _ => { if let Ok(i) = token.parse::<i64>() { Sexp::Integer(i, None) } else if let Ok(f) = token.parse::<f64>() { Sexp::Float(f, None) } else { Sexp::Symbol(token.to_string(), None) } } } }
        }
        fn read_sexp(tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>) -> Result<Sexp, ()> {
            let mut stack: Vec<(String, im::Vector<Sexp>)> = Vec::new();
            while let Some(token) = tokens.next() {
                match token.as_str() {
                    "'" => { let inner = read_sexp(tokens)?; let quoted = Sexp::List(im::vector![Sexp::Symbol("quote".into(), None), inner], None); if let Some(parent) = stack.last_mut() { parent.1.push_back(quoted); } else { return Ok(quoted); } }
                    "(" | "[" | "{" => { stack.push((token, im::Vector::new())); }
                    ")" => { let (_, items) = stack.pop().ok_or(())?; let val = Sexp::List(items, None); if let Some(parent) = stack.last_mut() { parent.1.push_back(val); } else { return Ok(val); } }
                    "]" => { let (_, items) = stack.pop().ok_or(())?; let val = Sexp::Vector(items, None); if let Some(parent) = stack.last_mut() { parent.1.push_back(val); } else { return Ok(val); } }
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
    fn test_if_true() {
        let ctx = make_ctx();
        let expr = parse("(if true 1 2)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(1));
    }

    #[test]
    fn test_if_false() {
        let ctx = make_ctx();
        let expr = parse("(if false 1 2)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(2));
    }

    #[test]
    fn test_if_no_else() {
        let ctx = make_ctx();
        let expr = parse("(if false 1)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Nil);
    }

    #[test]
    fn test_do_sequence() {
        let ctx = make_ctx();
        let expr = parse("(do 1 2 3)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(3));
    }

    #[test]
    fn test_and_empty() {
        let ctx = make_ctx();
        let expr = parse("(and)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Boolean(true));
    }

    #[test]
    fn test_and_short_circuit() {
        let ctx = make_ctx();
        let expr = parse("(and true false true)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Boolean(false));
    }

    #[test]
    fn test_or_empty() {
        let ctx = make_ctx();
        let expr = parse("(or)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Nil);
    }

    #[test]
    fn test_or_short_circuit() {
        let ctx = make_ctx();
        let expr = parse("(or false 42)");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(42));
    }

    #[test]
    fn test_cond() {
        let ctx = make_ctx();
        let expr = parse("(cond (false 1) (true 2) (else 3))");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(2));
    }

    #[test]
    fn test_cond_else() {
        let ctx = make_ctx();
        let expr = parse("(cond (false 1) (else 2 3))");
        assert_eq!(eval::eval(&expr, &ctx).unwrap(), Value::Integer(3));
    }
}