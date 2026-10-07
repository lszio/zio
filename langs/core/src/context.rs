use std::path::Path;
use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::module::{Module, ModuleTable};
use crate::sexp::Sexp;
use crate::special::TailResult;

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

    /// The directory a `load`ed relative path resolves against.
    ///
    /// `None` means the host named none, and resolution falls back to
    /// the process working directory. An installed application sets
    /// this so a `(load "contracts.zio")` finds the sibling it shipped
    /// with, whichever directory the launcher was run from.
    fn source_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Get the host I/O implementation for this evaluation runtime (ADR-011).
    fn io(&self) -> &dyn crate::io::IoHost {
        &DEFAULT_STD_IO
    }

    /// This runtime's execution observation port, so a special form can
    /// report the branch it took without knowing what kind of runtime it
    /// is running in. Inert when no observer is attached.
    fn observation(&self) -> &crate::observer::Observation {
        crate::observer::inert()
    }

    /// Charge `units` of execution against this runtime's ceiling.
    ///
    /// Every path that does work charges here: the tree-walking
    /// evaluator, macro expansion, a native callback re-entering the
    /// evaluator, and the compiled instruction loop. A ceiling that only
    /// one of those paths honours is not a ceiling — it is a number in
    /// a report. A runtime with no ceiling accepts the charge and
    /// returns `Ok`.
    fn spend(&self, units: u64) -> Result<(), EvalError> {
        let _ = units;
        Ok(())
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
pub type ModuleLoader = dyn Fn(&[String], &str, &Arc<Env>) -> Result<Module, EvalError>;

/// Evaluation context — holds all mutable state needed during eval.
pub struct EvalContext {
    pub env: Arc<Env>,
    pub modules: std::rc::Rc<std::cell::RefCell<ModuleTable>>,
    pub loader: std::cell::RefCell<Option<Box<ModuleLoader>>>,
    pub io: Arc<dyn crate::io::IoHost>,
    /// Registry of parsed sources; spans from eval errors resolve here.
    pub source_map: Arc<crate::span::SourceMap>,
    /// Directory a `load`ed path is resolved against, when the host
    /// named one.
    ///
    /// An installed application is launched from wherever the user
    /// happens to be, so resolving `(load "contracts.zio")` against the
    /// process CWD finds nothing the moment the launcher is run from
    /// another directory. Resolving against the source's own directory
    /// is what makes an application relocatable.
    pub source_dir: std::cell::RefCell<Option<std::path::PathBuf>>,
    /// Stack of export accumulators for modules being defined. The
    /// `(export a b)` form appends to the top; `module` forms and module
    /// loaders push before evaluating a body and take the result after.
    pub module_exports: std::cell::RefCell<Vec<Vec<String>>>,
    /// Execution observation. Absent unless a host attached one, in which
    /// case every branch, call, error, and macro expansion is reported as
    /// it happens. See [`crate::observer`].
    pub observation: crate::observer::Observation,
}

impl EvalContext {
    fn build(
        env: Arc<Env>,
        io: Arc<dyn crate::io::IoHost>,
        source_map: Arc<crate::span::SourceMap>,
    ) -> Self {
        EvalContext {
            env,
            modules: std::rc::Rc::new(std::cell::RefCell::new(ModuleTable::new())),
            loader: std::cell::RefCell::new(None),
            io,
            source_map,
            source_dir: std::cell::RefCell::new(None),
            module_exports: std::cell::RefCell::new(Vec::new()),
            observation: crate::observer::Observation::default(),
        }
    }

    pub fn new(env: Arc<Env>) -> Self {
        Self::build(
            env,
            Arc::new(crate::io::StdIoHost),
            Arc::new(crate::span::SourceMap::new()),
        )
    }

    /// Attach an execution observer. See [`crate::observer`]: this is the
    /// only way execution evidence enters the system, and the evaluator
    /// is the only thing that can produce it.
    pub fn attach_observer(&self, observer: Arc<dyn crate::observer::Observer>) {
        self.observation.attach(observer);
    }

    /// Payloads this context's observer has built. Zero while no observer
    /// is attached.
    pub fn payloads_built(&self) -> u64 {
        self.observation.payloads_built()
    }

    /// Bound evaluation to `ceiling` nodes, enforced by the evaluator.
    /// `0` is unbounded. A host running code it did not write sets this
    /// so a non-terminating program is stopped instead of the machine.
    pub fn set_step_ceiling(&self, ceiling: u64) {
        self.observation.set_fuel_ceiling(ceiling);
    }

    /// Nodes evaluated against the current ceiling.
    pub fn steps_spent(&self) -> u64 {
        self.observation.fuel_spent()
    }

    /// Charge `units` of execution. Every evaluator path funnels
    /// through here so a ceiling cannot be side-stepped by taking a
    /// different route through the same program.
    pub fn spend(&self, units: u64) -> Result<(), EvalError> {
        if self.observation.spend_fuel_by(units) {
            Ok(())
        } else {
            Err(EvalError::custom("execution stopped: step ceiling reached"))
        }
    }

    pub fn with_io(env: Arc<Env>, io: Arc<dyn crate::io::IoHost>) -> Self {
        Self::build(env, io, Arc::new(crate::span::SourceMap::new()))
    }

    /// Full assembly: loader, I/O, and the SourceMap a nested module
    /// registers its own source into, so every span — script, REPL input,
    /// or required module — resolves against one registry. Module loading
    /// only ever happens with an explicit SourceMap and IoHost, so this is
    /// the single way to build a loading context.
    pub fn with_loader_and_io(
        env: Arc<Env>,
        loader: Box<ModuleLoader>,
        io: Arc<dyn crate::io::IoHost>,
        source_map: Arc<crate::span::SourceMap>,
    ) -> Self {
        let ctx = Self::build(env, io, source_map);
        *ctx.loader.borrow_mut() = Some(loader);
        ctx
    }

    pub fn with_shared_modules(
        env: Arc<Env>,
        loader: Box<ModuleLoader>,
        io: Arc<dyn crate::io::IoHost>,
        source_map: Arc<crate::span::SourceMap>,
        modules: std::rc::Rc<std::cell::RefCell<ModuleTable>>,
    ) -> Self {
        let mut ctx = Self::with_loader_and_io(env, loader, io, source_map);
        ctx.modules = modules;
        ctx
    }
}
