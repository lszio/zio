use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::value::{Macro, Value};

/// Apply a macro: bind args (as Sexp data) to params, eval body, return expanded Sexp.
pub fn apply_macro(
    m: &Macro,
    args: &[Sexp],
    _env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Sexp, EvalError> {
    // Convert Sexp args to Value for binding (macros receive unevaluated data)
    let arg_values: Vector<Value> = args.iter().map(|a| Value::from(a.clone())).collect();

    let macro_env = if m.rest_param.is_some() {
        Env::bind_variadic(&m.env, &m.params, &m.rest_param, &arg_values)?
    } else {
        Env::bind(&m.env, &m.params, &arg_values)?
    };

    // Evaluate the macro body in the macro env
    let result = engine.eval_expr(&m.body, &macro_env, false)?;
    let val = result.into_value();

    // Convert the result Value back to Sexp
    value_to_sexp(&val)
}

/// Try to expand a macro call by looking up the name in the env.
/// Returns the expanded Sexp if it's a macro, None otherwise.
pub fn try_expand_by_name(
    name: &str,
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Option<Sexp>, EvalError> {
    if let Some(Value::Macro(m)) = env.get(name) {
        apply_macro(&m, args, env, engine).map(Some)
    } else {
        Ok(None)
    }
}

/// Apply a macro directly from the macro value (for computed-expression macros).
/// Same as apply_macro but exposed with a clearer name for the computed-macro path.
pub fn apply_macro_for_value(
    m: &Macro,
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Sexp, EvalError> {
    apply_macro(m, args, env, engine)
}

/// Convert a Value back to Sexp (for macro expansion results).
/// Fails if the Value contains runtime-only types (Function, NativeFunction, Macro).
pub fn value_to_sexp(value: &Value) -> Result<Sexp, EvalError> {
    match value {
        Value::Nil => Ok(Sexp::Nil),
        Value::Boolean(b) => Ok(Sexp::Boolean(*b)),
        Value::Integer(i) => Ok(Sexp::Integer(*i, None)),
        Value::Float(f) => Ok(Sexp::Float(*f, None)),
        Value::String(s) => Ok(Sexp::String(s.clone(), None)),
        Value::Symbol(s) => Ok(Sexp::Symbol(s.clone(), None)),
        Value::Keyword(k) => Ok(Sexp::Keyword(k.clone(), None)),
        Value::List(l) => {
            let mut new_list = Vector::new();
            for item in l { new_list.push_back(value_to_sexp(item)?); }
            Ok(Sexp::List(new_list, None))
        }
        Value::Vector(v) => {
            let mut new_vec = Vector::new();
            for item in v { new_vec.push_back(value_to_sexp(item)?); }
            Ok(Sexp::Vector(new_vec, None))
        }
        Value::Map(m) => {
            let mut new_map = im::HashMap::new();
            for (k, v) in m { new_map.insert(value_to_sexp(k)?, value_to_sexp(v)?); }
            Ok(Sexp::Map(new_map, None))
        }
        Value::Function(_) => Err(EvalError::macro_error("macro returned a function value")),
        Value::NativeFunction(_) => Err(EvalError::macro_error("macro returned a native function value")),
        Value::Macro(_) => Err(EvalError::macro_error("macro returned a macro value")),
        Value::Char(c) => Ok(Sexp::Char(*c, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins;
    use crate::context::EvalContext;
    use crate::env::Env;

    fn make_ctx() -> EvalContext {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        EvalContext::new(env)
    }

    #[test]
    fn test_value_to_sexp_roundtrip() {
        let v = Value::List(im::vector![
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(3),
        ]);
        let s = value_to_sexp(&v).unwrap();
        assert_eq!(s, Sexp::List(im::vector![
            Sexp::Integer(1, None),
            Sexp::Integer(2, None),
            Sexp::Integer(3, None),
        ], None));
    }

    #[test]
    fn test_value_to_sexp_function_error() {
        let f = Value::Function(Arc::new(crate::value::Function {
            params: im::vector![],
            rest_param: None,
            body: Sexp::Nil,
            env: Arc::new(Env::new(None)),
        }));
        assert!(value_to_sexp(&f).is_err());
    }

    #[test]
    fn test_apply_macro() {
        let m = Arc::new(Macro {
            name: "test-macro".into(),
            params: im::vector!["x".into()],
            rest_param: None,
            body: Sexp::Symbol("x".into(), None),
            env: Arc::new(Env::new(None)),
        });

        let ctx = make_ctx();
        let args = [Sexp::Integer(42, None)];
        let result = apply_macro(&m, &args, &ctx.env, &ctx).unwrap();
        assert_eq!(result, Sexp::Integer(42, None));
    }
}