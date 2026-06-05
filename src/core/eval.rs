use std::sync::Arc;

use im::Vector;

use crate::core::env::Env;
use crate::core::error::EvalError;
use crate::core::macros;
use crate::core::sexp::Sexp;
use crate::core::special::{eval_special_form, TailResult};
use crate::core::value::Value;

/// Public API: evaluate an S-expression.
pub fn eval(expr: &Sexp, env: &Arc<Env>) -> Result<Value, EvalError> {
    let result = eval_inner(expr, env, false)?;
    match result {
        TailResult::Value(v) => Ok(v),
        TailResult::Recur(_) => Err(EvalError::Custom(
            "recur without loop frame".to_string(),
        )),
    }
}

/// Internal evaluator with tail-position tracking.
fn eval_inner(expr: &Sexp, env: &Arc<Env>, tail: bool) -> Result<TailResult, EvalError> {
    match expr {
        // Self-evaluating types
        Sexp::Nil => Ok(TailResult::Value(Value::Nil)),
        Sexp::Boolean(b) => Ok(TailResult::Value(Value::Boolean(*b))),
        Sexp::Integer(i) => Ok(TailResult::Value(Value::Integer(*i))),
        Sexp::Float(f) => Ok(TailResult::Value(Value::Float(*f))),
        Sexp::String(s) => Ok(TailResult::Value(Value::String(s.clone()))),
        Sexp::Keyword(k) => Ok(TailResult::Value(Value::Keyword(k.clone()))),

        // Symbol lookup
        Sexp::Symbol(s) => env
            .get(s)
            .map(TailResult::Value)
            .ok_or_else(|| EvalError::SymbolNotFound(s.clone())),

        // Empty list evaluates to nil
        Sexp::List(list) if list.is_empty() => Ok(TailResult::Value(Value::Nil)),

        // List: special form, macro, or function call
        Sexp::List(list) => {
            let first = &list[0];

            // Collect args (skip first element) into Vec<Sexp>
            let args: Vec<Sexp> = list.iter().skip(1).cloned().collect();

            // Check if it's a symbol naming a special form
            if let Sexp::Symbol(name) = first {
                if let Some(result) =
                    eval_special_form(name, &args, env, tail, &|e, env, t| eval_inner(e, env, t))?
                {
                    return Ok(result);
                }
            }

            // Evaluate the first element to get the function/macro value
            let func_val = eval_inner(first, env, false)?.into_value();

            // Check if it's a macro — expand and re-evaluate
            if let Value::Macro(_) = &func_val {
                if let Sexp::Symbol(name) = first {
                    if let Some(expanded) = macros::try_expand_by_name(name, &args, env)? {
                        return eval_inner(&expanded, env, tail);
                    }
                }
            }

            // Evaluate arguments
            let mut evaled_args = Vector::new();
            for item in &args {
                evaled_args.push_back(eval_inner(item, env, false)?.into_value());
            }

            // Apply the function
            apply(func_val, evaled_args)
        }

        // Vector: evaluate each element
        Sexp::Vector(v) => {
            let mut new_v = Vector::new();
            for item in v {
                new_v.push_back(eval_inner(item, env, false)?.into_value());
            }
            Ok(TailResult::Value(Value::Vector(new_v)))
        }

        // Map: evaluate each key and value
        Sexp::Map(m) => {
            let mut new_m = im::HashMap::new();
            for (k, v) in m {
                let k_val = eval_inner(k, env, false)?.into_value();
                let v_val = eval_inner(v, env, false)?.into_value();
                new_m.insert(k_val, v_val);
            }
            Ok(TailResult::Value(Value::Map(new_m)))
        }
    }
}

/// Apply a function value to arguments.
pub fn apply(func: Value, args: Vector<Value>) -> Result<TailResult, EvalError> {
    match func {
        Value::Function(f) => {
            let env = if f.rest_param.is_some() {
                Env::bind_variadic(&f.env, &f.params, &f.rest_param, &args)?
            } else {
                Env::bind(&f.env, &f.params, &args)?
            };
            eval_inner(&f.body, &env, true)
        }
        Value::NativeFunction(nf) => {
            let result = nf.call(args)?;
            Ok(TailResult::Value(result))
        }
        other => Err(EvalError::NotAFunction {
            value: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::vector;
    use crate::core::builtins;
    use crate::core::env::Env;
    use crate::core::reader;

    fn run(program: &str) -> Result<Value, EvalError> {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        let sexp = reader::read(program).map_err(|e| EvalError::Custom(e.to_string()))?;
        eval(&sexp, &env)
    }

    #[test]
    fn test_self_evaluating() {
        assert_eq!(run("nil").unwrap(), Value::Nil);
        assert_eq!(run("true").unwrap(), Value::Boolean(true));
        assert_eq!(run("42").unwrap(), Value::Integer(42));
        assert_eq!(run("3.14").unwrap(), Value::Float(3.14));
    }

    #[test]
    fn test_arithmetic() {
        assert_eq!(run("(+ 1 2 3)").unwrap(), Value::Integer(6));
        assert_eq!(run("(- 10 3)").unwrap(), Value::Integer(7));
        assert_eq!(run("(* 2 3 4)").unwrap(), Value::Integer(24));
        assert_eq!(run("(/ 10 2)").unwrap(), Value::Integer(5));
    }

    #[test]
    fn test_quote() {
        assert_eq!(run("(quote 42)").unwrap(), Value::Integer(42));
        assert_eq!(
            run("(quote (1 2 3))").unwrap(),
            Value::List(vector![Value::Integer(1), Value::Integer(2), Value::Integer(3)])
        );
    }

    #[test]
    fn test_quote_reader() {
        assert_eq!(run("'42").unwrap(), Value::Integer(42));
    }

    #[test]
    fn test_def_and_lookup() {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);

        let sexp = reader::read("(def x 42)").unwrap();
        assert_eq!(eval(&sexp, &env).unwrap(), Value::Integer(42));
        assert_eq!(env.get("x"), Some(Value::Integer(42)));
    }

    #[test]
    fn test_if() {
        assert_eq!(run("(if true 1 2)").unwrap(), Value::Integer(1));
        assert_eq!(run("(if false 1 2)").unwrap(), Value::Integer(2));
        assert_eq!(run("(if nil 1 2)").unwrap(), Value::Integer(2));
        assert_eq!(run("(if false 1)").unwrap(), Value::Nil);
    }

    #[test]
    fn test_do() {
        assert_eq!(run("(do 1 2 3)").unwrap(), Value::Integer(3));
    }

    #[test]
    fn test_fn_apply() {
        assert_eq!(
            run("((fn [x] (+ x 1)) 41)").unwrap(),
            Value::Integer(42)
        );
    }

    #[test]
    fn test_closure() {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);

        // (def make-adder (fn [x] (fn [y] (+ x y))))
        let s = reader::read("(def make-adder (fn [x] (fn [y] (+ x y))))").unwrap();
        eval(&s, &env).unwrap();
        // (def add10 (make-adder 10))
        let s = reader::read("(def add10 (make-adder 10))").unwrap();
        eval(&s, &env).unwrap();
        // (add10 5) => 15
        let s = reader::read("(add10 5)").unwrap();
        assert_eq!(eval(&s, &env).unwrap(), Value::Integer(15));
    }

    #[test]
    fn test_recursion() {
        // Fibonacci via recursion
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);

        let program = r#"
            (def fib (fn [n]
                (if (< n 2)
                    n
                    (+ (fib (- n 1)) (fib (- n 2))))))
        "#;
        let s = reader::read(program).unwrap();
        eval(&s, &env).unwrap();
        let s = reader::read("(fib 10)").unwrap();
        assert_eq!(eval(&s, &env).unwrap(), Value::Integer(55));
    }

    #[test]
    fn test_defn() {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        let s = reader::read("(defn add [a b] (+ a b))").unwrap();
        eval(&s, &env).unwrap();
        let s = reader::read("(add 3 4)").unwrap();
        assert_eq!(eval(&s, &env).unwrap(), Value::Integer(7));
    }

    #[test]
    fn test_let() {
        assert_eq!(run("(let [x 10 y 20] (+ x y))").unwrap(), Value::Integer(30));
    }

    #[test]
    fn test_let_star() {
        assert_eq!(run("(let* [x 1 y (+ x 1)] y)").unwrap(), Value::Integer(2));
    }

    #[test]
    fn test_loop_recur() {
        assert_eq!(
            run("(loop [i 0 acc 0] (if (< i 5) (recur (+ i 1) (+ acc i)) acc))").unwrap(),
            Value::Integer(10)
        );
    }

    #[test]
    fn test_macro() {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);

        // (defmacro unless [test body] (list 'if test nil body))
        let s = reader::read(
            "(defmacro unless [test body] (list 'if test nil body))",
        )
        .unwrap();
        eval(&s, &env).unwrap();

        // (unless false 42)
        let s = reader::read("(unless false 42)").unwrap();
        assert_eq!(eval(&s, &env).unwrap(), Value::Integer(42));
    }

    #[test]
    fn test_variadic_fn() {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        let s = reader::read("(defn sum [& nums] (reduce + 0 nums))").unwrap();
        eval(&s, &env).unwrap();
        let s = reader::read("(sum 1 2 3 4)").unwrap();
        assert_eq!(eval(&s, &env).unwrap(), Value::Integer(10));
    }

    #[test]
    fn test_type_predicates() {
        assert_eq!(run("(nil? nil)").unwrap(), Value::Boolean(true));
        assert_eq!(run("(nil? 42)").unwrap(), Value::Boolean(false));
        assert_eq!(run("(number? 42)").unwrap(), Value::Boolean(true));
        assert_eq!(run("(string? \"hello\")").unwrap(), Value::Boolean(true));
        assert_eq!(run("(fn? +)").unwrap(), Value::Boolean(true));
        assert_eq!(run("(list? (list 1 2))").unwrap(), Value::Boolean(true));
    }

    #[test]
    fn test_map_filter_reduce() {
        assert_eq!(
            run("(map (fn [x] (* x 2)) [1 2 3])").unwrap(),
            Value::List(vector![Value::Integer(2), Value::Integer(4), Value::Integer(6)])
        );
        assert_eq!(
            run("(filter (fn [x] (< x 3)) [1 2 3 4])").unwrap(),
            Value::List(vector![Value::Integer(1), Value::Integer(2)])
        );
        assert_eq!(run("(reduce + 0 [1 2 3 4])").unwrap(), Value::Integer(10));
    }

    #[test]
    fn test_cons_car_cdr() {
        assert_eq!(
            run("(cons 1 (list 2 3))").unwrap(),
            Value::List(vector![Value::Integer(1), Value::Integer(2), Value::Integer(3)])
        );
        assert_eq!(run("(car (list 1 2 3))").unwrap(), Value::Integer(1));
        assert_eq!(
            run("(cdr (list 1 2 3))").unwrap(),
            Value::List(vector![Value::Integer(2), Value::Integer(3)])
        );
    }
}
