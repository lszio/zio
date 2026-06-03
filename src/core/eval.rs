use crate::core::value::Value;
use crate::core::env::Env;
use std::sync::Arc;
use im::Vector;

pub fn eval(val: Value, env: Arc<Env>) -> Result<Value, String> {
    match val {
        Value::List(list) => {
            if list.is_empty() {
                return Ok(Value::List(list));
            }

            let first = &list[0];
            match first {
                Value::Symbol(s) if s == "quote" => {
                    if list.len() != 2 {
                        return Err("quote requires exactly one argument".to_string());
                    }
                    Ok(list[1].clone())
                }
                Value::Symbol(s) if s == "def" => {
                    if list.len() != 3 {
                        return Err("def requires exactly two arguments".to_string());
                    }
                    let key = match &list[1] {
                        Value::Symbol(s) => s.clone(),
                        _ => return Err("def first argument must be a symbol".to_string()),
                    };
                    let val = eval(list[2].clone(), env.clone())?;
                    env.set(key, val.clone());
                    Ok(val)
                }
                Value::Symbol(s) if s == "if" => {
                    if list.len() < 3 || list.len() > 4 {
                        return Err("if requires 2 or 3 arguments".to_string());
                    }
                    let cond = eval(list[1].clone(), env.clone())?;
                    match cond {
                        Value::Nil | Value::Boolean(false) => {
                            if list.len() == 4 {
                                eval(list[3].clone(), env)
                            } else {
                                Ok(Value::Nil)
                            }
                        }
                        _ => eval(list[2].clone(), env),
                    }
                }
                Value::Symbol(s) if s == "do" => {
                    let mut res = Value::Nil;
                    for i in 1..list.len() {
                        res = eval(list[i].clone(), env.clone())?;
                    }
                    Ok(res)
                }
                Value::Symbol(s) if s == "fn" => {
                    if list.len() != 3 {
                        return Err("fn requires exactly two arguments (params and body)".to_string());
                    }
                    let params_list = match &list[1] {
                        Value::List(l) | Value::Vector(l) => l,
                        _ => return Err("fn params must be a list or vector".to_string()),
                    };
                    let mut params = Vector::new();
                    for p in params_list {
                        match p {
                            Value::Symbol(s) => params.push_back(s.clone()),
                            _ => return Err("fn params must be symbols".to_string()),
                        }
                    }
                    Ok(Value::Function(Arc::new(crate::core::value::Function {
                        params,
                        body: Arc::new(list[2].clone()),
                        env: env.clone(),
                    })))
                }
                _ => {
                    let func = eval(list[0].clone(), env.clone())?;
                    let mut args = Vector::new();
                    for i in 1..list.len() {
                        args.push_back(eval(list[i].clone(), env.clone())?);
                    }
                    apply(func, args)
                }
            }
        }
        Value::Nil => Ok(Value::Nil),
        Value::Boolean(b) => Ok(Value::Boolean(b)),
        Value::Integer(i) => Ok(Value::Integer(i)),
        Value::Float(f) => Ok(Value::Float(f)),
        Value::String(s) => Ok(Value::String(s)),
        Value::Keyword(k) => Ok(Value::Keyword(k)),
        Value::Symbol(s) => env.get(&s).ok_or_else(|| format!("Symbol not found: {}", s)),
        Value::Vector(v) => {
            let mut new_v = Vector::new();
            for item in v {
                new_v.push_back(eval(item, env.clone())?);
            }
            Ok(Value::Vector(new_v))
        }
        Value::Map(m) => {
            let mut new_m = im::HashMap::new();
            for (k, v) in m {
                new_m.insert(eval(k, env.clone())?, eval(v, env.clone())?);
            }
            Ok(Value::Map(new_m))
        }
        Value::Function(_) | Value::NativeFunction(_) => Ok(val),
    }
}

pub fn apply(func: Value, args: Vector<Value>) -> Result<Value, String> {
    match func {
        Value::Function(f) => {
            let env = Env::bind(f.env.clone(), f.params.clone(), args)?;
            eval((*f.body).clone(), env)
        }
        Value::NativeFunction(f) => f(args),
        _ => Err(format!("Value is not a function: {:?}", func)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::env::Env;
    use im::vector;

    #[test]
    fn test_eval_self_evaluating() {
        let env = Arc::new(Env::new(None));
        assert_eq!(eval(Value::Nil, env.clone()), Ok(Value::Nil));
        assert_eq!(eval(Value::Boolean(true), env.clone()), Ok(Value::Boolean(true)));
        assert_eq!(eval(Value::Integer(42), env.clone()), Ok(Value::Integer(42)));
        assert_eq!(eval(Value::Float(3.14), env.clone()), Ok(Value::Float(3.14)));
        assert_eq!(eval(Value::String("hello".into()), env.clone()), Ok(Value::String("hello".into())));
        assert_eq!(eval(Value::Keyword("foo".into()), env.clone()), Ok(Value::Keyword("foo".into())));
    }

    #[test]
    fn test_eval_symbol() {
        let env = Arc::new(Env::new(None));
        env.set("x".to_string(), Value::Integer(10));
        assert_eq!(eval(Value::Symbol("x".to_string()), env.clone()), Ok(Value::Integer(10)));
        assert_eq!(eval(Value::Symbol("y".to_string()), env.clone()), Err("Symbol not found: y".to_string()));
    }

    #[test]
    fn test_eval_quote() {
        let env = Arc::new(Env::new(None));
        let quoted = Value::List(vector![Value::Symbol("quote".to_string()), Value::Integer(42)]);
        assert_eq!(eval(quoted, env.clone()), Ok(Value::Integer(42)));

        let quoted_list = Value::List(vector![Value::Symbol("quote".to_string()), Value::List(vector![Value::Integer(1), Value::Integer(2)])]);
        assert_eq!(eval(quoted_list, env.clone()), Ok(Value::List(vector![Value::Integer(1), Value::Integer(2)])));
    }

    #[test]
    fn test_eval_def() {
        let env = Arc::new(Env::new(None));
        let def_expr = Value::List(vector![Value::Symbol("def".to_string()), Value::Symbol("x".to_string()), Value::Integer(42)]);
        assert_eq!(eval(def_expr, env.clone()), Ok(Value::Integer(42)));
        assert_eq!(env.get("x"), Some(Value::Integer(42)));
    }

    #[test]
    fn test_eval_if() {
        let env = Arc::new(Env::new(None));
        let if_true = Value::List(vector![Value::Symbol("if".to_string()), Value::Boolean(true), Value::Integer(1), Value::Integer(2)]);
        assert_eq!(eval(if_true, env.clone()), Ok(Value::Integer(1)));

        let if_false = Value::List(vector![Value::Symbol("if".to_string()), Value::Boolean(false), Value::Integer(1), Value::Integer(2)]);
        assert_eq!(eval(if_false, env.clone()), Ok(Value::Integer(2)));

        let if_nil = Value::List(vector![Value::Symbol("if".to_string()), Value::Nil, Value::Integer(1), Value::Integer(2)]);
        assert_eq!(eval(if_nil, env.clone()), Ok(Value::Integer(2)));
    }

    #[test]
    fn test_eval_do() {
        let env = Arc::new(Env::new(None));
        let do_expr = Value::List(vector![
            Value::Symbol("do".to_string()),
            Value::List(vector![Value::Symbol("def".to_string()), Value::Symbol("x".to_string()), Value::Integer(1)]),
            Value::List(vector![Value::Symbol("def".to_string()), Value::Symbol("x".to_string()), Value::Integer(2)]),
            Value::Symbol("x".to_string())
        ]);
        assert_eq!(eval(do_expr, env.clone()), Ok(Value::Integer(2)));
    }

    #[test]
    fn test_eval_fn() {
        let env = Arc::new(Env::new(None));
        crate::core::builtins::setup_env(env.clone());

        // (def add1 (fn (x) (+ x 1)))
        let fn_expr = Value::List(vector![
            Value::Symbol("fn".to_string()),
            Value::List(vector![Value::Symbol("x".to_string())]),
            Value::List(vector![Value::Symbol("+".to_string()), Value::Symbol("x".to_string()), Value::Integer(1)])
        ]);
        let def_expr = Value::List(vector![Value::Symbol("def".to_string()), Value::Symbol("add1".to_string()), fn_expr]);
        eval(def_expr, env.clone()).unwrap();

        // (add1 10)
        let call_expr = Value::List(vector![Value::Symbol("add1".to_string()), Value::Integer(10)]);
        assert_eq!(eval(call_expr, env.clone()), Ok(Value::Integer(11)));
    }

    #[test]
    fn test_eval_closure() {
        let env = Arc::new(Env::new(None));
        crate::core::builtins::setup_env(env.clone());

        // (def make-adder (fn (x) (fn (y) (+ x y))))
        let make_adder = Value::List(vector![
            Value::Symbol("fn".to_string()),
            Value::List(vector![Value::Symbol("x".to_string())]),
            Value::List(vector![
                Value::Symbol("fn".to_string()),
                Value::List(vector![Value::Symbol("y".to_string())]),
                Value::List(vector![Value::Symbol("+".to_string()), Value::Symbol("x".to_string()), Value::Symbol("y".to_string())])
            ])
        ]);
        eval(Value::List(vector![Value::Symbol("def".to_string()), Value::Symbol("make-adder".to_string()), make_adder]), env.clone()).unwrap();

        // (def add10 (make-adder 10))
        eval(Value::List(vector![
            Value::Symbol("def".to_string()),
            Value::Symbol("add10".to_string()),
            Value::List(vector![Value::Symbol("make-adder".to_string()), Value::Integer(10)])
        ]), env.clone()).unwrap();

        // (add10 5)
        assert_eq!(eval(Value::List(vector![Value::Symbol("add10".to_string()), Value::Integer(5)]), env.clone()), Ok(Value::Integer(15)));
    }

    #[test]
    fn test_eval_recursion() {
        let env = Arc::new(Env::new(None));
        crate::core::builtins::setup_env(env.clone());

        // (def fib (fn (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))))
        let fib_body = Value::List(vector![
            Value::Symbol("if".to_string()),
            Value::List(vector![Value::Symbol("<".to_string()), Value::Symbol("n".to_string()), Value::Integer(2)]),
            Value::Symbol("n".to_string()),
            Value::List(vector![
                Value::Symbol("+".to_string()),
                Value::List(vector![Value::Symbol("fib".to_string()), Value::List(vector![Value::Symbol("-".to_string()), Value::Symbol("n".to_string()), Value::Integer(1)])]),
                Value::List(vector![Value::Symbol("fib".to_string()), Value::List(vector![Value::Symbol("-".to_string()), Value::Symbol("n".to_string()), Value::Integer(2)])])
            ])
        ]);
        let fib_fn = Value::List(vector![
            Value::Symbol("fn".to_string()),
            Value::List(vector![Value::Symbol("n".to_string())]),
            fib_body
        ]);
        eval(Value::List(vector![Value::Symbol("def".to_string()), Value::Symbol("fib".to_string()), fib_fn]), env.clone()).unwrap();

        // (fib 10) -> 55
        assert_eq!(eval(Value::List(vector![Value::Symbol("fib".to_string()), Value::Integer(10)]), env.clone()), Ok(Value::Integer(55)));
    }
}
