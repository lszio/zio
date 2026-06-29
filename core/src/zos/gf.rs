use std::sync::Arc;

use im::Vector;

use crate::value::Value;
use crate::zos::object::{ClassRef, ObjectHeader, ObjectFlags, ZosObject};
use crate::zos::class::is_subclass_of;

/// The qualifier of a method in a generic function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodQualifier {
    Primary,
    Before,
    After,
    Around,
}

/// A method belonging to a generic function.
#[derive(Debug, Clone)]
pub struct Method {
    pub specializers: Vec<Specializer>,
    pub qualifier: MethodQualifier,
    /// The function body as a Value::Function (captured at defmethod time).
    pub body: Arc<crate::value::Function>,
}

/// Type specializer — specifies which argument types match.
#[derive(Debug, Clone)]
pub enum Specializer {
    /// Accept any type
    T,
    /// Accept only this exact class (by name)
    Exact(String),
}

/// A generic function — dispatches to methods based on argument types.
#[derive(Debug, Clone)]
pub struct GenericFunction {
    pub name: String,
    pub methods: Vec<Arc<Method>>,
    pub lambda_list: Vec<String>,
    // Simplified dispatch cache: (specializer_0, ..., specializer_3) → methods list
    pub dispatch_cache: std::collections::HashMap<(u64, u64, u64, u64), Vec<Arc<Method>>>,
}

impl GenericFunction {
    pub fn new(name: String, lambda_list: Vec<String>) -> Self {
        GenericFunction {
            name,
            methods: Vec::new(),
            lambda_list,
            dispatch_cache: std::collections::HashMap::new(),
        }
    }

    /// Add a method to this GF and invalidate the cache.
    pub fn add_method(&mut self, method: Arc<Method>) {
        self.methods.push(method);
        self.dispatch_cache.clear();
    }

    /// Find applicable methods for the given argument types (class refs).
    /// Returns methods sorted by specificity (most specific first).
    pub fn find_applicable_methods(&self, arg_classes: &[ClassRef]) -> Vec<Arc<Method>> {
        let mut applicable: Vec<Arc<Method>> = Vec::new();

        for method in &self.methods {
            if self.method_applies(method, arg_classes) {
                applicable.push(Arc::clone(method));
            }
        }

        // Sort by specificity: more specific specializers first
        applicable.sort_by(|a, b| {
            let a_specificity = self.specificity_score(&a.specializers, arg_classes);
            let b_specificity = self.specificity_score(&b.specializers, arg_classes);
            b_specificity.cmp(&a_specificity)
        });

        applicable
    }

    /// Check if a method applies to the given argument classes.
    fn method_applies(&self, method: &Arc<Method>, arg_classes: &[ClassRef]) -> bool {
        if method.specializers.len() > arg_classes.len() {
            return false;
        }
        for (specializer, arg_class) in method.specializers.iter().zip(arg_classes.iter()) {
            match specializer {
                Specializer::T => continue,
                Specializer::Exact(name) => {
                    if !is_subclass_of(arg_class, &Arc::new(
                        crate::zos::object::Class {
                            name: name.clone(),
                            superclasses: Vec::new(),
                            slots: Vec::new(),
                            cpl: vec![name.clone()],
                        }
                    )) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Compute specificity score for sorting methods.
    /// Higher score = more specific (better match).
    fn specificity_score(&self, specializers: &[Specializer], arg_classes: &[ClassRef]) -> usize {
        let mut score = 0usize;
        for (i, (specializer, arg_class)) in specializers.iter().zip(arg_classes.iter()).enumerate() {
            match specializer {
                Specializer::T => score += 1 << (i * 4),       // least specific
                Specializer::Exact(name) => {
                    if arg_class.name == *name {
                        score += 10 << (i * 4);                // exact match = most specific
                    } else {
                        score += 5 << (i * 4);                 // subclass = medium
                    }
                }
            }
        }
        score
    }

    /// Build a cache key from argument classes.
    fn cache_key(&self, arg_classes: &[ClassRef]) -> (u64, u64, u64, u64) {
        let id = |i: usize| -> u64 {
            arg_classes.get(i).map(|c| simple_hash(&c.name)).unwrap_or(0)
        };
        (id(0), id(1), id(2), id(3))
    }

    /// Dispatch: find and sort applicable methods, return by qualifier.
    pub fn dispatch(&mut self, arg_classes: &[ClassRef]) -> DispatchResult {
        let key = self.cache_key(arg_classes);

        // Check cache first (separate lookup from insert to avoid borrow conflict)
        if !self.dispatch_cache.contains_key(&key) {
            let methods = self.find_applicable_methods(arg_classes);
            self.dispatch_cache.insert(key, methods);
        }

        let methods = self.dispatch_cache.get(&key).unwrap();

        let mut around = Vec::new();
        let mut before = Vec::new();
        let mut primary = Vec::new();
        let mut after = Vec::new();

        for m in methods.iter() {
            match m.qualifier {
                MethodQualifier::Around => around.push(Arc::clone(m)),
                MethodQualifier::Before => before.push(Arc::clone(m)),
                MethodQualifier::Primary => primary.push(Arc::clone(m)),
                MethodQualifier::After => after.push(Arc::clone(m)),
            }
        }

        DispatchResult { around, before, primary, after }
    }
}

/// The result of dispatching a generic function.
#[derive(Debug, Clone)]
pub struct DispatchResult {
    pub around: Vec<Arc<Method>>,
    pub before: Vec<Arc<Method>>,
    pub primary: Vec<Arc<Method>>,
    pub after: Vec<Arc<Method>>,
}

/// Simple hash function for cache keys.
fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u64);
    }
    h
}
