use std::path::Path;
use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::module::{Module, ModuleRegistry};

/// The evaluation engine — abstract interface for the recursive eval loop.
///
/// Special forms receive a `&dyn EvalEngine` instead of a closure,
/// eliminating lifetime annotations and dynamic dispatch on function pointers.
/// Module operations are included so that module_forms and eval don't need
/// direct access to the evaluation context.
pub trait EvalEngine {
    /// Evaluate an S-expression in a given environment, with tail-position tracking.
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool) -> Result<TailResult, EvalError>;

    /// Get the current (root/top-level) environment for this execution context.
    fn env(&self) -> &Arc<Env>;

    // ── Module registry operations ─────────────────────────────────
    fn register_module(&self, m: Module);
    fn find_module(&self, name: &[String]) -> Option<Module>;
    fn is_module_loaded(&self, name: &[String]) -> bool;
    fn call_loader(
        &self,
        name: &[String],
        source: &str,
        parent_env: &Arc<Env>,
    ) -> Option<Result<Module, EvalError>>;
    fn begin_loading(&self, path: &Path) -> Result<(), EvalError>;
    fn end_loading(&self, path: &Path);
}

/// Type for the injectable module loader function.
/// Given: (module_name_parts, source_content, parent_env) -> Result<Module, EvalError>
pub type ModuleLoader = dyn Fn(&[String], &str, &Arc<Env>) -> Result<Module, EvalError> + Send + Sync;

/// Evaluation context -- holds all mutable state needed during eval.
///
/// Previously this state was scattered across 4 thread_local! globals
/// (MODULES, LOADING_STACK, REQUIRE_LOADER, GLOBAL_ENV).
/// Now it's an explicit parameter passed through the eval chain,
/// making the evaluator testable, embeddable, and free of ambient state.
pub struct EvalContext {
    /// The top-level environment (root of the lexical chain)
    pub env: Arc<Env>,
    /// Registry of all loaded modules
    pub modules: std::cell::RefCell<ModuleRegistry>,
    /// Injectable module loader (set by CLI layer)
    pub loader: std::cell::RefCell<Option<Box<ModuleLoader>>>,
}

impl EvalContext {
    pub fn new(env: Arc<Env>) -> Self {
        EvalContext {
            env,
            modules: std::cell::RefCell::new(ModuleRegistry::new()),
            loader: std::cell::RefCell::new(None),
        }
    }

    pub fn with_loader(env: Arc<Env>, loader: Box<ModuleLoader>) -> Self {
        EvalContext {
            env,
            modules: std::cell::RefCell::new(ModuleRegistry::new()),
            loader: std::cell::RefCell::new(Some(loader)),
        }
    }
}