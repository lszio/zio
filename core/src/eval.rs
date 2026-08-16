use std::sync::Arc;

use im::Vector;

use crate::context::{EvalContext, EvalEngine, EvalRuntime, ModuleRegistry};
use crate::env::Env;
use crate::error::EvalError;
use crate::macros;
use crate::sexp::Sexp;
use crate::special::{eval_special_form, TailResult};
use crate::value::Value;

// ── EvalRuntime implementation for EvalContext ───────────────────

impl EvalRuntime for EvalContext {
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool) -> Result<TailResult, EvalError> {
        eval_inner(expr, env, tail, self)
    }

    fn env(&self) -> &Arc<Env> {
        &self.env
    }

    fn io(&self) -> &dyn crate::io::IoHost {
        &*self.io
    }
}

// ── ModuleRegistry implementation for EvalContext ─────────────────

impl ModuleRegistry for EvalContext {
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

// ── EvalEngine marker supertrait for backward compat ──────────────

impl EvalEngine for EvalContext {}

/// Public API: evaluate an S-expression in the given context.
/// Uses a trampoline loop for proper tail-call optimization.
pub fn eval(expr: &Sexp, ctx: &dyn EvalEngine) -> Result<Value, EvalError> {
    let mut result = eval_inner(expr, ctx.env(), false, ctx)?;
    loop {
        match result {
            TailResult::Value(v) => return Ok(v),
            TailResult::Recur(_) => return Err(EvalError::custom("recur without loop frame")),
            TailResult::TailCall(func, args) => result = apply(func, args, ctx)?,
        }
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
    eval(expr, &ctx)
}

/// Internal evaluator with tail-position tracking.
fn eval_inner(expr: &Sexp, env: &Arc<Env>, tail: bool, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
    match expr {
        // Self-evaluating types
        Sexp::Nil => Ok(TailResult::Value(Value::Nil)),
        Sexp::Boolean(b) => Ok(TailResult::Value(Value::Boolean(*b))),
        Sexp::Integer(i, _) => Ok(TailResult::Value(Value::Integer(*i))),
        Sexp::Float(f, _) => Ok(TailResult::Value(Value::Float(*f))),
        Sexp::String(s, _) => Ok(TailResult::Value(Value::String(s.clone()))),
        Sexp::Keyword(k, _) => Ok(TailResult::Value(Value::Keyword(k.clone()))),
        Sexp::Char(c, _) => Ok(TailResult::Value(Value::Char(*c))),

        // Symbol lookup
        Sexp::Symbol(s, span) => env
            .get(s)
            .map(TailResult::Value)
            .ok_or_else(|| EvalError::symbol_not_found(s.clone()).with_opt_span(*span)),

        // Empty list evaluates to nil
        Sexp::List(list, _) if list.is_empty() => Ok(TailResult::Value(Value::Nil)),

        // List: special form, macro, or function call
        Sexp::List(list, _) => {
            let first = &list[0];
            let args: Vec<Sexp> = list.iter().skip(1).cloned().collect();

            // Check if it's a symbol naming a special form
            if let Sexp::Symbol(name, _) = first {
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
                if let Sexp::Symbol(name, _) = first {
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

            // In tail position: defer to outer trampoline for proper TCO.
            // This avoids growing the Rust call stack through mutual recursion
            // (e.g. even?/odd?). The eval() trampoline loop catches TailCall
            // and drives apply() without recursive eval_inner frames.
            if tail {
                return Ok(TailResult::TailCall(func_val, evaled_args));
            }

            // Non-tail: apply directly with inner trampoline for nested TailCalls.
            let mut r = apply(func_val, evaled_args, engine)?;
            loop {
                match r {
                    TailResult::Value(_) => break Ok(r),
                    TailResult::TailCall(f, a) => r = apply(f, a, engine)?,
                    TailResult::Recur(_) => return Err(EvalError::custom("unexpected recur")),
                }
            }
        }

        // Vector: evaluate each element
        Sexp::Vector(v, _) => {
            let mut new_v = Vector::new();
            for item in v {
                new_v.push_back(eval_inner(item, env, false, engine)?.into_value());
            }
            Ok(TailResult::Value(Value::Vector(new_v)))
        }

        // Map: evaluate each key and value
        Sexp::Map(m, _) => {
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
/// Handles GF dispatch for ZOS generic functions.
pub fn apply(func: Value, args: Vector<Value>, engine: &dyn EvalEngine) -> Result<TailResult, EvalError> {
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
        // ZOS Generic Function dispatch with full method combination
        Value::Object(o) => {
            if let Some(gf_obj) = o.as_any().downcast_ref::<crate::special::zos_forms::GFObject>() {
                let mut gf = gf_obj.gf.borrow_mut();
                let arg_classes: Vec<crate::zos::object::ClassRef> = args.iter().map(value_class_ref).collect();
                let dispatch = gf.dispatch(&arg_classes);

                // No applicable methods?
                if dispatch.primary.is_empty() && dispatch.around.is_empty() {
                    return Err(EvalError::custom(format!(
                        "no applicable method for {} with args {:?}",
                        gf.name,
                        arg_classes.iter().map(|c| &c.name).collect::<Vec<_>>()
                    )));
                }

                // Build the method combination chain from innermost to outermost.
                // The innermost step: execute :before → :primary → :after
                let primary_step = build_primary_combination(&dispatch, &args, engine.env())?;

                // Wrap in :around methods (outermost → innermost)
                let chain = if dispatch.around.is_empty() {
                    primary_step
                } else {
                    build_around_wrappers(&dispatch.around, &args, primary_step, engine.env())?
                };

                // chain is a NativeFn. Call it with empty args (args captured in closures)
                let result = chain.call(im::vector![], engine)?;
                Ok(TailResult::Value(result))
            } else {
                Err(EvalError::not_a_function(format!("#<{}>", o.header().class.name)))
            }
        }
        _ => Err(EvalError::not_a_function(other_display(&func))),
    }
}

/// Build the primary combination: :before → :primary → :after
fn build_primary_combination(
    dispatch: &crate::zos::gf::DispatchResult,
    args: &im::Vector<Value>,
    _parent_env: &Arc<Env>,
) -> Result<crate::value::NativeFn, EvalError> {
    let args = args.clone();
    let before = dispatch.before.clone();
    let primary = dispatch.primary.clone();
    let after = dispatch.after.clone();
    Ok(crate::value::NativeFn::new("__primary_combination__", move |_: im::Vector<Value>, engine: &dyn EvalEngine| -> Result<Value, EvalError> {
        // Execute :before methods (most specific first)
        for m in &before {
            let env = if m.body.rest_param.is_some() {
                Env::bind_variadic(&m.body.env, &m.body.params, &m.body.rest_param, &args)?
            } else {
                Env::bind(&m.body.env, &m.body.params, &args)?
            };
            engine.eval_expr(&m.body.body, &env, false)?;
        }

        // Execute :primary methods (most specific first — take the first)
        if let Some(m) = primary.first() {
            let env = if m.body.rest_param.is_some() {
                Env::bind_variadic(&m.body.env, &m.body.params, &m.body.rest_param, &args)?
            } else {
                Env::bind(&m.body.env, &m.body.params, &args)?
            };
            let result = engine.eval_expr(&m.body.body, &env, false)?.into_value();

            // Execute :after methods (least specific first → reverse order)
            for m in after.iter().rev() {
                let env = if m.body.rest_param.is_some() {
                    Env::bind_variadic(&m.body.env, &m.body.params, &m.body.rest_param, &args)?
                } else {
                    Env::bind(&m.body.env, &m.body.params, &args)?
                };
                engine.eval_expr(&m.body.body, &env, false)?;
            }

            Ok(result)
        } else {
            Ok(Value::Nil)
        }
    }))
}

/// Wrap the primary combination in :around methods (outermost first).
/// Each :around method can call (call-next-method) to invoke the next in chain.
fn build_around_wrappers(
    around_methods: &[Arc<crate::zos::gf::Method>],
    args: &im::Vector<Value>,
    inner: crate::value::NativeFn,
    _parent_env: &Arc<Env>,
) -> Result<crate::value::NativeFn, EvalError> {
    let mut chain: crate::value::NativeFn = inner;
    let args = args.clone();
    // Build from innermost to outermost
    for m in around_methods.iter().rev() {
        let prev_chain = chain.clone();
        let args = args.clone();
        let method = Arc::clone(m);

        chain = crate::value::NativeFn::new("__around_method__", move |_: im::Vector<Value>, engine: &dyn EvalEngine| -> Result<Value, EvalError> {
            let env: Arc<Env>;
            if method.body.rest_param.is_some() {
                let e = Env::bind_variadic(&method.body.env, &method.body.params, &method.body.rest_param, &args)?;
                e.set("*next-method*".into(), Value::NativeFunction(prev_chain.clone()));
                env = e;
            } else {
                let e = Env::bind(&method.body.env, &method.body.params, &args)?;
                e.set("*next-method*".into(), Value::NativeFunction(prev_chain.clone()));
                env = e;
            }
            engine.eval_expr(&method.body.body, &env, false).map(|r| r.into_value())
        });
    }

    Ok(chain)
}

/// Get the class ref for a Value (used by GF dispatch).
fn value_class_ref(v: &Value) -> crate::zos::object::ClassRef {
    let name = value_class_name(v);
    Arc::new(crate::zos::object::Class {
        name: name.clone(),
        superclasses: Vec::new(),
        slots: Vec::new(),
        cpl: vec![name],
    })
}

/// Get the class name string for any Value.
fn value_class_name(v: &Value) -> String {
    match v {
        Value::Nil => "Nil".into(),
        Value::Boolean(_) => "Boolean".into(),
        Value::Integer(_) => "Integer".into(),
        Value::Float(_) => "Float".into(),
        Value::String(_) => "String".into(),
        Value::Symbol(_) => "Symbol".into(),
        Value::Keyword(_) => "Keyword".into(),
        Value::List(_) => "List".into(),
        Value::Vector(_) => "Vector".into(),
        Value::Map(_) => "Map".into(),
        Value::Function(_) => "Function".into(),
        Value::NativeFunction(_) => "NativeFunction".into(),
        Value::Macro(_) => "Macro".into(),
        Value::Char(_) => "Character".into(),
        Value::Object(o) => o.header().class.name.clone(),
        Value::Buffer(_) => "Buffer".into(),
        Value::Future(_) => "Future".into(),
        Value::Channel(_) => "Channel".into(),
    }
}

/// Display a value for error messages (non-panicking).
fn other_display(v: &Value) -> String {
    match v {
        Value::Nil => "nil".into(),
        Value::Integer(i) => format!("integer {i}"),
        Value::Float(f) => format!("float {f}"),
        Value::String(s) => format!("string {s:?}"),
        Value::Function(_) => "#<function>".into(),
        Value::NativeFunction(nf) => format!("#<native {}>", nf.name()),
        Value::Macro(m) => format!("#<macro {}>", m.name),
        Value::Object(o) => format!("#<{}>", o.header().class.name),
        other => format!("{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::vector;
    use crate::builtins;
    use crate::env::Env;

    use crate::error::ReaderError;

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
                    let quoted = Sexp::List(im::vector![Sexp::Symbol("quote".into(), None), inner], None);
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
                        "(" => Sexp::List(items, None),
                        "[" => Sexp::Vector(items, None),
                        "{" => {
                            if items.len() % 2 != 0 { return Err(ReaderError::OddMapElements); }
                            let mut map = im::HashMap::new();
                            let mut iter = items.into_iter();
                            while let Some(key) = iter.next() { let val = iter.next().unwrap(); map.insert(key, val); }
                            Sexp::Map(map, None)
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
            Sexp::String(unescaped, None)
        } else if token.starts_with(':') { Sexp::Keyword(token[1..].to_string(), None) }
        else {
            match token {
                "nil" => Sexp::Nil,
                "true" => Sexp::Boolean(true),
                "false" => Sexp::Boolean(false),
                _ => {
                    if let Ok(i) = token.parse::<i64>() { Sexp::Integer(i, None) }
                    else if let Ok(f) = token.parse::<f64>() { Sexp::Float(f, None) }
                    else { Sexp::Symbol(token.to_string(), None) }
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
        assert_eq!(run("3.5").unwrap(), Value::Float(3.5));
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
    fn test_macroexpand_1() {
        let ctx = make_ctx();
        // Define a macro
        let s = test_read("(defmacro unless [test body] (list 'if test nil body))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        // macroexpand-1 the quoted form
        let s = test_read("(macroexpand-1 '(unless false 42))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        // (unless false 42) expands to (if false nil 42)
        // nil and false are runtime values, not symbols
        assert_eq!(
            result,
            Value::List(vector![
                Value::Symbol("if".into()),
                Value::Boolean(false),
                Value::Nil,
                Value::Integer(42)
            ])
        );
    }

    #[test]
    fn test_macroexpand_non_macro() {
        let ctx = make_ctx();
        // macroexpand-1 on a non-macro form returns the form unchanged
        let s = test_read("(macroexpand-1 '(+ 1 2))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(
            result,
            Value::List(vector![
                Value::Symbol("+".into()),
                Value::Integer(1),
                Value::Integer(2)
            ])
        );
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

    #[test]
    fn test_new_builtins() {
        // apply
        assert_eq!(run("(apply + (list 1 2 3))").unwrap(), Value::Integer(6));
        assert_eq!(run("(apply list (list 1 2 3))").unwrap(), Value::List(vector![Value::Integer(1), Value::Integer(2), Value::Integer(3)]));

        // get
        assert_eq!(run("(get (list 10 20 30) 1)").unwrap(), Value::Integer(20));
        assert_eq!(run("(get [1 2 3] 2)").unwrap(), Value::Integer(3));

        // count
        assert_eq!(run("(count (list 1 2 3))").unwrap(), Value::Integer(3));
        assert_eq!(run("(count \"hello\")").unwrap(), Value::Integer(5));

        // type
        assert_eq!(run("(type 42)").unwrap(), Value::Keyword("integer".to_string()));
        assert_eq!(run("(type \"hello\")").unwrap(), Value::Keyword("string".to_string()));
        assert_eq!(run("(type true)").unwrap(), Value::Keyword("boolean".to_string()));
        assert_eq!(run("(type nil)").unwrap(), Value::Keyword("nil".to_string()));
        assert_eq!(run("(type (fn [x] x))").unwrap(), Value::Keyword("fn".to_string()));

        // mod
        assert_eq!(run("(mod 10 3)").unwrap(), Value::Integer(1));
        assert_eq!(run("(mod 7 2)").unwrap(), Value::Integer(1));
        assert_eq!(run("(mod 4 2)").unwrap(), Value::Integer(0));

        // String operations
        assert_eq!(run("(str-trim \"  hello  \")").unwrap(), Value::String("hello".to_string()));
        assert_eq!(
            run("(str-join \", \" \"a\" \"b\" \"c\")").unwrap(),
            Value::String("a, b, c".to_string())
        );
        assert_eq!(
            run("(str-split \",\" \"a,b,c\")").unwrap(),
            Value::Vector(vector![
                Value::String("a".to_string()),
                Value::String("b".to_string()),
                Value::String("c".to_string()),
            ])
        );
        assert_eq!(
            run("(str-contains? \"hello world\" \"world\")").unwrap(),
            Value::Boolean(true)
        );
        assert_eq!(
            run("(str-contains? \"hello world\" \"xyz\")").unwrap(),
            Value::Boolean(false)
        );
        assert_eq!(
            run("(str-starts-with? \"hello\" \"hel\")").unwrap(),
            Value::Boolean(true)
        );
        assert_eq!(
            run("(str-starts-with? \"hello\" \"xyz\")").unwrap(),
            Value::Boolean(false)
        );
        assert_eq!(
            run("(str-ends-with? \"hello\" \"llo\")").unwrap(),
            Value::Boolean(true)
        );
        assert_eq!(
            run("(str-ends-with? \"hello\" \"xyz\")").unwrap(),
            Value::Boolean(false)
        );

        // range
        assert_eq!(run("(range 5)").unwrap(), Value::Vector(vector![
            Value::Integer(0), Value::Integer(1), Value::Integer(2),
            Value::Integer(3), Value::Integer(4),
        ]));
        assert_eq!(run("(range 2 5)").unwrap(), Value::Vector(vector![
            Value::Integer(2), Value::Integer(3), Value::Integer(4),
        ]));

        // sort
        assert_eq!(run("(sort < [3 1 2])").unwrap(), Value::Vector(vector![
            Value::Integer(1), Value::Integer(2), Value::Integer(3),
        ]));
}

    #[test]
    fn test_error_span_display() {
        // Verify that EvalError Display includes span info when available.
        // This tests the with_opt_span mechanism.
        use crate::span::{BytePos, Span, SourceId};
        let span = Span {
            source_id: SourceId::NONE,
            start: BytePos(5),
            end: BytePos(8),
            line: 1,
            col: 6,
        };
        let err = EvalError::symbol_not_found("foo").with_opt_span(Some(span));
        let msg = err.to_string();
        assert!(msg.contains("symbol not found: foo"), "msg: {msg}");
        assert!(msg.contains("at line 1, col 6"), "msg should contain position, got: {msg}");
    }
    #[test]
    fn test_tco_mutual_recursion() {
        // Tail-call optimization for mutual recursion.
        // Without TCO, (even? 100000) overflows the Rust stack.
        let ctx = make_ctx();
        // Builtins don't include zero?/dec, so define them inline.
        let s = test_read("(defn zero? [n] (= n 0))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defn dec [n] (- n 1))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read(
            "(defn even? [n] (if (zero? n) true (odd? (dec n))))",
        )
        .unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read(
            "(defn odd? [n] (if (zero? n) false (even? (dec n))))",
        )
        .unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(even? 100000)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result.to_string(), "true");
        let s = test_read("(odd? 100001)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result.to_string(), "true");
    }

    #[test]
    fn test_defstruct() {
        // defstruct creates constructor + accessors via stdlib macro.
        // Load the full stdlib wrapped in (do ...) like the CLI does.
        let ctx = make_ctx();
        let stdlib = include_str!("../stdlib/zio/core.zio");
        let wrapped = format!("(do\n{stdlib}\n)");
        let s = test_read(&wrapped).unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Now define a struct and use it
        let s = test_read("(defstruct point [x y])").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(def p (point 10 20))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(point-x p)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(10), "point-x should be 10");

        let s = test_read("(point-y p)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(20), "point-y should be 20");

        // Verify it's a map
        let s = test_read("(map? p)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Boolean(true), "struct should be a map");
    }

    #[test]
    fn test_stdlib_nth_and_last_use_function_tail_calls() {
        let ctx = make_ctx();
        let stdlib = include_str!("../stdlib/zio/core.zio");
        let stdlib = test_read(&format!("(do\n{stdlib}\n)")).unwrap();
        eval_in_context(&stdlib, &ctx).unwrap();

        for (input, expected) in [
            ("(nth (list 10 20 30) 1)", Value::Integer(20)),
            ("(last (list 10 20 30))", Value::Integer(30)),
            ("(nth (list 10 20 30) 99)", Value::Nil),
        ] {
            let expression = test_read(input).unwrap();
            assert_eq!(eval_in_context(&expression, &ctx).unwrap(), expected);
        }
    }

    #[test]
    fn test_macro_example_generates_variadic_logger_functions() {
        let ctx = make_ctx();
        let stdlib = include_str!("../stdlib/zio/core.zio");
        let stdlib = test_read(&format!("(do\n{stdlib}\n)")).unwrap();
        eval_in_context(&stdlib, &ctx).unwrap();

        let example = include_str!("../../examples/macros.zio");
        let example = test_read(&format!("(do\n{example}\n)")).unwrap();

        assert_eq!(eval_in_context(&example, &ctx).unwrap(), Value::Nil);
    }

    #[test]
    fn test_syntax_rules_macro() {
        let ctx = make_ctx();

        // (unless test body) → (if test nil body)
        let s = test_read("(defmacro unless
          (syntax-rules ()
            ((_ test body) (if test nil body))))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(unless false 42)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(42));

        let s = test_read("(unless true 42)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Nil);

        // Macro with literal symbol matching
        // (define name value) → (def name value)
        let s = test_read("(defmacro define
          (syntax-rules ()
            ((_ name value) (def name value))))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(define x 42)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(42));

        let s = test_read("x").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(42));
    }

    #[test]
    fn test_zos_defclass_and_make_instance() {
        let ctx = make_ctx();

        // Define a class
        let s = test_read("(defclass point nil ((x :initarg :x :accessor point-x)
                                                (y :initarg :y :accessor point-y)))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Create an instance
        let s = test_read("(def p (make-instance point :x 10 :y 20))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(slot-value p :x)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(10), "slot-value :x should be 10");

        let s = test_read("(slot-value p :y)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(20), "slot-value :y should be 20");
    }

    #[test]
    fn test_zos_generic_function() {
        // Test defgeneric, defmethod, and dispatch
        let ctx = make_ctx();

        // Define classes
        let s = test_read("(defclass shape nil ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defclass circle shape ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Define GF and methods
        let s = test_read("(defgeneric draw (x))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defmethod draw ((x shape)) (str \"Drawing a shape\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defmethod draw ((x circle)) (str \"Drawing a circle\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Test dispatch
        let s = test_read("(draw (make-instance shape))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert!(result.to_string().contains("shape"), "shape method should be called, got: {result}");

        let s = test_read("(draw (make-instance circle))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert!(result.to_string().contains("circle"), "circle method should be called, got: {result}");
    }

    #[test]
    fn test_zos_method_combination() {
        // Test :around and call-next-method
        let ctx = make_ctx();

        let s = test_read("(defclass thing nil ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defgeneric process (x))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Primary method
        let s = test_read("(defmethod process ((x thing)) (str \"primary\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Around method with call-next-method
        let s = test_read("(defmethod process :around ((x thing)) (str \"before-\" (call-next-method) \"-after\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(process (make-instance thing))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        let result_str = result.to_string();
        assert!(result_str.contains("before"), "should contain 'before', got: {result_str}");
        assert!(result_str.contains("primary"), "should contain 'primary', got: {result_str}");
        assert!(result_str.contains("after"), "should contain 'after', got: {result_str}");
    }

    #[test]
    fn test_try_catch() {
        let ctx = make_ctx();

        // try with no error
        let s = test_read("(try 42)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Integer(42));

        // try/catch with caught error
        let s = test_read("(try (error \"oops\") (catch any \"caught\"))").unwrap();
        let result = eval_in_context(&s, &ctx);
        match result {
            Ok(v) => assert_eq!(v.to_string(), "\"caught\""),
            Err(e) => panic!("try/catch should catch error, got: {e}"),
        }
    }

    #[test]
    fn test_defpackage() {
        let ctx = make_ctx();

        let s = test_read("(defpackage :my-pkg (:use :core) (:export :my-fn))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Keyword("my-pkg".into()));

        // Check that package metadata was stored
        let s = test_read("(get *packages* :my-pkg)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Keyword("package".into()));
    }

    #[test]
    fn test_c3_linearization() {
        // Test C3 linearization via multi-inheritance classes
        let ctx = make_ctx();

        // Define a simple hierarchy with multiple inheritance
        //   A
        //  / \
        // B   C
        //  \ /
        //   D
        let s = test_read("(defclass a nil ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defclass b (a) ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defclass c (a) ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defclass d (b c) ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Verify inheritance works via GF dispatch
        let s = test_read("(defgeneric identify (x))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defmethod identify ((x a)) (str \"A\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defmethod identify ((x d)) (str \"D\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // D's method should be preferred over A's
        let s = test_read("(identify (make-instance d))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result.to_string(), "\"D\"", "D's method should be preferred");
    }

    #[test]
    fn test_reflection_class_of() {
        // Test basic reflection: class-of via type
        let ctx = make_ctx();
        let s = test_read("(type 42)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Keyword("integer".into()));

        let s = test_read("(type \"hello\")").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Keyword("string".into()));
    }

    #[test]
    fn test_reflection_class_metadata() {
        let ctx = make_ctx();
        let s = test_read("(defclass my-obj nil ((x :initarg :x)))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // class-name on an instance
        let s = test_read("(class-name (make-instance my-obj :x 42))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result, Value::Symbol("my-obj".into()));

        // class-precedence-list
        let s = test_read("(class-precedence-list (make-instance my-obj))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert!(matches!(result, Value::List(_)), "CPL should be a list");

        // class-slots
        let s = test_read("(class-slots (make-instance my-obj))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert!(matches!(result, Value::List(_)), "slots should be a list");
    }

    #[test]
    fn test_multi_dispatch() {
        let ctx = make_ctx();

        // Define a class hierarchy
        let s = test_read("(defclass vehicle nil ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defclass car (vehicle) ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defclass truck (vehicle) ())").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Multi-dispatch GF
        let s = test_read("(defgeneric collide (a b))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defmethod collide ((a vehicle) (b vehicle)) (str \"generic\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();
        let s = test_read("(defmethod collide ((a car) (b truck)) (str \"car-truck\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        // Test dispatch
        let s = test_read("(collide (make-instance car) (make-instance truck))").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(result.to_string(), "\"car-truck\"", "car-truck method should be preferred");

        // GF methods reflection
        let s = test_read("(generic-function-methods collide)").unwrap();
        let result = eval_in_context(&s, &ctx).unwrap();
        assert!(matches!(result, Value::List(_)), "methods should be a list");
    }
    #[test]
    fn test_io_host_capture() {
        use std::sync::Arc;
        use crate::io::BufferIoHost;

        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        let io = Arc::new(BufferIoHost::with_input(vec!["test-input-line\n".into()]));
        let ctx = EvalContext::with_io(env, io.clone());

        let expr = test_read("(println \"hello\" \"world\")").unwrap();
        eval_in_context(&expr, &ctx).unwrap();

        let expr = test_read("(print \"foo\")").unwrap();
        eval_in_context(&expr, &ctx).unwrap();

        assert_eq!(io.get_output(), "\"hello\" \"world\"\n\"foo\"");

        let expr = test_read("(read-line)").unwrap();
        let val = eval_in_context(&expr, &ctx).unwrap();
        assert_eq!(val, Value::String("test-input-line".into()));
    }
    #[test]
    fn test_syntax_rules_eval() {
        let ctx = make_ctx();
        let s = test_read("(defmacro my-unless (syntax-rules () ((my-unless test body) (if test nil body))))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(my-unless false 42)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::Integer(42));

        let s = test_read("(my-unless true 42)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::Nil);
    }
    #[test]
    fn test_buffer_eval() {
        let ctx = make_ctx();
        let s = test_read("(def buf (bytes \"hello\"))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(buffer-length buf)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::Integer(5));

        let s = test_read("(buffer-get buf 0)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::Integer(b'h' as i64));

        let s = test_read("(buffer-set! buf 0 74)").unwrap(); // 'J'
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(buffer-get buf 0)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::Integer(74));
    }

    #[test]
    fn test_future_promise_eval() {
        let ctx = make_ctx();
        let s = test_read("(def p (promise))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(deliver p 42)").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(deref p)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::Integer(42));
    }

    #[test]
    fn test_channel_csp_eval() {
        let ctx = make_ctx();
        let s = test_read("(def c (chan 10))").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(send! c \"message\")").unwrap();
        eval_in_context(&s, &ctx).unwrap();

        let s = test_read("(recv! c)").unwrap();
        let val = eval_in_context(&s, &ctx).unwrap();
        assert_eq!(val, Value::String("message".into()));
    }
}
