use std::path::PathBuf;
use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::span::SourceId;
use crate::value::Value;

/// A loaded or declared module.
///
/// Every module has:
/// - A fully-qualified name like `["math", "linear"]`
/// - Its own environment (scope) where its definitions live
/// - A list of exported symbols
/// - A reference to the source file it came from
#[derive(Debug, Clone)]
pub struct Module {
    /// Hierarchical module name, e.g. ["zio", "math"]
    pub name: Vec<String>,
    /// The module's own environment (child of global env)
    pub env: Arc<Env>,
    /// Symbols explicitly exported
    pub exports: Vec<String>,
    /// Source file, if loaded from disk
    pub source: Option<SourceId>,
}

impl Module {
    /// Create a new empty module with the given name.
    pub fn new(name: Vec<String>, parent: &Arc<Env>) -> Self {
        Module {
            name,
            env: Arc::new(Env::new(Some(parent.clone()))),
            exports: Vec::new(),
            source: None,
        }
    }

    /// Full module name as a display string: "zio.math.linear"
    pub fn display_name(&self) -> String {
        self.name.join(".")
    }
}

/// Registry of all loaded modules, keyed by fully-qualified name.
#[derive(Debug, Default)]
pub struct ModuleRegistry {
    pub modules: Vec<Module>,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        ModuleRegistry { modules: Vec::new() }
    }

    /// Register a module.
    pub fn register(&mut self, module: Module) {
        self.modules.push(module);
    }

    /// Find a module by name.
    pub fn find(&self, name: &[String]) -> Option<&Module> {
        self.modules.iter().find(|m| m.name == name)
    }

    /// Remove a module (for cleanup).
    pub fn remove(&mut self, name: &[String]) {
        self.modules.retain(|m| m.name != name);
    }
}

/// A module reference stored as a runtime Value.
/// This wraps an index into a global ModuleRegistry.
#[derive(Debug, Clone)]
pub struct ModuleRef {
    pub index: usize,
}

/// Resolve a module path like "zio/math" to a file path.
/// Looks in:
///   1. Current directory (./{path}.zio)
///   2. ZIO_PATH env var directories
pub fn resolve_module_path(path: &str) -> Result<PathBuf, EvalError> {
    // Normalise: "zio.math" → "zio/math.zio"
    let file_stem = path.replace('.', "/");
    let filename = format!("{}.zio", file_stem);

    // Check current directory first
    let cwd_path = std::env::current_dir()
        .map_err(|e| EvalError::Custom(format!("cannot get cwd: {e}")))?;
    let candidate = cwd_path.join(&filename);
    if candidate.exists() {
        return Ok(candidate);
    }

    // Check ZIO_PATH
    if let Ok(paths) = std::env::var("ZIO_PATH") {
        for dir in paths.split(':') {
            let candidate = PathBuf::from(dir).join(&filename);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    Err(EvalError::Custom(format!(
        "module not found: {path} (searched ./{filename})"
    )))
}

/// Read a file and return its content.
pub fn read_source_file(path: &PathBuf) -> Result<String, EvalError> {
    std::fs::read_to_string(path)
        .map_err(|e| EvalError::Custom(format!("cannot read {}: {e}", path.display())))
}

/// Parse a module name from a keyword or symbol.
/// `:zio.math` or `zio.math` → ["zio", "math"]
pub fn parse_module_name(value: &Value) -> Result<Vec<String>, EvalError> {
    let s = match value {
        Value::Keyword(s) => s.clone(),
        Value::Symbol(s) => s.clone(),
        other => return Err(EvalError::type_error("module name (keyword or symbol)", other.value_type())),
    };
    Ok(s.split('.').map(|p| p.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_new() {
        let parent = Arc::new(Env::new(None));
        let m = Module::new(vec!["test".into()], &parent);
        assert_eq!(m.display_name(), "test");
        assert!(m.exports.is_empty());
    }

    #[test]
    fn test_module_registry() {
        let parent = Arc::new(Env::new(None));
        let mut reg = ModuleRegistry::new();
        let m = Module::new(vec!["foo".into(), "bar".into()], &parent);
        reg.register(m);
        assert!(reg.find(&["foo".into(), "bar".into()]).is_some());
        assert!(reg.find(&["foo".into()]).is_none());
    }

    #[test]
    fn test_module_name_parse() {
        let kw = Value::Keyword("zio.math".into());
        let name = parse_module_name(&kw).unwrap();
        assert_eq!(name, vec!["zio", "math"]);

        let sym = Value::Symbol("core.utils".into());
        let name = parse_module_name(&sym).unwrap();
        assert_eq!(name, vec!["core", "utils"]);
    }
}