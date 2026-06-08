use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::module::{Module, ModuleRegistry, call_require_loader};
use crate::sexp::Sexp;
use crate::special::{eval_last_body, EvalFn, TailResult};
use crate::value::Value;

// Global module registry (thread-local)
use std::cell::RefCell;
thread_local! {
    static MODULES: RefCell<ModuleRegistry> = RefCell::new(ModuleRegistry { modules: Vec::new() });
}

/// Register a module in the global registry.
pub fn register_module(m: Module) {
    MODULES.with(|cell| {
        cell.borrow_mut().register(m);
    });
}

/// Find a module by name in the global registry.
pub fn find_module(name: &[String]) -> Option<Module> {
    MODULES.with(|cell| {
        cell.borrow().find(name).cloned()
    })
}

/// Check if a module is already loaded in the global registry.
pub fn is_module_loaded(name: &[String]) -> bool {
    MODULES.with(|cell| {
        cell.borrow().is_loaded(name)
    })
}

/// (module name ...)
/// Declare a module: name must be a keyword or symbol.
/// All top-level def/defn/defmacro after this go into the module's namespace.
pub fn do_module<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("module requires a name".into()));
    }

    let name_str = match &args[0] {
        Sexp::Keyword(s) | Sexp::Symbol(s) => s.clone(),
        other => return Err(EvalError::TypeError {
            expected: "keyword or symbol",
            got: other.kind().to_string(),
        }),
    };

    let module_name: Vec<String> = name_str.split('.').map(|s| s.to_string()).collect();
    let body = &args[1..];

    let module_env = Arc::new(Env::new(Some(env.clone())));

    let result = if body.is_empty() {
        TailResult::Value(Value::Nil)
    } else {
        eval_last_body(body, &module_env, false, eval_fn)?
    };

    let module = Module {
        name: module_name.clone(),
        env: module_env.clone(),
        exports: Vec::new(),
        source: None,
    };

    register_module(module);

    Ok(result)
}

/// (require :module-name [:sym1 :sym2 ...])
/// Require and import symbols from a module.
/// Automatically loads from disk if not already loaded,
/// using the injectable loader registered by CLI layer.
pub fn do_require<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    _eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("require needs a module name".into()));
    }

    let module_key = match &args[0] {
        Sexp::Keyword(s) => s.clone(),
        Sexp::Symbol(s) => s.clone(),
        other => return Err(EvalError::TypeError {
            expected: "keyword or symbol",
            got: other.kind().to_string(),
        }),
    };

    let module_name: Vec<String> = module_key.split('.').map(|s| s.to_string()).collect();
    let name_str = module_name.join(".");

    // Try to auto-load from disk if not yet loaded
    if !is_module_loaded(&module_name) {
        auto_load_module(&module_name, &name_str, env)?;
    }

    // Find the now-loaded module
    let module = find_module(&module_name)
        .ok_or_else(|| EvalError::Custom(format!("module not loaded: {name_str}")))?;

    // If specific symbols requested, import those
    let specific_syms: Vec<&Sexp> = args[1..].iter().collect();
    if specific_syms.is_empty() {
        // Import all exports
        for sym in &module.exports {
            if let Some(val) = module.env.get(sym) {
                env.set(sym.clone(), val);
            }
        }
    } else {
        for sym_sexp in &specific_syms {
            let sym_name = match sym_sexp {
                Sexp::Keyword(s) | Sexp::Symbol(s) => s.clone(),
                other => return Err(EvalError::TypeError {
                    expected: "symbol or keyword",
                    got: other.kind().to_string(),
                }),
            };
            if let Some(val) = module.env.get(&sym_name) {
                env.set(sym_name, val);
            } else {
                return Err(EvalError::Custom(format!(
                    "symbol {sym_name} not exported by module {name_str}"
                )));
            }
        }
    }

    Ok(TailResult::Value(Value::Nil))
}

/// Try to load a module from disk using the injectable loader (set by CLI layer).
fn auto_load_module(
    module_name: &[String],
    _name_str: &str,
    _parent_env: &Arc<Env>,
) -> Result<(), EvalError> {
    // Try the injectable loader (set by CLI layer, has access to reader + eval)
    let result = call_require_loader(module_name, "", _parent_env);
    match result {
        Some(Ok(module)) => {
            register_module(module);
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err(EvalError::Custom(format!(
            "module not found: {} — use (load) or set up module path",
            module_name.join(".")
        ))),
    }
}