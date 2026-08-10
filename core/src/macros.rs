use std::sync::Arc;

use im::Vector;

use crate::context::EvalEngine;
use crate::env::Env;
use crate::error::EvalError;
use crate::sexp::Sexp;
use crate::value::{Macro, Value};

/// Apply a macro: bind args (as Sexp data) to params, eval body, return expanded Sexp.
/// If the body is a `syntax-rules` form, use pattern matching instead of eval.
pub fn apply_macro(
    m: &Macro,
    args: &[Sexp],
    _env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Sexp, EvalError> {
    // Check if the macro body is a syntax-rules form
    if let Sexp::List(body_list, _) = &m.body {
        if !body_list.is_empty() {
            if let Sexp::Symbol(name, _) = &body_list[0] {
                if name == "syntax-rules" {
                    return expand_syntax_rules(&m.body, args);
                }
            }
        }
    }

    // Standard macro expansion: bind args, eval body
    let arg_values: Vector<Value> = args.iter().map(|a| Value::from(a.clone())).collect();

    let macro_env = if m.rest_param.is_some() {
        Env::bind_variadic(&m.env, &m.params, &m.rest_param, &arg_values)?
    } else {
        Env::bind(&m.env, &m.params, &arg_values)?
    };

    let result = engine.eval_expr(&m.body, &macro_env, false)?;
    let val = result.into_value();
    value_to_sexp(&val)
}

/// Try to expand a macro call by looking up the name in the env.
/// Returns the expanded Sexp if it's a macro, None otherwise.
pub fn try_expand_by_name(
    name: &str,
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Option<Sexp>, EvalError> {
    if let Some(Value::Macro(m)) = env.get(name) {
        apply_macro(&m, args, env, engine).map(Some)
    } else {
        Ok(None)
    }
}

/// Apply a macro directly from the macro value (for computed-expression macros).
/// Same as apply_macro but exposed with a clearer name for the computed-macro path.
pub fn apply_macro_for_value(
    m: &Macro,
    args: &[Sexp],
    env: &Arc<Env>,
    engine: &dyn EvalEngine,
) -> Result<Sexp, EvalError> {
    apply_macro(m, args, env, engine)
}

/// Convert a Value back to Sexp (for macro expansion results).
/// Fails if the Value contains runtime-only types (Function, NativeFunction, Macro).
pub fn value_to_sexp(value: &Value) -> Result<Sexp, EvalError> {
    match value {
        Value::Nil => Ok(Sexp::Nil),
        Value::Boolean(b) => Ok(Sexp::Boolean(*b)),
        Value::Integer(i) => Ok(Sexp::Integer(*i, None)),
        Value::Float(f) => Ok(Sexp::Float(*f, None)),
        Value::String(s) => Ok(Sexp::String(s.clone(), None)),
        Value::Symbol(s) => Ok(Sexp::Symbol(s.clone(), None)),
        Value::Keyword(k) => Ok(Sexp::Keyword(k.clone(), None)),
        Value::List(l) => {
            let mut new_list = Vector::new();
            for item in l { new_list.push_back(value_to_sexp(item)?); }
            Ok(Sexp::List(new_list, None))
        }
        Value::Vector(v) => {
            let mut new_vec = Vector::new();
            for item in v { new_vec.push_back(value_to_sexp(item)?); }
            Ok(Sexp::Vector(new_vec, None))
        }
        Value::Map(m) => {
            let mut new_map = im::HashMap::new();
            for (k, v) in m { new_map.insert(value_to_sexp(k)?, value_to_sexp(v)?); }
            Ok(Sexp::Map(new_map, None))
        }
        Value::Function(_) => Err(EvalError::macro_error("macro returned a function value")),
        Value::NativeFunction(_) => Err(EvalError::macro_error("macro returned a native function value")),
        Value::Macro(_) => Err(EvalError::macro_error("macro returned a macro value")),
        Value::Char(c) => Ok(Sexp::Char(*c, None)),
        Value::Object(_) => Err(EvalError::macro_error("macro returned an object value")),
    }
}

// ── syntax-rules (pattern-matching macros) ─────────────────────

/// A hygienic macro expansion system using pattern matching.
/// Called when a macro's body is a `syntax-rules` form.
///
/// `rules` is the full form: (syntax-rules (literal ...) (pattern template) ...)
/// `args` are the unevaluated arguments from the macro call.
pub fn expand_syntax_rules(rules: &Sexp, args: &[Sexp]) -> Result<Sexp, EvalError> {
    // Parse: (syntax-rules <literals> <clause>*)
    let list = match rules {
        Sexp::List(l, _) => l,
        _ => return Err(EvalError::macro_error("syntax-rules requires a list form")),
    };
    if list.len() < 3 {
        return Err(EvalError::macro_error("syntax-rules requires (literal ...) and at least one clause"));
    }
    // list[0] = syntax-rules symbol, list[1] = literals list
    let literals = match &list[1] {
        Sexp::List(l, _) | Sexp::Vector(l, _) => {
            let mut ls = Vec::new();
            for item in l {
                if let Sexp::Symbol(s, _) = item {
                    ls.push(s.clone());
                }
            }
            ls
        }
        _ => return Err(EvalError::macro_error("syntax-rules literals must be a list")),
    };

    // Try each clause in order
    for clause in list.iter().skip(2) {
        let clause_list = match clause {
            Sexp::List(l, _) => l,
            _ => return Err(EvalError::macro_error("syntax-rules clause must be a list")),
        };
        if clause_list.len() != 2 {
            return Err(EvalError::macro_error("syntax-rules clause must have (pattern template)"));
        }
        let pattern = &clause_list[0];
        let template = &clause_list[1];

        // Prepend a dummy symbol for the macro name (which is consumed before
        // apply_macro is called, but syntax-rules patterns expect it).
        let mut adjusted_args: Vec<Sexp> = Vec::new();
        adjusted_args.push(Sexp::Symbol("_".into(), None));
        adjusted_args.extend_from_slice(args);

        let mut bindings = Vec::new();
        if match_pattern(pattern, &adjusted_args, &literals, &mut bindings) {
            return Ok(expand_template(template, &bindings, &literals));
        }
    }

    Err(EvalError::macro_error("no matching syntax-rules clause"))
}

/// Try to match `args` against `pattern`, collecting bindings.
/// Returns true if the match succeeds.
fn match_single(pattern: &Sexp, arg: &Sexp, literals: &[String], bindings: &mut Vec<(String, Sexp)>) -> bool {
    match (pattern, arg) {
        // Wildcard
        (Sexp::Symbol(s, _), _) if s == "_" => true,

        // Literal symbol
        (Sexp::Symbol(s, _), Sexp::Symbol(a, _)) if literals.contains(s) => s == a,

        // Pattern variable
        (Sexp::Symbol(s, _), _) if !s.starts_with("...") && s != "_" => {
            if let Some((_, existing)) = bindings.iter().find(|(k, _)| k == s) {
                sexp_equal(existing, arg)
            } else {
                bindings.push((s.clone(), arg.clone()));
                true
            }
        }

        // Empty list
        (Sexp::List(lp, _), Sexp::List(la, _)) if lp.is_empty() => la.is_empty(),

        // List
        (Sexp::List(lp, _), Sexp::List(la, _)) => {
            if lp.len() >= 3 && lp.last().map_or(false, |e| matches!(e, Sexp::Symbol(s, _) if s == "...")) {
                let prefix_end = lp.len() - 2;
                let var_pat = &lp[prefix_end];
                if la.len() < prefix_end { return false; }
                for i in 0..prefix_end {
                    if !match_single(&lp[i], &la[i], literals, bindings) { return false; }
                }
                for i in prefix_end..la.len() {
                    if !match_single(var_pat, &la[i], literals, bindings) { return false; }
                }
                true
            } else {
                if lp.len() != la.len() { return false; }
                for i in 0..lp.len() {
                    if !match_single(&lp[i], &la[i], literals, bindings) { return false; }
                }
                true
            }
        }

        // Vector
        (Sexp::Vector(vp, _), Sexp::Vector(va, _)) => {
            if vp.len() != va.len() { return false; }
            for i in 0..vp.len() {
                if !match_single(&vp[i], &va[i], literals, bindings) { return false; }
            }
            true
        }

        // Literal values
        _ => sexp_equal(pattern, arg),
    }
}

/// Match args (as a list) against pattern.
fn match_pattern(
    pattern: &Sexp,
    args: &[Sexp],
    literals: &[String],
    bindings: &mut Vec<(String, Sexp)>,
) -> bool {
    let wrapped = if args.len() == 1 {
        args[0].clone()
    } else {
        Sexp::List(args.iter().cloned().collect(), None)
    };
    match_single(pattern, &wrapped, literals, bindings)
}

/// Check if two Sexp values are structurally equal (ignoring spans).
fn sexp_equal(a: &Sexp, b: &Sexp) -> bool {
    match (a, b) {
        (Sexp::Nil, Sexp::Nil) => true,
        (Sexp::Boolean(a), Sexp::Boolean(b)) => a == b,
        (Sexp::Integer(a, _), Sexp::Integer(b, _)) => a == b,
        (Sexp::Float(a, _), Sexp::Float(b, _)) => a == b,
        (Sexp::String(a, _), Sexp::String(b, _)) => a == b,
        (Sexp::Symbol(a, _), Sexp::Symbol(b, _)) => a == b,
        (Sexp::Keyword(a, _), Sexp::Keyword(b, _)) => a == b,
        (Sexp::Char(a, _), Sexp::Char(b, _)) => a == b,
        (Sexp::List(a, _), Sexp::List(b, _)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| sexp_equal(a, b))
        }
        (Sexp::Vector(a, _), Sexp::Vector(b, _)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| sexp_equal(a, b))
        }
        (Sexp::Map(a, _), Sexp::Map(b, _)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k) == Some(v))
        }
        _ => false,
    }
}

/// Expand a template by substituting pattern bindings and applying hygiene.
fn expand_template(
    template: &Sexp,
    bindings: &[(String, Sexp)],
    literals: &[String],
) -> Sexp {
    match template {
        // Pattern variable: substitute bound value
        Sexp::Symbol(s, _) if !literals.contains(s) && s != "_" && s != "..." => {
            if let Some((_, val)) = bindings.iter().rev().find(|(k, _)| k == s) {
                val.clone()
            } else {
                // Not a pattern variable — hygienic rename
                Sexp::Symbol(format!("{s}"), None)
            }
        }
        // Literal symbol or special: keep as-is
        Sexp::Symbol(s, _) => Sexp::Symbol(s.clone(), None),

        // List: expand each element
        Sexp::List(items, _) => {
            // Check for ellipsis in template
            let mut result = Vector::new();
            let mut i = 0;
            while i < items.len() {
                if i + 2 < items.len() {
                    if let Sexp::Symbol(s, _) = &items[i + 1] {
                        if s == "..." {
                            // Expand ... : repeat the preceding element for each binding
                            let pattern_item = &items[i];
                            if let Sexp::Symbol(var_name, _) = pattern_item {
                                // Find all bindings for this variable (from ellipsis matching)
                                let vals: Vec<&Sexp> = bindings.iter()
                                    .filter(|(k, _)| k == var_name)
                                    .map(|(_, v)| v)
                                    .collect();
                                for val in vals {
                                    result.push_back(expand_template(val, bindings, literals));
                                }
                            }
                            i += 2; // skip var and ...
                            continue;
                        }
                    }
                }
                result.push_back(expand_template(&items[i], bindings, literals));
                i += 1;
            }
            Sexp::List(result, None)
        }

        // Vector: expand each element
        Sexp::Vector(items, _) => {
            let result: Vector<Sexp> = items.iter()
                .map(|item| expand_template(item, bindings, literals))
                .collect();
            Sexp::Vector(result, None)
        }

        // Map: expand each key and value
        Sexp::Map(m, _) => {
            let mut result = im::HashMap::new();
            for (k, v) in m {
                result.insert(expand_template(k, bindings, literals),
                              expand_template(v, bindings, literals));
            }
            Sexp::Map(result, None)
        }

        // Other literals: keep as-is
        other => other.clone(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins;
    use crate::context::EvalContext;
    use crate::env::Env;

    fn make_ctx() -> EvalContext {
        let env = Arc::new(Env::new(None));
        builtins::setup_env(&env);
        EvalContext::new(env)
    }

    #[test]
    fn test_value_to_sexp_roundtrip() {
        let v = Value::List(im::vector![
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(3),
        ]);
        let s = value_to_sexp(&v).unwrap();
        assert_eq!(s, Sexp::List(im::vector![
            Sexp::Integer(1, None),
            Sexp::Integer(2, None),
            Sexp::Integer(3, None),
        ], None));
    }

    #[test]
    fn test_value_to_sexp_function_error() {
        let f = Value::Function(Arc::new(crate::value::Function {
            params: im::vector![],
            rest_param: None,
            body: Sexp::Nil,
            env: Arc::new(Env::new(None)),
        }));
        assert!(value_to_sexp(&f).is_err());
    }

    #[test]
    fn test_apply_macro() {
        let m = Arc::new(Macro {
            name: "test-macro".into(),
            params: im::vector!["x".into()],
            rest_param: None,
            body: Sexp::Symbol("x".into(), None),
            env: Arc::new(Env::new(None)),
        });

        let ctx = make_ctx();
        let args = [Sexp::Integer(42, None)];
        let result = apply_macro(&m, &args, &ctx.env, &ctx).unwrap();
        assert_eq!(result, Sexp::Integer(42, None));
    }
}
