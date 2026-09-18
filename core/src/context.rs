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

    /// The source map this context registers parsed sources into, so that
    /// spans from every loaded file resolve against one registry.
    fn source_map(&self) -> &Arc<crate::span::SourceMap>;

    /// Get the host I/O implementation for this evaluation runtime (ADR-011).
    fn io(&self) -> &dyn crate::io::IoHost {
        &DEFAULT_STD_IO
    }
}

static DEFAULT_STD_IO: crate::io::StdIoHost = crate::io::StdIoHost;

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

    /// Export accumulator for the module being defined (see the `export`
    /// form). Loaders and `module` push before evaluating a body and take
    /// the collected names afterwards.
    fn push_module_exports(&self);
    fn add_module_export(&self, name: String);
    fn take_module_exports(&self) -> Vec<String>;
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
    pub io: Arc<dyn crate::io::IoHost>,
    /// Registry of parsed sources; spans from eval errors resolve here.
    pub source_map: Arc<crate::span::SourceMap>,
    /// Stack of export accumulators for modules being defined. The
    /// `(export a b)` form appends to the top; `module` forms and module
    /// loaders push before evaluating a body and take the result after.
    pub module_exports: std::cell::RefCell<Vec<Vec<String>>>,
}

impl EvalContext {
    fn build(
        env: Arc<Env>,
        io: Arc<dyn crate::io::IoHost>,
    ) -> Self {
        EvalContext {
            env,
            modules: std::cell::RefCell::new(ModuleTable::new()),
            loader: std::cell::RefCell::new(None),
            io,
            source_map: Arc::new(crate::span::SourceMap::new()),
            module_exports: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub fn new(env: Arc<Env>) -> Self {
        Self::build(env, Arc::new(crate::io::StdIoHost))
    }

    pub fn with_io(env: Arc<Env>, io: Arc<dyn crate::io::IoHost>) -> Self {
        Self::build(env, io)
    }

    pub fn with_loader(env: Arc<Env>, loader: Box<ModuleLoader>) -> Self {
        let ctx = Self::build(env, Arc::new(crate::io::StdIoHost));
        *ctx.loader.borrow_mut() = Some(loader);
        ctx
    }

    pub fn with_loader_and_io(
        env: Arc<Env>,
        loader: Box<ModuleLoader>,
        io: Arc<dyn crate::io::IoHost>,
    ) -> Self {
        let ctx = Self::build(env, io);
        *ctx.loader.borrow_mut() = Some(loader);
        ctx
    }
}