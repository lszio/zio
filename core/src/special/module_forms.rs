use std::sync::Arc;
use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::special::{eval_last_body, TailResult};
use crate::value::Value;

// ── module ────────────────────────────────────────────────────────

/// (module name ...)
/// Declare a module with optional :export clause.
/// (module :zio.math (:export :pi :sin :cos) body...)
pub fn do_module(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::invalid_form("module requires a name"));
    }

    let module_name = match &args[0] {
        Sexp::Keyword(s, _) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        Sexp::Symbol(s, _) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        other => return Err(EvalError::invalid_form(
            format!("module name must be a keyword or symbol, got {}", other.kind()),
        )),
    };

    if module_name.is_empty() {
        return Err(EvalError::invalid_form("module name cannot be empty"));
    }

    // Parse optional :export clause
    let mut body_start = 1;
    let mut exports = Vec::new();
    if args.len() > 1 {
        if let Sexp::Keyword(k, _) = &args[1] {
            if k == "export" || k == ":export" {
                if args.len() < 3 {
                    return Err(EvalError::invalid_form("module :export requires a list of symbols"));
                }
                let export_list = match &args[2] {
                    Sexp::List(list, _) | Sexp::Vector(list, _) => list,
                    other => return Err(EvalError::invalid_form(
                        format!("module :export must be a list or vector, got {}", other.kind()),
                    )),
                };
                for sym in export_list {
                    match sym {
                        Sexp::Keyword(s, _) => exports.push(s.clone()),
                        Sexp::Symbol(s, _) => exports.push(s.clone()),
                        other => return Err(EvalError::invalid_form(
                            format!("module export symbols must be keywords or symbols, got {}", other.kind()),
                        )),
                    }
                }
                body_start = 3;
            }
        }
    }

    let module_env = Arc::new(Env::new(Some(env.clone())));
    let body = &args[body_start..];
    let result = if body.is_empty() {
        TailResult::Value(Value::Nil)
    } else {
        eval_last_body(body, &module_env, true, engine)?
    };

    let module = crate::module::Module {
        name: module_name,
        env: module_env,
        exports,
        source: None,
    };
    engine.register_module(module);
    Ok(result)
}

// ── require ───────────────────────────────────────────────────────

/// (require :module.name)
/// (require :module.name :refer [sym1 sym2])
/// (require :module.name :as alias)
/// (require :module.name :as alias :refer [sym1 sym2])
///
/// Loads a module and optionally imports symbols into the current env.
/// :as creates an alias map in the current env for prefix access.
/// :refer selectively imports named symbols into the current env.
pub fn do_require(
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<TailResult, EvalError> {
    if args.is_empty() {
        return Err(EvalError::invalid_form("require requires a module name"));
    }

    let module_name = match &args[0] {
        Sexp::Keyword(s, _) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        Sexp::Symbol(s, _) => s.split('.').map(|p| p.to_string()).collect::<Vec<_>>(),
        other => return Err(EvalError::invalid_form(
            format!("require requires a module name (keyword or symbol), got {}", other.kind()),
        )),
    };

    // Parse :as and :refer
    let mut alias: Option<String> = None;
    let mut refer: Option<Vec<String>> = None;
    let mut i = 1;
    while i < args.len() {
        match &args[i] {
            Sexp::Keyword(k, _) | Sexp::Symbol(k, _) if k == "as" || k == ":as" => {
                i += 1;
                if i >= args.len() {
                    return Err(EvalError::invalid_form("require :as expects an alias name"));
                }
                alias = Some(match &args[i] {
                    Sexp::Symbol(s, _) => s.clone(),
                    other => return Err(EvalError::invalid_form(
                        format!("require :as expects a symbol, got {}", other.kind()),
                    )),
                });
                i += 1;
            }
            Sexp::Keyword(k, _) | Sexp::Symbol(k, _) if k == "refer" || k == ":refer" => {
                i += 1;
                if i >= args.len() {
                    return Err(EvalError::invalid_form("require :refer expects a list of symbols"));
                }
                let sym_list = match &args[i] {
                    Sexp::List(list, _) | Sexp::Vector(list, _) => list,
                    other => return Err(EvalError::invalid_form(
                        format!("require :refer expects a list or vector, got {}", other.kind()),
                    )),
                };
                let mut syms = Vec::new();
                for sym in sym_list {
                    match sym {
                        Sexp::Keyword(s, _) => syms.push(s.clone()),
                        Sexp::Symbol(s, _) => syms.push(s.clone()),
                        other => return Err(EvalError::invalid_form(
                            format!("require :refer list items must be symbols or keywords"),
                        )),
                    }
                }
                refer = Some(syms);
                i += 1;
            }
            other => return Err(EvalError::invalid_form(
                format!("require: unexpected argument {}, expected :as or :refer", other.kind()),
            )),
        }
    }

    // Auto-load from disk if not already loaded
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

    let module = engine.find_module(&module_name).ok_or_else(|| {
        EvalError::custom(format!("module not found after load: {}", module_name.join(".")))
    })?;

    // :refer — import specific symbols into current env
    let target_syms = refer.unwrap_or_else(|| module.exports.clone());
    for sym_name in &target_syms {
        if let Some(val) = module.env.get(sym_name) {
            env.set(sym_name.clone(), val);
        }
    }

    // :as — create an alias map
    if let Some(alias_name) = alias {
        let alias_syms = if module.exports.is_empty() {
            // No explicit exports — make all module symbols available
            // We use a placeholder; for now, just use target_syms
            target_syms.clone()
        } else {
            module.exports.clone()
        };
        let mut map = im::HashMap::new();
        for sym_name in &alias_syms {
            if let Some(val) = module.env.get(sym_name) {
                map.insert(Value::Keyword(sym_name.clone()), val);
            }
        }
        env.set(alias_name, Value::Map(map));
    }

    Ok(TailResult::Value(Value::Nil))
}
