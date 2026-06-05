use std::sync::Arc;

use im::Vector;

use crate::core::env::Env;
use crate::core::error::EvalError;
use crate::core::sexp::Sexp;
use crate::core::value::{Macro, Value};

/// Apply a macro: bind args (as Sexp data) to params, eval body, return expanded Sexp.
pub fn apply_macro(
    m: &Macro,
    args: &[Sexp],
    _env: &Arc<Env>,
) -> Result<Sexp, EvalError> {
    // Convert Sexp args to Value for binding (macros receive unevaluated data)
    let arg_values: Vector<Value> = args.iter().map(|a| Value::from(a.clone())).collect();

    let macro_env = if m.rest_param.is_some() {
        Env::bind_variadic(&m.env, &m.params, &m.rest_param, &arg_values)?
    } else {
        if m.params.len() != arg_values.len() {
            return Err(EvalError::WrongArgCount {
                expected: m.params.len(),
                got: arg_values.len(),
            });
        }
        Env::bind(&m.env, &m.params, &arg_values)?
    };

    // Evaluate the macro body in the macro env
    let result = crate::core::eval::eval(&m.body, &macro_env)?;

    // Convert the result Value back to Sexp
    value_to_sexp(&result)
}

/// Try to expand a macro call by looking up the name in the env.
/// Returns the expanded Sexp if it's a macro, None otherwise.
pub fn try_expand_by_name(
    name: &str,
    args: &[Sexp],
    env: &Arc<Env>,
) -> Result<Option<Sexp>, EvalError> {
    if let Some(Value::Macro(m)) = env.get(name) {
        let expanded = apply_macro(&m, args, env)?;
        Ok(Some(expanded))
    } else {
        Ok(None)
    }
}

/// Convert a Value back to Sexp (for macro expansion results).
/// Fails if the Value contains runtime-only types (Function, NativeFunction, Macro).
pub fn value_to_sexp(value: &Value) -> Result<Sexp, EvalError> {
    match value {
        Value::Nil => Ok(Sexp::Nil),
        Value::Boolean(b) => Ok(Sexp::Boolean(*b)),
        Value::Integer(i) => Ok(Sexp::Integer(*i)),
        Value::Float(f) => Ok(Sexp::Float(*f)),
        Value::String(s) => Ok(Sexp::String(s.clone())),
        Value::Symbol(s) => Ok(Sexp::Symbol(s.clone())),
        Value::Keyword(k) => Ok(Sexp::Keyword(k.clone())),
        Value::List(l) => {
            let mut result = Vector::new();
            for item in l {
                result.push_back(value_to_sexp(item)?);
            }
            Ok(Sexp::List(result))
        }
        Value::Vector(v) => {
            let mut result = Vector::new();
            for item in v {
                result.push_back(value_to_sexp(item)?);
            }
            Ok(Sexp::Vector(result))
        }
        Value::Map(m) => {
            let mut result = im::HashMap::new();
            for (k, v) in m {
                result.insert(value_to_sexp(k)?, value_to_sexp(v)?);
            }
            Ok(Sexp::Map(result))
        }
        Value::Function(_) | Value::NativeFunction(_) | Value::Macro(_) => {
            Err(EvalError::MacroError(format!(
                "cannot convert runtime value to syntax: {value}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::env::Env;
    use crate::core::eval;
    use im::vector;

    #[test]
    fn test_simple_macro() {
        let env = Arc::new(Env::new(None));
        crate::core::builtins::setup_env(&env);

        // (defmacro unless (test body) (list 'if test nil body))
        let macro_body = Sexp::List(vector![
            Sexp::Symbol("list".into()),
            Sexp::List(vector![Sexp::Symbol("quote".into()), Sexp::Symbol("if".into())]),
            Sexp::Symbol("test".into()),
            Sexp::Nil,
            Sexp::Symbol("body".into()),
        ]);

        let m = Arc::new(Macro {
            name: "unless".into(),
            params: vector!["test".to_string(), "body".to_string()],
            rest_param: None,
            body: macro_body,
            env: env.clone(),
        });

        env.set("unless".to_string(), Value::Macro(m));

        // Test expansion
        let expanded = try_expand_by_name(
            "unless",
            &[Sexp::Boolean(false), Sexp::Integer(42)],
            &env,
        )
        .unwrap()
        .expect("should expand");

        assert_eq!(
            expanded,
            Sexp::List(vector![
                Sexp::Symbol("if".into()),
                Sexp::Boolean(false),
                Sexp::Nil,
                Sexp::Integer(42),
            ])
        );

        // Test full eval via defmacro special form
        let expr = Sexp::List(vector![
            Sexp::Symbol("unless".into()),
            Sexp::Boolean(false),
            Sexp::Integer(42),
        ]);
        let result = eval::eval(&expr, &env).unwrap();
        assert_eq!(result, Value::Integer(42));
    }
}
