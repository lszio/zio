use std::any::Any;
use std::sync::Arc;

use crate::value::Value;

/// Flags for ObjectHeader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectFlags(u8);

impl ObjectFlags {
    pub const NONE: ObjectFlags = ObjectFlags(0);
    pub const MUTABLE: ObjectFlags = ObjectFlags(1);
}

/// Header shared by all heap-allocated ZOS objects.
#[derive(Debug, Clone)]
pub struct ObjectHeader {
    /// The class of this object (or metaclass for class objects).
    pub class: ClassRef,
    /// Object flags (mutable, etc.)
    pub flags: ObjectFlags,
    /// Optional unique identity (for entity tracking).
    pub identity: Option<u64>,
}

/// A reference to a ZOS Class (thread-safe, shared).
pub type ClassRef = Arc<Class>;

/// Trait for all heap-allocated ZOS objects.
pub trait ZosObject: std::fmt::Debug {
    fn header(&self) -> &ObjectHeader;
    fn as_any(&self) -> &dyn Any;
    /// Clone into a new Box. Required because `dyn ZosObject` is not Clone.
    fn clone_box(&self) -> Box<dyn ZosObject>;
}

/// Helper to clone a Box<dyn ZosObject>.
impl Clone for Box<dyn ZosObject> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

impl PartialEq for Box<dyn ZosObject> {
    fn eq(&self, other: &Self) -> bool {
        self.header().identity == other.header().identity
            && self.header().class.name == other.header().class.name
    }
}

impl std::hash::Hash for Box<dyn ZosObject> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.header().identity.hash(state);
        self.header().class.name.hash(state);
    }
}

// ── Forward declarations (defined in class.rs) ─────────────────

/// A ZOS Class definition.
#[derive(Debug, Clone)]
pub struct Class {
    pub name: String,
    pub superclass: Option<ClassRef>,
    pub slots: Vec<SlotDefinition>,
}

/// A slot (instance variable) definition.
#[derive(Debug, Clone)]
pub struct SlotDefinition {
    pub name: String,
    pub initargs: Vec<String>,
    pub initform: Option<Value>,
    pub accessor: Option<String>,
}

/// Convert a Value to a displayable string for debugging.
pub fn value_short(v: &Value) -> String {
    match v {
        Value::Nil => "nil".into(),
        Value::Integer(i) => format!("{i}"),
        Value::Symbol(s) => s.clone(),
        Value::Keyword(k) => format!(":{k}"),
        Value::String(s) => format!("\"{s}\""),
        Value::Object(o) => format!("#<{}>", o.header().class.name),
        _ => format!("{v}"),
    }
}
