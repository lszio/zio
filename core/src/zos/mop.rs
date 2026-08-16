use std::collections::HashMap;
use std::sync::Arc;

use crate::error::EvalError;
use crate::zos::object::ClassRef;

/// MetaClass definition — describes a class of classes (MOP).
#[derive(Debug, Clone)]
pub struct MetaClass {
    pub name: String,
    pub super_metaclasses: Vec<String>,
}

/// MetaClass registry — tracks meta-level classes (e.g. StandardClass, BuiltinClass).
#[derive(Debug, Default)]
pub struct MetaClassRegistry {
    metaclasses: HashMap<String, Arc<MetaClass>>,
}

impl MetaClassRegistry {
    pub fn new() -> Self {
        let mut reg = MetaClassRegistry {
            metaclasses: HashMap::new(),
        };

        // Default MetaClasses
        reg.register(MetaClass {
            name: "StandardClass".into(),
            super_metaclasses: vec!["Class".into()],
        }).unwrap();

        reg.register(MetaClass {
            name: "BuiltinClass".into(),
            super_metaclasses: vec!["Class".into()],
        }).unwrap();

        reg.register(MetaClass {
            name: "StructureClass".into(),
            super_metaclasses: vec!["Class".into()],
        }).unwrap();

        reg
    }

    pub fn register(&mut self, meta: MetaClass) -> Result<(), EvalError> {
        if self.metaclasses.contains_key(&meta.name) {
            return Err(EvalError::custom(format!("metaclass already registered: {}", meta.name)));
        }
        self.metaclasses.insert(meta.name.clone(), Arc::new(meta));
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<Arc<MetaClass>> {
        self.metaclasses.get(name).cloned()
    }
}

/// Meta-object reflection helpers for ZOS classes.
#[derive(Debug, Clone)]
pub struct ClassReflectionInfo {
    pub name: String,
    pub direct_superclasses: Vec<String>,
    pub cpl: Vec<String>,
    pub slot_names: Vec<String>,
}

impl ClassReflectionInfo {
    pub fn from_class(class_ref: &ClassRef) -> Self {
        ClassReflectionInfo {
            name: class_ref.name.clone(),
            direct_superclasses: class_ref.superclasses.iter().map(|c| c.name.clone()).collect(),
            cpl: class_ref.cpl.clone(),
            slot_names: class_ref.slots.iter().map(|s| s.name.clone()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metaclass_registry() {
        let reg = MetaClassRegistry::new();
        assert!(reg.find("StandardClass").is_some());
        assert!(reg.find("BuiltinClass").is_some());
        assert!(reg.find("StructureClass").is_some());
    }
}
