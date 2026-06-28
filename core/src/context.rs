use std::path::Path;
use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::TailResult;
use crate::module::{Module, ModuleTable};

/// Minimal evaluation capability — eval and environment access.
/// Embedding scenarios that don't need modules or ZOS only need this trait.
pub trait EvalRuntime {
    /// Evaluate an S-expression in a given environment, with tail-position tracking.
    fn eval_expr(&self, expr: &Sexp, env: &Arc<Env>, tail: bool) -> Result<TailResult, EvalError>;

    /// Get the current (root/top-level) environment for this execution context.
    fn env(&self) -> &Arc<Env>;
}

/// Module registry — loading, caching, and dependency tracking for modules.
/// Independent from evaluation; embedders that don't load modules skip this.
pub trait ModuleRegistry {
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

/// The evaluation engine — abstract interface for the recursive eval loop.
///
/// Combines `EvalRuntime` and `ModuleRegistry`. Special forms receive a
/// `&dyn EvalEngine`, which can be coerced to either sub-trait as needed.
///
/// ADR-005 split: embedders can implement `EvalRuntime` alone when modules
/// are not needed; CLI and ZOS scenarios implement all three traits.
pub trait EvalEngine: EvalRuntime + ModuleRegistry {}

/// Type for the injectable module loader function.
pub type ModuleLoader = dyn Fn(&[String], &str, &Arc<Env>) -> Result<Module, EvalError> + Send + Sync;

/// Evaluation context — holds all mutable state needed during eval.
pub struct EvalContext {
    pub env: Arc<Env>,
    pub modules: std::cell::RefCell<ModuleTable>,
    pub loader: std::cell::RefCell<Option<Box<ModuleLoader>>>,
}

impl EvalContext {
    pub fn new(env: Arc<Env>) -> Self {
        EvalContext {
            env,
            modules: std::cell::RefCell::new(ModuleTable::new()),
            loader: std::cell::RefCell::new(None),
        }
    }

    pub fn with_loader(env: Arc<Env>, loader: Box<ModuleLoader>) -> Self {
        EvalContext {
            env,
            modules: std::cell::RefCell::new(ModuleTable::new()),
            loader: std::cell::RefCell::new(Some(loader)),
        }
    }
}