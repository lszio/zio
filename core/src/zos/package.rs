use std::collections::{HashMap, HashSet};

/// A ZOS Package — manages symbol namespaces.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    /// Internal symbol table (name → interned symbol value)
    pub symbols: HashMap<String, crate::value::Value>,
    /// Explicitly exported symbols
    pub exports: HashSet<String>,
    /// Packages that are :used (inherited)
    pub uses: Vec<String>,
    /// Imported symbols from other packages
    pub imports: HashMap<String, String>, // local-name → source-package
}

impl Package {
    pub fn new(name: String) -> Self {
        Package {
            name,
            symbols: HashMap::new(),
            exports: HashSet::new(),
            uses: Vec::new(),
            imports: HashMap::new(),
        }
    }

    /// Intern a symbol in this package.
    pub fn intern(&mut self, name: String, value: crate::value::Value) {
        self.symbols.insert(name, value);
    }

    /// Find a symbol in this package or its uses chain.
    pub fn find(&self, name: &str, packages: &HashMap<String, Package>) -> Option<crate::value::Value> {
        // Check own symbols
        if let Some(val) = self.symbols.get(name) {
            return Some(val.clone());
        }
        // Check used packages
        for use_pkg_name in &self.uses {
            if let Some(pkg) = packages.get(use_pkg_name) {
                if let Some(val) = pkg.symbols.get(name) {
                    return Some(val.clone());
                }
            }
        }
        None
    }
}
