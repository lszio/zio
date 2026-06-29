use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use im::{HashMap, Vector};

/// User-defined function.
#[derive(Debug, Clone)]
pub struct Function {
    pub params: Vector<String>,
    pub rest_param: Option<String>,
    pub body: Sexp,
    pub env: Arc<Env>,
}

/// Native (Rust-implemented) function.
/// The closure receives the args and a reference to the eval engine,
/// which is needed by higher-order native functions like map/filter/reduce.
#[derive(Clone)]
pub struct NativeFn {
    id: usize,
    name: &'static str,
    func: Arc<dyn Fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError> + Send + Sync>,
}

impl std::fmt::Debug for NativeFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeFn")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

static NATIVE_ID_COUNTER: AtomicUsize = AtomicUsize::new(1);

impl NativeFn {
    pub fn new(
        name: &'static str,
        f: impl Fn(Vector<Value>, &dyn EvalEngine) -> Result<Value, EvalError> + Send + Sync + 'static,
    ) -> Self {
        let id = NATIVE_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        NativeFn {
            id,
            name,
            func: Arc::new(f),
        }
    }

    pub fn call(&self, args: Vector<Value>, engine: &dyn EvalEngine) -> Result<Value, EvalError> {
        (self.func)(args, engine)
    }

    pub fn name(&self) -> &str {
        self.name
    }

    pub fn id(&self) -> usize {
        self.id
    }
}

impl PartialEq for NativeFn {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for NativeFn {}

impl Hash for NativeFn {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Macro defined via `defmacro`.
#[derive(Debug, Clone)]
pub struct Macro {
    pub name: String,
    pub params: Vector<String>,
    pub rest_param: Option<String>,
    pub body: Sexp,
    pub env: Arc<Env>,
}



/// A ZOS instance — the most common heap object.
/// Stores slot values keyed by slot name.
#[derive(Debug, Clone)]
pub struct ZosInstance {
    pub header: crate::zos::object::ObjectHeader,
    pub slots: std::collections::HashMap<String, Value>,
}

impl ZosInstance {
    pub fn new(class: Arc<crate::zos::object::Class>) -> Self {
        let header = crate::zos::object::ObjectHeader {
            class,
            flags: crate::zos::object::ObjectFlags::NONE,
            identity: None,
        };
        ZosInstance {
            header,
            slots: std::collections::HashMap::new(),
        }
    }
}

impl crate::zos::object::ZosObject for ZosInstance {
    fn header(&self) -> &crate::zos::object::ObjectHeader {
        &self.header
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn clone_box(&self) -> Box<dyn crate::zos::object::ZosObject> {
        Box::new(self.clone())
    }
}

impl std::fmt::Display for ZosInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#<{}>", self.header.class.name)
    }
}
/// Runtime value — the output of evaluation.
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
    Macro(Arc<Macro>),
    Char(char),
    /// ZOS heap object — entry point for the runtime object system.
    Object(Box<dyn crate::zos::object::ZosObject>),
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
            (Value::Macro(a), Value::Macro(b)) => Arc::ptr_eq(a, b),
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::NativeFunction(a), Value::NativeFunction(b)) => a.eq(b),
            (Value::Object(a), Value::Object(b)) => a == b,
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
            Value::Macro(m) => Arc::as_ptr(m).hash(state),
            Value::NativeFunction(nf) => nf.hash(state),
            Value::Char(c) => c.hash(state),
            Value::Object(o) => o.hash(state),
        }
    }
}

impl Value {
    /// Returns a human-readable type name for error messages.
    pub fn value_type(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Boolean(_) => "boolean",
            Value::Integer(_) | Value::Float(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Keyword(_) => "keyword",
            Value::List(_) => "list",
            Value::Vector(_) => "vector",
            Value::Map(_) => "map",
            Value::Function(_) => "function",
            Value::NativeFunction(_) => "native-function",
            Value::Macro(_) => "macro",
            Value::Char(_) => "character",
            Value::Object(_) => "object",
        }
    }
}
/// Infallible conversion from Sexp (syntax tree) to Value (runtime).
impl From<Sexp> for Value {
    fn from(s: Sexp) -> Self {
        match s {
            Sexp::Nil => Value::Nil,
            Sexp::Boolean(b) => Value::Boolean(b),
            Sexp::Integer(i, _) => Value::Integer(i),
            Sexp::Float(f, _) => Value::Float(f),
            Sexp::String(s, _) => Value::String(s),
            Sexp::Symbol(s, _) => Value::Symbol(s),
            Sexp::Keyword(k, _) => Value::Keyword(k),
            Sexp::List(l, _) => Value::List(l.into_iter().map(Value::from).collect()),
            Sexp::Vector(v, _) => Value::Vector(v.into_iter().map(Value::from).collect()),
            Sexp::Map(m, _) => Value::Map(
                m.into_iter()
                    .map(|(k, v)| (Value::from(k), Value::from(v)))
                    .collect(),
            ),
            Sexp::Char(c, _) => Value::Char(c),
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Boolean(b) => write!(f, "{b}"),
            Value::Integer(i) => write!(f, "{i}"),
            Value::Float(fl) => write!(f, "{fl}"),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Symbol(s) => write!(f, "{s}"),
            Value::Keyword(k) => write!(f, ":{k}"),
            Value::List(l) => {
                write!(f, "(")?;
                for (i, val) in l.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{val}")?;
                }
                write!(f, ")")
            }
            Value::Vector(v) => {
                write!(f, "[")?;
                for (i, val) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{val}")?;
                }
                write!(f, "]")
            }
            Value::Map(m) => {
                write!(f, "{{")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k} {v}")?;
                }
                write!(f, "}}")
            }
            Value::Function(fun) => {
                let mut parts: Vec<&str> = fun.params.iter().map(|p| p.as_str()).collect();
                if let Some(ref rest) = fun.rest_param {
                    parts.push("&");
                    parts.push(rest);
                }
                write!(f, "#<function ({})>", parts.join(" "))
            }
            Value::NativeFunction(nf) => write!(f, "#<native {}>", nf.name()),
            Value::Macro(m) => {
                let mut parts: Vec<&str> = m.params.iter().map(|p| p.as_str()).collect();
                if let Some(ref rest) = m.rest_param {
                    parts.push("&");
                    parts.push(rest);
                }
                write!(f, "#<macro {} ({})>", m.name, parts.join(" "))
            }
            Value::Char(c) => write!(f, "#\\{c}"),
            Value::Object(o) => write!(f, "#<{}>", o.header().class.name),
        }
    }
}
impl Value {
    /// Pretty-print this value with indentation for readability.
    pub fn pretty_print(&self, f: &mut impl std::fmt::Write, indent: usize) -> std::fmt::Result {
        match self {
            Value::List(l) => {
                // Check if this is a "short" list (all atoms, ≤5 elements)
                let is_short = l.len() <= 5 && l.iter().all(|v| matches!(v,
                    Value::Nil | Value::Boolean(_) | Value::Integer(_)
                    | Value::Float(_) | Value::String(_) | Value::Symbol(_)
                    | Value::Keyword(_) | Value::Char(_)));
                if is_short {
                    write!(f, "(")?;
                    for (i, val) in l.iter().enumerate() {
                        if i > 0 { write!(f, " ")?; }
                        val.pretty_print(f, indent + 1)?;
                    }
                    write!(f, ")")
                } else {
                    writeln!(f, "(")?;
                    for val in l.iter() {
                        write!(f, "{:indent$}", "", indent = indent + 2)?;
                        val.pretty_print(f, indent + 2)?;
                        writeln!(f)?;
                    }
                    write!(f, "{:indent$})", "", indent = indent)
                }
            }
            Value::Vector(v) => {
                let is_short = v.len() <= 5 && v.iter().all(|val| matches!(val,
                    Value::Nil | Value::Boolean(_) | Value::Integer(_)
                    | Value::Float(_) | Value::String(_) | Value::Symbol(_)
                    | Value::Keyword(_) | Value::Char(_)));
                if is_short {
                    write!(f, "[")?;
                    for (i, val) in v.iter().enumerate() {
                        if i > 0 { write!(f, " ")?; }
                        val.pretty_print(f, indent + 1)?;
                    }
                    write!(f, "]")
                } else {
                    writeln!(f, "[")?;
                    for val in v.iter() {
                        write!(f, "{:indent$}", "", indent = indent + 2)?;
                        val.pretty_print(f, indent + 2)?;
                        writeln!(f)?;
                    }
                    write!(f, "{:indent$}]", "", indent = indent)
                }
            }
            Value::Map(m) => {
                if m.is_empty() {
                    write!(f, "{{}}")
                } else if m.len() <= 3 {
                    write!(f, "{{")?;
                    for (i, (k, v)) in m.iter().enumerate() {
                        if i > 0 { write!(f, " ")?; }
                        k.pretty_print(f, indent + 1)?;
                        write!(f, " ")?;
                        v.pretty_print(f, indent + 1)?;
                    }
                    write!(f, "}}")
                } else {
                    writeln!(f, "{{")?;
                    for (k, v) in m.iter() {
                        write!(f, "{:indent$}", "", indent = indent + 2)?;
                        k.pretty_print(f, indent + 2)?;
                        write!(f, " ")?;
                        v.pretty_print(f, indent + 2)?;
                        writeln!(f)?;
                    }
                    write!(f, "{:indent$}}}", "", indent = indent)
                }
            }
            // Atoms: use Display
            _ => write!(f, "{self}"),
        }
    }
}

/// Check if a Value is truthy (everything except nil and false).
pub fn is_truthy(v: &Value) -> bool {
    !matches!(v, Value::Nil | Value::Boolean(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use im::{hashmap, vector};

    #[test]
    fn test_value_equality() {
        assert_eq!(Value::Nil, Value::Nil);
        assert_eq!(Value::Boolean(true), Value::Boolean(true));
        assert_ne!(Value::Boolean(true), Value::Boolean(false));
        assert_eq!(Value::Integer(42), Value::Integer(42));
        assert_ne!(Value::Integer(42), Value::Integer(43));
        assert_eq!(Value::Float(3.5), Value::Float(3.5));
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

        let m1 = Value::Map(hashmap! {Value::Keyword("a".into()) => Value::Integer(1)});
        let m2 = Value::Map(hashmap! {Value::Keyword("a".into()) => Value::Integer(1)});
        assert_eq!(m1, m2);
    }

    #[test]
    fn test_value_type() {
        assert_eq!(Value::Nil.value_type(), "nil");
        assert_eq!(Value::Integer(42).value_type(), "number");
        assert_eq!(Value::String("hi".into()).value_type(), "string");
    }

    #[test]
    fn test_truthy() {
        assert!(!is_truthy(&Value::Nil));
        assert!(!is_truthy(&Value::Boolean(false)));
        assert!(is_truthy(&Value::Boolean(true)));
        assert!(is_truthy(&Value::Integer(0)));
        assert!(is_truthy(&Value::String("".into())));
    }

    #[test]
    fn test_from_sexp() {
        let s = Sexp::List(vector![
            Sexp::Symbol("+".into(), None),
            Sexp::Integer(1, None),
            Sexp::Integer(2, None),
        ], None);
        let v: Value = s.into();
        assert_eq!(
            v,
            Value::List(vector![
                Value::Symbol("+".into()),
                Value::Integer(1),
                Value::Integer(2)
            ])
        );
    }

    #[test]
    fn test_nativefn_eq_hash() {
        // Use a helper that provides a dummy engine
        fn dummy_engine() -> &'static dyn EvalEngine {
            // Used only in tests that don't need actual eval
            struct DummyEngine;
            impl crate::context::EvalRuntime for DummyEngine {
                fn eval_expr(&self, _: &Sexp, _: &Arc<Env>, _: bool) -> Result<crate::special::TailResult, EvalError> {
                    unimplemented!()
                }
                fn env(&self) -> &Arc<Env> { panic!("DummyEngine has no env") }
            }
            impl crate::context::ModuleRegistry for DummyEngine {
                fn register_module(&self, _: crate::module::Module) {}
                fn find_module(&self, _: &[String]) -> Option<crate::module::Module> { None }
                fn is_module_loaded(&self, _: &[String]) -> bool { false }
                fn call_loader(&self, _: &[String], _: &str, _: &Arc<Env>) -> Option<Result<crate::module::Module, EvalError>> { None }
                fn begin_loading(&self, _: &std::path::Path) -> Result<(), EvalError> { Ok(()) }
                fn end_loading(&self, _: &std::path::Path) {}
            }
            impl crate::context::EvalEngine for DummyEngine {}
            static ENGINE: DummyEngine = DummyEngine;
            &ENGINE
        }

        let a = NativeFn::new("test", |_, _| Ok(Value::Nil));
        let b = NativeFn::new("test", |_, _| Ok(Value::Nil));
        assert_ne!(a, b);
        assert_eq!(a, a);
    }
}