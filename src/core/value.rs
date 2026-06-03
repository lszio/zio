use im::{Vector, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use crate::core::env::Env;

pub type NativeFn = fn(Vector<Value>) -> Result<Value, String>;

#[derive(Debug, Clone)]
pub struct Function {
    pub params: Vector<String>,
    pub body: Arc<Value>,
    pub env: Arc<Env>,
}

#[derive(Debug, Clone)]
pub enum Value {
    Nil,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Symbol(String),
    Keyword(String),
    List(Vector<Value>),
    Vector(Vector<Value>),
    Map(HashMap<Value, Value>),
    Function(Arc<Function>),
    NativeFunction(NativeFn),
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Boolean(a), Value::Boolean(b)) => a == b,
            (Value::Integer(a), Value::Integer(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Symbol(a), Value::Symbol(b)) => a == b,
            (Value::Keyword(a), Value::Keyword(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Vector(a), Value::Vector(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (Value::Function(a), Value::Function(b)) => Arc::ptr_eq(a, b),
            (Value::NativeFunction(a), Value::NativeFunction(b)) => *a as usize == *b as usize,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Nil => {}
            Value::Boolean(b) => b.hash(state),
            Value::Integer(i) => i.hash(state),
            Value::Float(f) => f.to_bits().hash(state),
            Value::String(s) => s.hash(state),
            Value::Symbol(s) => s.hash(state),
            Value::Keyword(k) => k.hash(state),
            Value::List(l) => l.hash(state),
            Value::Vector(v) => v.hash(state),
            Value::Map(m) => m.hash(state),
            Value::Function(f) => Arc::as_ptr(f).hash(state),
            Value::NativeFunction(f) => (*f as usize).hash(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::{vector, hashmap};

    #[test]
    fn test_value_equality() {
        assert_eq!(Value::Nil, Value::Nil);
        assert_eq!(Value::Boolean(true), Value::Boolean(true));
        assert_ne!(Value::Boolean(true), Value::Boolean(false));
        assert_eq!(Value::Integer(42), Value::Integer(42));
        assert_ne!(Value::Integer(42), Value::Integer(43));
        assert_eq!(Value::Float(3.14), Value::Float(3.14));
        assert_eq!(Value::String("hello".into()), Value::String("hello".into()));
        assert_eq!(Value::Symbol("foo".into()), Value::Symbol("foo".into()));
        assert_eq!(Value::Keyword("bar".into()), Value::Keyword("bar".into()));
    }

    #[test]
    fn test_float_equality() {
        assert_eq!(Value::Float(f64::NAN), Value::Float(f64::NAN));
        assert_ne!(Value::Float(0.0), Value::Float(-0.0));
    }

    #[test]
    fn test_collection_equality() {
        let v1 = Value::Vector(vector![Value::Integer(1), Value::Integer(2)]);
        let v2 = Value::Vector(vector![Value::Integer(1), Value::Integer(2)]);
        assert_eq!(v1, v2);

        let l1 = Value::List(vector![Value::Integer(1), Value::Integer(2)]);
        let l2 = Value::List(vector![Value::Integer(1), Value::Integer(2)]);
        assert_eq!(l1, l2);

        let m1 = Value::Map(hashmap!{Value::Keyword("a".into()) => Value::Integer(1)});
        let m2 = Value::Map(hashmap!{Value::Keyword("a".into()) => Value::Integer(1)});
        assert_eq!(m1, m2);
    }
}
