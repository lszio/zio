use std::sync::Arc;
use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{eval_last_body, TailResult};
use crate::value::Value;

// ── module ────────────────────────────────────────────────────────

/// (module name ...)
/// Declare a module: name must be a keyword or symbol.
/// All top-level def/defn/defmacro after this go into the module's namespace.
pub fn do_module(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::invalid_form("module requires a name"));
    }

    // Extract module name (keyword or symbol)
    let module_name = match &args[0] {
        Sexp::Keyword(s) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        Sexp::Symbol(s) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        other => {
            return Err(EvalError::invalid_form(
                format!("module name must be a keyword or symbol, got {}", other.kind()),
            ));
        }
    };

    if module_name.is_empty() {
        return Err(EvalError::invalid_form("module name cannot be empty"));
    }

    // Create a module-local env
    let module_env = Arc::new(Env::new(Some(env.clone())));

    // Evaluate body expressions in the module env
    let body = &args[1..];
    let result = if body.is_empty() {
        TailResult::Value(Value::Nil)
    } else {
        eval_last_body(body, &module_env, true, engine)?
    };

    // Register the module
    let module = crate::module::Module {
        name: module_name,
        env: module_env,
        exports: Vec::new(),
        source: None,
    };
    engine.register_module(module);

    Ok(result)
}

// ── require ───────────────────────────────────────────────────────

/// (require :module-name [:sym1 :sym2 ...])
/// Require and import symbols from a module.
/// Automatically loads from disk if not already loaded,
/// using the injectable loader registered by CLI layer.
pub fn do_require(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::invalid_form("require requires a module name"));
    }

    // Parse module name from first arg (keyword or symbol)
    let module_name = match &args[0] {
        Sexp::Keyword(s) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        Sexp::Symbol(s) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        other => {
            return Err(EvalError::invalid_form(
                format!("require requires a module name (keyword or symbol), got {}", other.kind()),
            ));
        }
    };

    // Optional: specific symbols to import
    let symbols: Option<Vec<String>> = if args.len() > 1 {
        let mut syms = Vec::new();
        for arg in &args[1..] {
            match arg {
                Sexp::Keyword(s) => syms.push(s.clone()),
                Sexp::Symbol(s) => syms.push(s.clone()),
                other => {
                    return Err(EvalError::invalid_form(
                        format!("require symbols must be keywords or symbols, got {}", other.kind()),
                    ));
                }
            }
        }
        Some(syms)
    } else {
        None
    };

    // Auto-load from disk if not already loaded (call the loader hook)
    if !engine.is_module_loaded(&module_name) {
        let name_str = module_name.join(".");
        let result = engine.call_loader(&module_name, &name_str, env);
        match result {
            None => {
                return Err(EvalError::custom(format!(
                    "no loader registered for modules and module not found: {name_str}"
                )));
            }
            Some(Ok(module)) => {
                engine.register_module(module);
            }
            Some(Err(e)) => return Err(e),
        }
    }

    // Find the now-loaded module and import symbols
    let module = engine.find_module(&module_name).ok_or_else(|| {
        EvalError::custom(format!("module not found after load: {}", module_name.join(".")))
    })?;

    let target_syms = symbols.unwrap_or(module.exports.clone());

    for sym_name in &target_syms {
        if let Some(val) = module.env.get(sym_name) {
            env.set(sym_name.clone(), val);
        }
    }

    Ok(TailResult::Value(Value::Nil))
}