
use crate::zos::class::ClassRegistry;
use crate::zos::gf::GenericFunction;
use crate::zos::mop::ClassReflectionInfo;
use crate::zos::object::ClassRef;
use crate::value::Value;

/// Reflective inspector for ZOS runtime entities.
pub struct ReflectionEngine;

impl ReflectionEngine {
    /// Retrieve full reflection metadata for a class.
    pub fn inspect_class(class_ref: &ClassRef) -> ClassReflectionInfo {
        ClassReflectionInfo::from_class(class_ref)
    }

    /// Retrieve the class of a given runtime Value.
    pub fn class_of(val: &Value, registry: &ClassRegistry, top_class: &ClassRef) -> ClassRef {
        registry.class_of(val, top_class)
    }

    /// Extract method signatures from a generic function.
    pub fn find_methods(gf: &GenericFunction) -> Vec<(Vec<String>, String)> {
        gf.methods
            .iter()
            .map(|m| {
                let specs = m
                    .specializers
                    .iter()
                    .map(|s| match s {
                        crate::zos::gf::Specializer::T => "T".into(),
                        crate::zos::gf::Specializer::Exact(name) => name.clone(),
                    })
                    .collect();
                let qual = match m.qualifier {
                    crate::zos::gf::MethodQualifier::Primary => "primary",
                    crate::zos::gf::MethodQualifier::Before => "before",
                    crate::zos::gf::MethodQualifier::After => "after",
                    crate::zos::gf::MethodQualifier::Around => "around",
                };
                (specs, qual.into())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use crate::zos::object::Class;

    #[test]
    fn test_reflection_engine() {
        let top = Arc::new(Class {
            name: "TObject".into(),
            superclasses: vec![],
            slots: vec![],
            cpl: vec!["TObject".into()],
        });
        let point_cls = Arc::new(Class {
            name: "Point".into(),
            superclasses: vec![top.clone()],
            slots: vec![],
            cpl: vec!["Point".into(), "TObject".into()],
        });

        let info = ReflectionEngine::inspect_class(&point_cls);
        assert_eq!(info.name, "Point");
        assert_eq!(info.direct_superclasses, vec!["TObject"]);
        assert_eq!(info.cpl, vec!["Point", "TObject"]);
    }
}
