use crate::core::value::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use im::Vector;

#[derive(Debug)]
pub struct Env {
    data: Mutex<HashMap<String, Value>>,
    outer: Option<Arc<Env>>,
}

impl Env {
    pub fn new(outer: Option<Arc<Env>>) -> Self {
        Self {
            data: Mutex::new(HashMap::new()),
            outer,
        }
    }

    pub fn set(&self, key: String, value: Value) {
        let mut data = self.data.lock().unwrap();
        data.insert(key, value);
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let data = self.data.lock().unwrap();
        if let Some(value) = data.get(key) {
            return Some(value.clone());
        }
        if let Some(outer) = &self.outer {
            return outer.get(key);
        }
        None
    }

    pub fn bind(outer: Arc<Env>, params: Vector<String>, args: Vector<Value>) -> Result<Arc<Env>, String> {
        if params.len() != args.len() {
            return Err(format!("Expected {} arguments, got {}", params.len(), args.len()));
        }
        let env = Arc::new(Env::new(Some(outer)));
        for (p, a) in params.into_iter().zip(args.into_iter()) {
            env.set(p, a);
        }
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
