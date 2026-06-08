use std::sync::Arc;

use crate::env::Env;
use crate::error::EvalError;
use crate::module::{ModuleRegistry, Module};
use crate::sexp::Sexp;
use crate::special::{eval_last_body, EvalFn, TailResult};
use crate::value::Value;

// Global module registry (thread-local)
use std::cell::RefCell;
thread_local! {
    static MODULES: RefCell<ModuleRegistry> = RefCell::new(ModuleRegistry { modules: Vec::new() });
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

    // First arg is the module name (keyword or symbol)
    let name_str = match &args[0] {
        Sexp::Keyword(s) | Sexp::Symbol(s) => s.clone(),
        other => return Err(EvalError::TypeError {
            expected: "keyword or symbol",
            got: other.kind().to_string(),
        }),
    };

    let module_name: Vec<String> = name_str.split('.').map(|s| s.to_string()).collect();
    let body = &args[1..];

    // Create a module env as child of current env
    let module_env = Arc::new(Env::new(Some(env.clone())));

    // Evaluate body in module env
    let result = if body.is_empty() {
        TailResult::Value(Value::Nil)
    } else {
        eval_last_body(body, &module_env, false, eval_fn)?
    };

    // Collect exports: look at what was defined in module_env
    // For now, we can't enumerate env bindings, so we mark it conceptually
    let module = Module {
        name: module_name.clone(),
        env: module_env.clone(),
        exports: Vec::new(), // Will be populated by (export ...)
        source: None,
    };

    MODULES.with(|cell| {
        cell.borrow_mut().register(module);
    });

    Ok(result)
}

/// (require :module-name [:sym1 :sym2 ...])
/// Require and import symbols from a module.
pub fn do_require<'a>(
    args: &[Sexp],
    env: &Arc<Env>,
    _eval_fn: &'a EvalFn<'a>,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::InvalidForm("require needs a module name".into()));
    }

    // First arg: module name
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

    // Check if already loaded
    let already_loaded = MODULES.with(|cell| {
        cell.borrow().find(&module_name).is_some()
    });

    if !already_loaded {
            // Module loading from file in (require) needs zio-reader, which core can't depend on.
            // Use (load) to load files, then (require :module) for already-declared modules.
            return Err(EvalError::Custom(
                "module not loaded — use (load \"path.zio\") first, then (require :module)".into(),
            ));
    }

    // Import symbols
    let module = MODULES.with(|cell| {
        cell.borrow().find(&module_name).cloned()
    });

    let module = match module {
        Some(m) => m,
        None => return Err(EvalError::Custom(format!("module not loaded: {name_str}"))),
    };

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