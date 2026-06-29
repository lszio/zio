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
        superclasses: Vec::new(),
        slots: Vec::new(),
        cpl: vec!["TObject".into()],
    });
    classes.push(top.clone());

    // Immediate type classes
    for name in &[
        "Nil", "Boolean", "Integer", "Float", "String", "Symbol",
        "Keyword", "List", "Vector", "Map", "Function", "NativeFunction",
        "Macro", "Character",
    ] {
        let cpl = vec![name.to_string(), "TObject".into()];
        let c = Arc::new(Class {
            name: name.to_string(),
            superclasses: vec![top.clone()],
            slots: Vec::new(),
            cpl,
        });
        classes.push(c);
    }

    classes
}

/// Check if a class is a subclass of (or equal to) another, by name.
pub fn is_subclass_of(c: &ClassRef, target: &ClassRef) -> bool {
    if c.name == target.name {
        return true;
    }
    for sc in &c.superclasses {
        if is_subclass_of(sc, target) {
            return true;
        }
    }
    false
}

/// Compute C3 linearization for a class given its superclasses.
pub fn c3_linearize(class_name: &str, direct_supers: &[ClassRef]) -> Vec<String> {
    // Start with the class itself
    let mut result = vec![class_name.to_string()];

    // Compute CPL for each direct superclass
    let mut super_cpls: Vec<Vec<String>> = Vec::new();
    for sup in direct_supers {
        let cpl = if sup.cpl.is_empty() {
            vec![sup.name.clone()]
        } else {
            sup.cpl.clone()
        };
        super_cpls.push(cpl);
    }

    // Merge: C3 algorithm
    let mut merged = c3_merge(&mut super_cpls, direct_supers);
    result.append(&mut merged);
    result
}

/// C3 merge algorithm: take lists of CPLs + direct superclasses as the last list.
fn c3_merge(cpls: &mut [Vec<String>], direct_supers: &[ClassRef]) -> Vec<String> {
    let mut result = Vec::new();

    // Build the direct superclass name list (the last list in C3 merge)
    let direct_names: Vec<String> = direct_supers.iter().map(|c| c.name.clone()).collect();

    // Collect all lists: superclass CPLs + direct superclass list
    let mut all_lists: Vec<Vec<String>> = cpls.to_vec();
    all_lists.push(direct_names);

    // Remove any empty lists
    all_lists.retain(|l| !l.is_empty());

    while !all_lists.is_empty() {
        // Find a candidate: first element of any list that doesn't appear
        // in the tail of any other list
        let mut found: Option<String> = None;
        'candidate: for list in &all_lists {
            let candidate = &list[0];
            // Check that candidate is not in the tail of any list
            for other in &all_lists {
                if other.len() > 1 && other[1..].contains(candidate) {
                    continue 'candidate;
                }
            }
            found = Some(candidate.clone());
            break;
        }

        match found {
            Some(candidate) => {
                result.push(candidate.clone());
                // Remove candidate from all lists
                for list in &mut all_lists {
                    if list.first() == Some(&candidate) {
                        list.remove(0);
                    }
                }
                all_lists.retain(|l| !l.is_empty());
            }
            None => {
                // Inconsistent hierarchy — just return what we have
                break;
            }
        }
    }

    result
}


