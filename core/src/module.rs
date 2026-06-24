use std::path::{Path, PathBuf};
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
    /// The module's own environment (child of parent env)
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
    /// Stack of file paths being loaded (circular require detection).
    pub loading_stack: Vec<PathBuf>,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        ModuleRegistry {
            modules: Vec::new(),
            loading_stack: Vec::new(),
        }
    }

    /// Register a module.
    pub fn register(&mut self, module: Module) {
        self.modules.push(module);
    }

    /// Find a module by name.
    pub fn find(&self, name: &[String]) -> Option<&Module> {
        self.modules.iter().find(|m| m.name == name)
    }

    #[allow(dead_code)]
    pub fn find_mut(&mut self, name: &[String]) -> Option<&mut Module> {
        self.modules.iter_mut().find(|m| m.name == name)
    }

    /// Remove a module (for cleanup).
    pub fn remove(&mut self, name: &[String]) {
        self.modules.retain(|m| m.name != name);
    }

    /// Check if a module is already loaded.
    pub fn is_loaded(&self, name: &[String]) -> bool {
        self.modules.iter().any(|m| m.name == name)
    }

    /// Push a path onto the loading stack. Returns error if already loading (circular).
    pub fn begin_loading(&mut self, path: &Path) -> Result<(), EvalError> {
        if self.loading_stack.iter().any(|p| p == path) {
            let chain: Vec<String> = self.loading_stack.iter().map(|p| p.display().to_string()).collect();
            return Err(EvalError::custom(format!(
                "circular require detected: {} → {}",
                chain.join(" → "),
                path.display()
            )));
        }
        self.loading_stack.push(path.to_path_buf());
        Ok(())
    }

    /// Pop a path from the loading stack after loading completes.
    pub fn end_loading(&mut self, path: &Path) {
        self.loading_stack.retain(|p| p != path);
    }
}

/// Resolve a module path like "zio/math" to a file path.
/// Looks in:
///   1. Current directory (./{path}.zio)
///   2. ZIO_PATH env var directories
pub fn resolve_module_path(path: &str) -> Result<PathBuf, EvalError> {
    let file_stem = path.replace('.', "/");
    let filename = format!("{}.zio", file_stem);

    // Check current directory first
    let cwd_path = std::env::current_dir()
        .map_err(|e| EvalError::custom(format!("cannot get cwd: {e}")))?;
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

    Err(EvalError::custom(format!(
        "module not found: {path} (searched ./{filename})"
    )))
}

/// Read a file and return its content.
pub fn read_source_file(path: &PathBuf) -> Result<String, EvalError> {
    std::fs::read_to_string(path)
        .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path.display())))
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
    fn test_module_registry() {
        let mut reg = ModuleRegistry::new();
        let env = Arc::new(Env::new(None));
        let m = Module::new(vec!["test".into()], &env);
        reg.register(m);
        assert!(reg.is_loaded(&["test".into()]));
        assert!(!reg.is_loaded(&["nope".into()]));
    }

    #[test]
    fn test_resolve_module_valid_name() {
        let name = parse_module_name(&Value::Keyword("zio.math".into())).unwrap();
        assert_eq!(name, vec!["zio", "math"]);
    }

    #[test]
    fn test_begin_loading_ok() {
        let mut reg = ModuleRegistry::new();
        assert!(reg.begin_loading(Path::new("foo.zio")).is_ok());
        assert!(reg.loading_stack.len() == 1);
    }

    #[test]
    fn test_circular_require_detection() {
        let mut reg = ModuleRegistry::new();
        reg.begin_loading(Path::new("a.zio")).unwrap();
        reg.begin_loading(Path::new("b.zio")).unwrap();
        let result = reg.begin_loading(Path::new("a.zio"));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("circular"));
    }

    #[test]
    fn test_end_loading() {
        let mut reg = ModuleRegistry::new();
        reg.begin_loading(Path::new("a.zio")).unwrap();
        reg.end_loading(Path::new("a.zio"));
        assert!(reg.loading_stack.is_empty());
    }
}