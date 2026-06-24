use std::sync::Arc;

use im::Vector;

use crate::context::{EvalContext, EvalEngine};
use crate::env::Env;
use crate::error::EvalError;
use crate::macros;
use crate::sexp::Sexp;
use crate::special::{eval_special_form, TailResult};
use crate::value::Value;

// ── EvalEngine implementation for EvalContext ────────────────────

impl EvalEngine for EvalContext {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool) -> Result<TailResult, EvalError> {
        eval_inner(expr, env, tail, self)
    }

    fn env(&self) -> &Arc<Env> {
        &self.env
    }
    fn register_module(&self, m: crate::module::Module) {
        self.modules.borrow_mut().register(m);
    }

    fn find_module(&self, name: &[String]) -> Option<crate::module::Module> {
        self.modules.borrow().find(name).cloned()
    }

    fn is_module_loaded(&self, name: &[String]) -> bool {
        self.modules.borrow().is_loaded(name)
    }

    fn call_loader(
        &self,
        name: &[String],
        source: &str,
        parent_env: &Arc<Env>,
    ) -> Option<Result<crate::module::Module, EvalError>> {
        let guard = self.loader.borrow();
        guard.as_ref().map(|loader| loader(name, source, parent_env))
    }

    fn begin_loading(&self, path: &std::path::Path) -> Result<(), EvalError> {
        self.modules.borrow_mut().begin_loading(path)
    }

    fn end_loading(&self, path: &std::path::Path) {
        self.modules.borrow_mut().end_loading(path);
    }
}

/// Public API: evaluate an S-expression in the given context.
pub fn eval(expr: &Sexp, ctx: &dyn EvalEngine) -> Result<Value, EvalError> {
    let result = eval_inner(expr, ctx.env(), false, ctx)?;
    match result {
        TailResult::Value(v) => Ok(v),
        TailResult::Recur(_) => Err(EvalError::custom(
            "recur without loop frame".to_string(),
        )),
    }
}

/// Public convenience: evaluate with an EvalContext directly.
pub fn eval_in_context(expr: &Sexp, ctx: &EvalContext) -> Result<Value, EvalError> {
    eval(expr, ctx)
}

/// Backward-compatible convenience: evaluate with a bare env (no context).
/// Creates a throwaway EvalContext internally.
pub fn eval_bare(expr: &Sexp, env: &Arc<Env>) -> Result<Value, EvalError> {
    let ctx = EvalContext::new(env.clone());
    let result = eval_inner(expr, env, false, &ctx)?;
    match result {
        TailResult::Value(v) => Ok(v),
        TailResult::Recur(_) => Err(EvalError::custom("recur without loop frame")),
    }
}

/// Internal evaluator with tail-position tracking.
fn eval_inner(expr: &Sexp, env: &Arc<Env>, tail: bool, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
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
            .ok_or_else(|| EvalError::symbol_not_found(s.clone())),

        // Empty list evaluates to nil
        Sexp::List(list) if list.is_empty() => Ok(TailResult::Value(Value::Nil)),

        // List: special form, macro, or function call
        Sexp::List(list) => {
            let first = &list[0];
            let args: Vec<Sexp> = list.iter().skip(1).cloned().collect();

            // Check if it's a symbol naming a special form
            if let Sexp::Symbol(name) = first {
                if let Some(result) =
                    eval_special_form(name, &args, env, tail, engine)?
                {
                    return Ok(result);
                }
            }

            // Evaluate the first element to get the function/macro value
            let func_val = eval_inner(first, env, false, engine)?.into_value();

            // Check if it's a macro — expand and re-evaluate
            if let Value::Macro(m) = &func_val {
                // Try symbol-based expansion (fast path)
                if let Sexp::Symbol(name) = first {
                    if let Some(expanded) = macros::try_expand_by_name(name, &args, env, engine)? {
                        return eval_inner(&expanded, env, tail, engine);
                    }
                }
                // Fallback: expand from the macro value directly
                // This handles ((fn [] my-macro) arg1 arg2) patterns
                let expanded = macros::apply_macro_for_value(m, &args, env, engine)?;
                return eval_inner(&expanded, env, tail, engine);
            }

            // Evaluate arguments
            let mut evaled_args = Vector::new();
            for item in &args {
                evaled_args.push_back(eval_inner(item, env, false, engine)?.into_value());
            }

            // Apply the function
            apply(func_val, evaled_args, engine)
        }

        // Vector: evaluate each element
        Sexp::Vector(v) => {
            let mut new_v = Vector::new();
            for item in v {
                new_v.push_back(eval_inner(item, env, false, engine)?.into_value());
            }
            Ok(TailResult::Value(Value::Vector(new_v)))
        }

        // Map: evaluate each key and value
        Sexp::Map(m) => {
            let mut new_m = im::HashMap::new();
            for (k, v) in m {
                let k_val = eval_inner(k, env, false, engine)?.into_value();
                let v_val = eval_inner(v, env, false, engine)?.into_value();
                new_m.insert(k_val, v_val);
            }
            Ok(TailResult::Value(Value::Map(new_m)))
        }
    }
}

/// Apply a function value to arguments.
pub(crate) fn apply(func: Value, args: Vector<Value>, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    match func {
        Value::Function(f) => {
            let env = if f.rest_param.is_some() {
                Env::bind_variadic(&f.env, &f.params, &f.rest_param, &args)?
            } else {
                Env::bind(&f.env, &f.params, &args)?
            };
            engine.eval_expr(&f.body, &env, true)
        }
        Value::NativeFunction(nf) => {
            let result = nf.call(args, engine)?;
            Ok(TailResult::Value(result))
        }
        other => Err(EvalError::not_a_function(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::vector;
    use crate::builtins;
    use crate::env::Env;

    /// Re-export of `parse` for other test modules (control, letloop).
    pub fn parse_for_tests(input: &str) -> Sexp {
        parse(input)
    }
    use crate::error::ReaderError;

    /// Minimal S-expression reader for core crate tests.
    /// Avoids a circular dev-dependency on zio-reader.
    pub fn parse(input: &str) -> Sexp {
        test_read(input).unwrap()
    }

    fn test_read(input: &str) -> Result<Sexp, ReaderError> {
        let tokens = test_tokenize(input);
        let mut tokens = tokens.into_iter().peekable();
        let (sexp, _) = test_read_tokens(&mut tokens)?;
        Ok(sexp)
    }

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
                        match c {
                            '\\' => { s.push(chars.next().unwrap()); if let Some(nc) = chars.next() { s.push(nc); } }
                            '"' => { s.push(chars.next().unwrap()); break; }
                            _ => { s.push(chars.next().unwrap()); }
                        }
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

    fn test_read_tokens(tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>) -> Result<(Sexp, bool), ReaderError> {
        let mut stack: Vec<(String, im::Vector<Sexp>)> = Vec::new();
        while let Some(token) = tokens.next() {
            match token.as_str() {
                "'" => {
                    let (inner, _) = test_read_tokens(tokens)?;
                    let quoted = Sexp::List(im::vector![Sexp::Symbol("quote".into()), inner]);
                    if let Some(parent) = stack.last_mut() { parent.1.push_back(quoted); }
                    else { return Ok((quoted, tokens.peek().is_some())); }
                }
                "(" | "[" | "{" => { stack.push((token, im::Vector::new())); }
                ")" | "]" | "}" => {
                    let (open, items) = stack.pop().ok_or_else(|| ReaderError::UnexpectedToken(token.clone()))?;
                    if (open == "(" && token != ")") || (open == "[" && token != "]") || (open == "{" && token != "}") {
                        return Err(ReaderError::UnexpectedToken(token));
                    }
                    let val = match open.as_str() {
                        "(" => Sexp::List(items),
                        "[" => Sexp::Vector(items),
                        "{" => {
                            if items.len() % 2 != 0 { return Err(ReaderError::OddMapElements); }
                            let mut map = im::HashMap::new();
                            let mut iter = items.into_iter();
                            while let Some(key) = iter.next() { let val = iter.next().unwrap(); map.insert(key, val); }
                            Sexp::Map(map)
                        }
                        _ => unreachable!(),
                    };
                    if let Some(parent) = stack.last_mut() { parent.1.push_back(val); }
                    else { return Ok((val, tokens.peek().is_some())); }
                }
                _ => {
                    let val = test_parse_atom(&token);
                    if let Some(parent) = stack.last_mut() { parent.1.push_back(val); }
                    else { return Ok((val, tokens.peek().is_some())); }
                }
            }
        }
        if !stack.is_empty() { Err(ReaderError::UnexpectedEOF) }
        else { Ok((Sexp::Nil, false)) }
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
            Sexp::String(unescaped)
        } else if token.starts_with(':') { Sexp::Keyword(token[1..].to_string()) }
        else {
            match token {
                "nil" => Sexp::Nil,
                "true" => Sexp::Boolean(true),
                "false" => Sexp::Boolean(false),
                _ => {
                    if let Ok(i) = token.parse::<i64>() { Sexp::Integer(i) }
                    else if let Ok(f) = token.parse::<f64>() { Sexp::Float(f) }
                    else { Sexp::Symbol(token.to_string()) }
                }
            }
        }
    }

    fn make_ctx() -> EvalContext {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        EvalContext::new(env)
    }

    fn run(program: &str) -> Result<Value, EvalError> {
        let ctx = make_ctx();
        let sexp = test_read(program).map_err(|e| EvalError::custom(e.to_string()))?;
        eval_in_context(&sexp, &ctx)
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
        let ctx = make_ctx();
        let sexp = test_read("(def x 42)").unwrap();
        assert_eq!(eval_in_context(&sexp, &ctx).unwrap(), Value::Integer(42));
        assert_eq!(ctx.env.get("x"), Some(Value::Integer(42)));
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
        assert_eq!(run("((fn [x] (+ x 1)) 41)").unwrap(), Value::Integer(42));
    }

    #[test]
    fn test_closure() {
        let ctx = make_ctx();
        let s = test_read("(def make-adder (fn [x] (fn [y] (+ x y))))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(def add10 (make-adder 10))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(add10 5)").unwrap();
        assert_eq!(eval_in_context(&s, &ctx).unwrap(), Value::Integer(15));
    }

    #[test]
    fn test_recursion() {
        let ctx = make_ctx();
        let program = r#"
            (def fib (fn [n]
                (if (< n 2)
                    n
                    (+ (fib (- n 1)) (fib (- n 2))))))
        "#;
        let s = test_read(program).unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(fib 10)").unwrap();
        assert_eq!(eval_in_context(&s, &ctx).unwrap(), Value::Integer(55));
    }

    #[test]
    fn test_defn() {
        let ctx = make_ctx();
        let s = test_read("(defn add [a b] (+ a b))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(add 3 4)").unwrap();
        assert_eq!(eval_in_context(&s, &ctx).unwrap(), Value::Integer(7));
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
        let ctx = make_ctx();
        let s = test_read(
            "(defmacro unless [test body] (list 'if test nil body))",
        ).unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(unless false 42)").unwrap();
        assert_eq!(eval_in_context(&s, &ctx).unwrap(), Value::Integer(42));
    }

    #[test]
    fn test_variadic_fn() {
        let ctx = make_ctx();
        let s = test_read("(defn sum [& nums] (reduce + 0 nums))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(sum 1 2 3 4)").unwrap();
        assert_eq!(eval_in_context(&s, &ctx).unwrap(), Value::Integer(10));
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

    #[test]
    fn test_eval_context_construction() {
        let _ctx = make_ctx();
    }
}