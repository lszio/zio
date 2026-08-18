use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::EvalError;
use crate::value::Value;
use im::Vector;

#[derive(Debug)]
pub struct Env {
    data: RefCell<HashMap<String, Value>>,
    outer: Option<Arc<Env>>,
}

impl Env {
    pub fn new(outer: Option<Arc<Env>>) -> Self {
        Self {
            data: RefCell::new(HashMap::new()),
            outer,
        }
    }
    pub fn set(&self, key: String, value: Value) {
        self.data.borrow_mut().insert(key, value);
    }

    /// Walk the scope chain and update the binding for `key` to `value`.
    /// Returns true if a binding was found and updated, false otherwise.
    pub fn set_global(&self, key: &str, value: Value) -> bool {
        if self.data.borrow().contains_key(key) {
            self.data.borrow_mut().insert(key.to_string(), value);
            return true;
        }
        if let Some(outer) = &self.outer {
            return outer.set_global(key, value);
        }
        false
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let data = self.data.borrow();
        if let Some(value) = data.get(key) {
            return Some(value.clone());
        }
        drop(data);
        if let Some(outer) = &self.outer {
            return outer.get(key);
        }
        None
    }

    /// Bind parameters to arguments in a new child environment.
    pub fn bind(
        outer: &Arc<Env>,
        params: &Vector<String>,
        args: &Vector<Value>,
    ) -> Result<Arc<Env>, EvalError> {
        if params.len() != args.len() {
            return Err(EvalError::wrong_arg_count(params.len(), args.len(),
            ));
        }
        let env = Arc::new(Env::new(Some(outer.clone())));
        for (p, a) in params.iter().zip(args.iter()) {
            env.set(p.clone(), a.clone());
        }
        Ok(env)
    }

    /// Bind parameters with a rest parameter (variadic).
    /// Extra arguments are collected into a list and bound to `rest_param`.
    pub fn bind_variadic(
        outer: &Arc<Env>,
        params: &Vector<String>,
        rest_param: &Option<String>,
        args: &Vector<Value>,
    ) -> Result<Arc<Env>, EvalError> {
        let min_args = params.len();
        if args.len() < min_args {
            return Err(EvalError::wrong_arg_count_min(min_args, args.len(),
            ));
        }
        let env = Arc::new(Env::new(Some(outer.clone())));
        // Bind named params
        for (i, p) in params.iter().enumerate() {
            env.set(p.clone(), args[i].clone());
        }
        // Bind rest args as a list
        if let Some(rest) = rest_param {
            let rest_list: Vector<Value> = args.iter().skip(min_args).cloned().collect();
            env.set(rest.clone(), Value::List(rest_list));
        }
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::vector;

    #[test]
    fn test_env_set_get() {
        let env = Env::new(None);
        env.set("x".to_string(), Value::Integer(10));
        assert_eq!(env.get("x"), Some(Value::Integer(10)));
    }

    #[test]
    fn test_env_lexical_scoping() {
        let outer = Arc::new(Env::new(None));
        outer.set("x".to_string(), Value::Integer(10));
        outer.set("y".to_string(), Value::Integer(20));

        let inner = Env::new(Some(outer.clone()));
        inner.set("x".to_string(), Value::Integer(30));

        assert_eq!(inner.get("x"), Some(Value::Integer(30)));
        assert_eq!(inner.get("y"), Some(Value::Integer(20)));
        assert_eq!(outer.get("x"), Some(Value::Integer(10)));
    }

    #[test]
    fn test_bind() {
        let outer = Arc::new(Env::new(None));
        let params = vector!["a".to_string(), "b".to_string()];
        let args = vector![Value::Integer(1), Value::Integer(2)];
        let env = Env::bind(&outer, &params, &args).unwrap();
        assert_eq!(env.get("a"), Some(Value::Integer(1)));
        assert_eq!(env.get("b"), Some(Value::Integer(2)));
    }

    #[test]
    fn test_bind_arity_error() {
        let outer = Arc::new(Env::new(None));
        let params = vector!["a".to_string(), "b".to_string()];
        let args = vector![Value::Integer(1)];
        let result = Env::bind(&outer, &params, &args);
        assert!(result.is_err());
    }

    #[test]
    fn test_bind_variadic() {
        let outer = Arc::new(Env::new(None));
        let params = vector!["a".to_string()];
        let args = vector![Value::Integer(1), Value::Integer(2), Value::Integer(3)];
        let env = Env::bind_variadic(&outer, &params, &Some("rest".to_string()), &args).unwrap();
        assert_eq!(env.get("a"), Some(Value::Integer(1)));
        assert_eq!(
            env.get("rest"),
            Some(Value::List(vector![Value::Integer(2), Value::Integer(3)]))
        );
    }

    #[test]
    fn test_bind_variadic_no_rest() {
        let outer = Arc::new(Env::new(None));
        let params = vector!["a".to_string(), "b".to_string()];
        let args = vector![Value::Integer(1), Value::Integer(2)];
        let env = Env::bind_variadic(&outer, &params, &None, &args).unwrap();
        assert_eq!(env.get("a"), Some(Value::Integer(1)));
        assert_eq!(env.get("b"), Some(Value::Integer(2)));
    }

    #[test]
    fn test_bind_variadic_too_few() {
        let outer = Arc::new(Env::new(None));
        let params = vector!["a".to_string(), "b".to_string()];
        let args = vector![Value::Integer(1)];
        let result = Env::bind_variadic(&outer, &params, &Some("rest".to_string()), &args);
        assert!(result.is_err());
    }
}
