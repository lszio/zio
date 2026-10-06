//! The one assembly path shared by the language CLI, the compiler front
//! end (T01), and every embedding that runs Zio code — including Grove's
//! generated-candidate execution.
//!
//! Everything here is fail-closed: the standard library either loads
//! completely or the bootstrap fails, module resolution is limited to
//! explicitly granted roots, and a nested `require` shares the caller's
//! SourceMap and IoHost so a module body never quietly gets its own
//! filesystem or its own source registry.

use std::path::PathBuf;
use std::sync::Arc;

use crate::builtins;
use crate::context::{EvalContext, EvalRuntime, ModuleLoader, ModuleRegistry};
use crate::env::Env;
use crate::error::EvalError;
use crate::io::IoHost;
use crate::module::Module;
use crate::sexp::Sexp;
use crate::span::SourceMap;
use crate::value::Value;

/// Directories a runtime may load modules from. Nothing outside these
/// roots is reachable, and the ambient CWD / `ZIO_PATH` are never
/// consulted — a generated profile must not inherit whatever the process
/// happens to be sitting in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleRoots {
    roots: Vec<PathBuf>,
}

impl ModuleRoots {
    /// Grant explicit roots. Each must be an existing directory: a typo in
    /// a root is a configuration error, not a silently empty search path.
    pub fn new(roots: Vec<PathBuf>) -> Result<Self, EvalError> {
        for root in &roots {
            if !root.is_dir() {
                return Err(EvalError::custom(format!(
                    "module root is not a directory: {}",
                    root.display()
                )));
            }
        }
        Ok(ModuleRoots { roots })
    }

    /// No roots at all: only inline `(module ...)` forms work.
    pub fn empty() -> Self {
        ModuleRoots { roots: Vec::new() }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Resolve `a.b.c` to a `.zio` file inside a granted root.
    ///
    /// A candidate outside the root is refused, including when a symlink
    /// points out: the path is canonicalized before the prefix check.
    pub fn resolve(&self, module_path: &str) -> Result<PathBuf, EvalError> {
        let rel: PathBuf = module_path.split('.').filter(|p| !p.is_empty()).collect();
        if rel.as_os_str().is_empty() {
            return Err(EvalError::custom(format!("invalid module name: {module_path}")));
        }
        for root in &self.roots {
            let candidate = root.join(&rel).with_extension("zio");
            if !candidate.exists() {
                continue;
            }
            let real_root = root
                .canonicalize()
                .map_err(|e| EvalError::custom(format!("root {}: {e}", root.display())))?;
            let real_file = candidate
                .canonicalize()
                .map_err(|e| EvalError::custom(format!("{}: {e}", candidate.display())))?;
            if !real_file.starts_with(&real_root) {
                return Err(EvalError::custom(format!(
                    "module {module_path} resolves to {} which is outside the granted roots",
                    real_file.display()
                )));
            }
            if real_file.extension().and_then(|e| e.to_str()) != Some("zio") {
                return Err(EvalError::custom(format!(
                    "module {module_path} does not name a .zio file"
                )));
            }
            return Ok(real_file);
        }
        Err(EvalError::custom(format!(
            "module not found: {module_path} (searched the granted roots only)"
        )))
    }
}

/// The shared assembly inputs a `require` loader needs: which roots it
/// may read, the SourceMap every source registers into, and the IoHost
/// all file access goes through. A nested module inherits all three.
#[derive(Clone)]
pub struct LoadProfile {
    roots: ModuleRoots,
    source_map: Arc<SourceMap>,
    io: Arc<dyn IoHost>,
}

impl LoadProfile {
    pub fn new(roots: ModuleRoots, source_map: Arc<SourceMap>, io: Arc<dyn IoHost>) -> Self {
        LoadProfile { roots, source_map, io }
    }

    pub fn source_map(&self) -> &Arc<SourceMap> {
        &self.source_map
    }

    /// Build the `require` loader for this profile.
    pub fn loader(&self) -> Box<ModuleLoader> {
        let profile = self.clone();
        Box::new(move |mod_name: &[String], _source: &str, parent_env: &Arc<Env>| {
            let name_str = mod_name.join(".");
            let path = profile.roots.resolve(&name_str)?;

            let source = std::fs::read_to_string(&path)
                .map_err(|e| EvalError::custom(format!("cannot read {}: {e}", path.display())))?;

            // Register into the shared SourceMap: an error inside the
            // module resolves to this file, not a private registry.
            let source_id = profile
                .source_map
                .register(path.to_string_lossy().into_owned(), source.clone());
            let forms = crate::reader::reader::read_program_with_source(&source, source_id)
                .map_err(|e| EvalError::custom(format!("parse error in {}: {e}", path.display())))?;

            let module_env = Arc::new(Env::new(Some(parent_env.clone())));
            let ctx = EvalContext::with_loader_and_io(
                module_env.clone(),
                profile.loader(),
                Arc::clone(&profile.io),
                Arc::clone(&profile.source_map),
            );

            // Collect (export ...) declarations from the module body.
            ctx.push_module_exports();
            for sexp in forms {
                crate::eval::eval_in_context(&sexp, &ctx).map_err(|e| {
                    EvalError::custom(format!("error loading module {name_str}: {e}"))
                })?;
            }
            let exports = ctx.take_module_exports();

            Ok(Module {
                name: mod_name.to_vec(),
                env: module_env,
                exports,
                source: Some(source_id),
            })
        })
    }
}

/// Load the embedded core standard library into `ctx`.
///
/// Errors are returned, never downgraded to warnings: a half-loaded
/// stdlib makes every later symbol resolution a guess.
pub fn bootstrap(ctx: &EvalContext) -> Result<(), EvalError> {
    bootstrap_source(ctx, "core.zio", crate::stdlib_source())
}

/// Parse and evaluate every top-level form of `source`, registering it
/// under `name` in the context's SourceMap.
pub fn bootstrap_source(ctx: &EvalContext, name: &str, source: &str) -> Result<(), EvalError> {
    let source_id = ctx.source_map().register(name.to_string(), source.to_string());
    let forms = crate::reader::reader::read_program_with_source(source, source_id)
        .map_err(|e| EvalError::custom(format!("parse error in {name}: {e}")))?;
    for sexp in forms {
        crate::eval::eval_in_context(&sexp, ctx)
            .map_err(|e| EvalError::custom(format!("error loading {name}: {e}")))?;
    }
    Ok(())
}

/// A language context: builtins, a granted-roots `require` loader sharing
/// this context's SourceMap and IoHost, and the core stdlib.
pub fn language_context(roots: ModuleRoots) -> Result<EvalContext, EvalError> {
    language_context_with_io(roots, Arc::new(crate::io::StdIoHost))
}

/// Same as [`language_context`] but with a caller-supplied IoHost, so a
/// sandboxed host keeps its own I/O while sharing the assembly.
pub fn language_context_with_io(
    roots: ModuleRoots,
    io: Arc<dyn IoHost>,
) -> Result<EvalContext, EvalError> {
    let env = Arc::new(Env::new(None));
    builtins::setup_env(&env);
    let ctx = EvalContext::with_io(env, Arc::clone(&io));
    let profile = LoadProfile::new(roots, Arc::clone(ctx.source_map()), io);
    *ctx.loader.borrow_mut() = Some(profile.loader());
    bootstrap(&ctx)?;
    Ok(ctx)
}

/// Evaluate every top-level form of `source` in `ctx`, returning the last
/// value. The single entry point for "run this Zio text".
pub fn eval_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Value, EvalError> {
    let source_id = ctx.source_map().register(name.to_string(), source.to_string());
    let forms = crate::reader::reader::read_program_with_source(source, source_id)
        .map_err(|e| EvalError::custom(format!("parse error in {name}: {e}")))?;
    let mut last = Value::Nil;
    for sexp in forms {
        last = crate::eval::eval_in_context(&sexp, ctx)?;
    }
    Ok(last)
}

/// Parse `source` into forms without evaluating, registering the source.
/// This is the compiler front end's reader boundary (T01).
pub fn parse_source(ctx: &EvalContext, name: &str, source: &str) -> Result<Vec<Sexp>, EvalError> {
    let source_id = ctx.source_map().register(name.to_string(), source.to_string());
    crate::reader::reader::read_program_with_source(source, source_id)
        .map_err(|e| EvalError::custom(format!("parse error in {name}: {e}")))
}
