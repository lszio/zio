use std::sync::Arc;

use crate::error::EvalError;
use crate::value::Value;
use crate::zos::object::{Class, ClassRef, SlotDefinition};

/// Global registry of built-in classes.
#[derive(Debug)]
pub struct ClassRegistry {
    /// Built-in classes keyed by name.
    classes: Vec<ClassRef>,
}

impl ClassRegistry {
    pub fn new() -> Self {
        ClassRegistry { classes: Vec::new() }
    }

    /// Register a class. Returns error if name already taken.
    pub fn register(&mut self, class: ClassRef) -> Result<(), EvalError> {
        if self.find_by_name(&class.name).is_some() {
            return Err(EvalError::custom(format!("class already defined: {}", class.name)));
        }
        self.classes.push(class);
        Ok(())
    }

    /// Find a class by name.
    pub fn find_by_name(&self, name: &str) -> Option<ClassRef> {
        self.classes.iter().find(|c| c.name == name).cloned()
    }

    /// Get the class-of a Value (the most specific class).
    pub fn class_of(&self, val: &Value, top: &ClassRef) -> ClassRef {
        match val {
            Value::Nil => self.find_by_name("Nil").unwrap_or_else(|| top.clone()),
            Value::Boolean(_) => self.find_by_name("Boolean").unwrap_or_else(|| top.clone()),
            Value::Integer(_) => self.find_by_name("Integer").unwrap_or_else(|| top.clone()),
            Value::Float(_) => self.find_by_name("Float").unwrap_or_else(|| top.clone()),
            Value::String(_) => self.find_by_name("String").unwrap_or_else(|| top.clone()),
            Value::Symbol(_) => self.find_by_name("Symbol").unwrap_or_else(|| top.clone()),
            Value::Keyword(_) => self.find_by_name("Keyword").unwrap_or_else(|| top.clone()),
            Value::List(_) => self.find_by_name("List").unwrap_or_else(|| top.clone()),
            Value::Vector(_) => self.find_by_name("Vector").unwrap_or_else(|| top.clone()),
            Value::Map(_) => self.find_by_name("Map").unwrap_or_else(|| top.clone()),
            Value::Function(_) => self.find_by_name("Function").unwrap_or_else(|| top.clone()),
            Value::NativeFunction(_) => self.find_by_name("NativeFunction").unwrap_or_else(|| top.clone()),
            Value::Macro(_) => self.find_by_name("Macro").unwrap_or_else(|| top.clone()),
            Value::Char(_) => self.find_by_name("Character").unwrap_or_else(|| top.clone()),
            Value::Object(o) => o.header().class.clone(),
        }
    }
}

/// Create the built-in class hierarchy.
pub fn make_builtin_classes() -> Vec<ClassRef> {
    let mut classes = Vec::new();

    // Top class (TObject)
    let top = Arc::new(Class {
        name: "TObject".into(),
        superclass: None,
        slots: Vec::new(),
    });
    classes.push(top.clone());

    // Immediate type classes
    for name in &[
        "Nil", "Boolean", "Integer", "Float", "String", "Symbol",
        "Keyword", "List", "Vector", "Map", "Function", "NativeFunction",
        "Macro", "Character",
    ] {
        let c = Arc::new(Class {
            name: name.to_string(),
            superclass: Some(top.clone()),
            slots: Vec::new(),
        });
        classes.push(c);
    }

    classes
}

/// Check if a class is a subclass of (or equal to) another.
pub fn is_subclass_of(c: &ClassRef, target: &ClassRef) -> bool {
    if Arc::ptr_eq(c, target) {
        return true;
    }
    if let Some(ref superclass) = c.superclass {
        is_subclass_of(superclass, target)
    } else {
        false
    }
}
