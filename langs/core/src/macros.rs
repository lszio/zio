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
        Value::Buffer(_) => Err(EvalError::macro_error("macro returned a buffer value")),
        Value::Future(_) => Err(EvalError::macro_error("macro returned a future value")),
        Value::Channel(_) => Err(EvalError::macro_error("macro returned a channel value")),
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

    // Prepend a dummy symbol for the macro name (which is consumed before
    // apply_macro is called, but syntax-rules patterns expect it).
    let mut adjusted_args: Vec<Sexp> = Vec::new();
    adjusted_args.push(Sexp::Symbol("_".into(), None));
    adjusted_args.extend_from_slice(args);

    // Try each clause in order. Each clause is either a 2-element list
    // (pattern, template) OR a flat list of length 2 flattened to (pattern, template).
    for clause in list.iter().skip(2) {
        let clause_list = match clause {
            Sexp::List(l, _) if l.len() == 2 => l,
            Sexp::List(l, _) if l.len() == 1 => {
                // Allow (((pat tmpl))) — outer list with one inner list
                if let Sexp::List(inner, _) = &l[0] {
                    if inner.len() == 2 { inner } else { l }
                } else {
                    l
                }
            }
            _ => return Err(EvalError::macro_error("syntax-rules clause must have (pattern template)")),
        };
        if clause_list.len() != 2 {
            return Err(EvalError::macro_error("syntax-rules clause must have (pattern template)"));
        }
        let pattern = &clause_list[0];
        let template = &clause_list[1];
        let mut bindings = Vec::new();
        if match_pattern(pattern, &adjusted_args, &literals, &mut bindings) {
            let mut hygiene_map = std::collections::HashMap::new();
            return Ok(expand_template(template, &bindings, &literals, &mut hygiene_map));
        }
    }

    Err(EvalError::macro_error("no matching syntax-rules clause"))
}

/// Try to match `args` against `pattern`, collecting bindings.
/// Returns true if the match succeeds.
///
/// `in_ellipsis` marks a match inside an ellipsis repetition: a pattern
/// variable there binds each element (appends a new binding) instead of
/// acting as an equality constraint on an earlier binding.
fn match_single(
    pattern: &Sexp,
    arg: &Sexp,
    literals: &[String],
    bindings: &mut Vec<(String, Sexp)>,
    is_root_head: bool,
    in_ellipsis: bool,
) -> bool {
    match (pattern, arg) {
        // Root head (macro name position in pattern list) — consume without binding
        (Sexp::Symbol(_, _), _) if is_root_head => true,

        // Wildcard
        (Sexp::Symbol(s, _), _) if s == "_" => true,

        // Literal symbol
        (Sexp::Symbol(s, _), Sexp::Symbol(a, _)) if literals.contains(s) => s == a,

        // Pattern variable
        (Sexp::Symbol(s, _), _) if !s.starts_with("...") => {
            if in_ellipsis {
                // Ellipsis repetition: each element adds a binding.
                bindings.push((s.clone(), arg.clone()));
                true
            } else if let Some((_, existing)) = bindings.iter().find(|(k, _)| k == s) {
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
            let has_ellipsis = lp.len() >= 2 && matches!(lp.last(), Some(Sexp::Symbol(s, _)) if s == "...");
            if has_ellipsis {
                let prefix_end = lp.len() - 2;
                let var_pat = &lp[prefix_end];
                if la.len() < prefix_end { return false; }
                for i in 0..prefix_end {
                    let head = is_root_head && i == 0;
                    if !match_single(&lp[i], &la[i], literals, bindings, head, false) { return false; }
                }
                for i in prefix_end..la.len() {
                    if !match_single(var_pat, &la[i], literals, bindings, false, true) { return false; }
                }
                true
            } else {
                if lp.len() != la.len() { return false; }
                for i in 0..lp.len() {
                    let head = is_root_head && i == 0;
                    if !match_single(&lp[i], &la[i], literals, bindings, head, false) { return false; }
                }
                true
            }
        }

        // Vector
        (Sexp::Vector(vp, _), Sexp::Vector(va, _)) => {
            if vp.len() != va.len() { return false; }
            for i in 0..vp.len() {
                if !match_single(&vp[i], &va[i], literals, bindings, false, false) { return false; }
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
    // Always wrap as a list: args carry the dummy head symbol from
    // expand_syntax_rules, so a zero-argument call yields a 1-element list
    // and must not collapse to a bare symbol.
    let wrapped = Sexp::List(args.iter().cloned().collect(), None);
    match_single(pattern, &wrapped, literals, bindings, true, false)
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

static HYGIENE_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// Rename a template-introduced identifier hygienically.
fn hygienic_rename(
    s: &str,
    hygiene_map: &mut std::collections::HashMap<String, String>,
) -> Sexp {
    let renamed = hygiene_map.entry(s.to_string()).or_insert_with(|| {
        let id = HYGIENE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{s}__hyg_{id}")
    });
    Sexp::Symbol(renamed.clone(), None)
}

/// Expand a template by substituting pattern bindings and applying hygiene.
fn expand_template(
    template: &Sexp,
    bindings: &[(String, Sexp)],
    literals: &[String],
    hygiene_map: &mut std::collections::HashMap<String, String>,
) -> Sexp {
    expand_template_scoped(template, bindings, literals, hygiene_map, &mut Vec::new())
}

/// Expand one binder-position element. A plain symbol is introduced by the
/// template: it gets a hygienic name and joins `scope`. A pattern variable
/// substitutes caller data unchanged.
fn expand_binder(
    binder: &Sexp,
    bindings: &[(String, Sexp)],
    literals: &[String],
    hygiene_map: &mut std::collections::HashMap<String, String>,
    scope: &mut Vec<String>,
) -> Sexp {
    if let Sexp::Symbol(s, _) = binder {
        let is_pattern_var = s != "_"
            && !literals.contains(s)
            && bindings.iter().any(|(k, _)| k == s);
        if !is_pattern_var && s != "_" {
            scope.push(s.clone());
            return hygienic_rename(s, hygiene_map);
        }
    }
    expand_template_scoped(binder, bindings, literals, hygiene_map, scope)
}

/// Expand a template by substituting pattern bindings and applying hygiene.
///
/// Hygiene semantics: only identifiers *introduced* by the template as
/// bindings (let/let*/loop binding pairs, fn params, def/defn/defn names) are
/// renamed, together with references to them. Free references — builtins,
/// definition-site helpers, special forms — are preserved verbatim so they
/// resolve in the caller's environment. `scope` carries the
/// template-introduced binders visible at the current position.
fn expand_template_scoped(
    template: &Sexp,
    bindings: &[(String, Sexp)],
    literals: &[String],
    hygiene_map: &mut std::collections::HashMap<String, String>,
    scope: &mut Vec<String>,
) -> Sexp {
    match template {
        // Pattern variable: substitute bound value
        Sexp::Symbol(s, _) if !literals.contains(s) && s != "_" && s != "..." => {
            if let Some((_, val)) = bindings.iter().rev().find(|(k, _)| k == s) {
                val.clone()
            } else if scope.iter().any(|b| b == s) || hygiene_map.contains_key(s) {
                // Reference to a template-introduced binder: rename
                // consistently with its binding site.
                hygienic_rename(s, hygiene_map)
            } else {
                // Free reference: keep — resolves in the caller's environment.
                Sexp::Symbol(s.clone(), None)
            }
        }
        // Literal symbol or special: keep as-is
        Sexp::Symbol(s, _) => Sexp::Symbol(s.clone(), None),

        // List: expand each element
        Sexp::List(items, _) => {
            // Binder forms introduce scoped names; recurse with an extended
            // scope so references to the new bindings rename consistently.
            let head = items.get(0).and_then(|s| match s {
                Sexp::Symbol(h, _) => Some(h.as_str()),
                _ => None,
            });
            match head {
                Some("let") | Some("let*") | Some("loop") if items.len() >= 2 => {
                    let mut inner = scope.clone();
                    let mut new_items = Vector::new();
                    new_items.push_back(items[0].clone());
                    if let Sexp::Vector(pairs, _) = &items[1] {
                        let mut new_pairs = Vector::new();
                        for (p, pair) in pairs.iter().enumerate() {
                            if p % 2 == 0 {
                                new_pairs.push_back(expand_binder(pair, bindings, literals, hygiene_map, &mut inner));
                            } else {
                                new_pairs.push_back(expand_template_scoped(pair, bindings, literals, hygiene_map, &mut inner));
                            }
                        }
                        new_items.push_back(Sexp::Vector(new_pairs, None));
                    } else {
                        new_items.push_back(expand_template_scoped(&items[1], bindings, literals, hygiene_map, scope));
                    }
                    for item in items.iter().skip(2) {
                        new_items.push_back(expand_template_scoped(item, bindings, literals, hygiene_map, &mut inner));
                    }
                    return Sexp::List(new_items, None);
                }
                // The def'd name is not visible to its own value expression.
                Some("def") | Some("defmacro") if items.len() >= 2 => {
                    let mut scratch = scope.clone();
                    let mut new_items = Vector::new();
                    new_items.push_back(items[0].clone());
                    new_items.push_back(expand_binder(&items[1], bindings, literals, hygiene_map, &mut scratch));
                    for item in items.iter().skip(2) {
                        new_items.push_back(expand_template_scoped(item, bindings, literals, hygiene_map, scope));
                    }
                    return Sexp::List(new_items, None);
                }
                Some("defn") if items.len() >= 3 => {
                    let mut inner = scope.clone();
                    let mut new_items = Vector::new();
                    new_items.push_back(items[0].clone());
                    new_items.push_back(expand_binder(&items[1], bindings, literals, hygiene_map, &mut inner));
                    if let Sexp::Vector(params, _) = &items[2] {
                        let mut new_params = Vector::new();
                        for param in params.iter() {
                            new_params.push_back(expand_binder(param, bindings, literals, hygiene_map, &mut inner));
                        }
                        new_items.push_back(Sexp::Vector(new_params, None));
                    } else {
                        new_items.push_back(expand_template_scoped(&items[2], bindings, literals, hygiene_map, scope));
                    }
                    for item in items.iter().skip(3) {
                        new_items.push_back(expand_template_scoped(item, bindings, literals, hygiene_map, &mut inner));
                    }
                    return Sexp::List(new_items, None);
                }
                Some("fn") if items.len() >= 2 => {
                    let mut inner = scope.clone();
                    let mut new_items = Vector::new();
                    new_items.push_back(items[0].clone());
                    if let Sexp::Vector(params, _) = &items[1] {
                        let mut new_params = Vector::new();
                        for param in params.iter() {
                            new_params.push_back(expand_binder(param, bindings, literals, hygiene_map, &mut inner));
                        }
                        new_items.push_back(Sexp::Vector(new_params, None));
                    } else {
                        new_items.push_back(expand_template_scoped(&items[1], bindings, literals, hygiene_map, scope));
                    }
                    for item in items.iter().skip(2) {
                        new_items.push_back(expand_template_scoped(item, bindings, literals, hygiene_map, &mut inner));
                    }
                    return Sexp::List(new_items, None);
                }
                _ => {}
            }

            // Check for ellipsis in template
            let mut result = Vector::new();
            let mut i = 0;
            while i < items.len() {
                if i + 1 < items.len() {
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
                                    // Repeated segments are matched caller data;
                                    // expand with a fresh scope so template
                                    // binders do not leak into them.
                                    // ponytail: a caller symbol colliding with a
                                    // template binder name can still be renamed;
                                    // upgrade path is a marked-identifier pass.
                                    result.push_back(expand_template_scoped(
                                        val,
                                        bindings,
                                        literals,
                                        hygiene_map,
                                        &mut Vec::new(),
                                    ));
                                }
                            }
                            i += 2; // skip var and ...
                            continue;
                        }
                    }
                }
                result.push_back(expand_template_scoped(&items[i], bindings, literals, hygiene_map, scope));
                i += 1;
            }
            Sexp::List(result, None)
        }

        // Vector: expand each element
        Sexp::Vector(items, _) => {
            let result: Vector<Sexp> = items.iter()
                .map(|item| expand_template_scoped(item, bindings, literals, hygiene_map, scope))
                .collect();
            Sexp::Vector(result, None)
        }

        // Map: expand each key and value
        Sexp::Map(m, _) => {
            let mut result = im::HashMap::new();
            for (k, v) in m {
                result.insert(expand_template_scoped(k, bindings, literals, hygiene_map, scope),
                              expand_template_scoped(v, bindings, literals, hygiene_map, scope));
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
    #[test]
    fn test_syntax_rules_swap() {
        // (syntax-rules () ((swap! a b) (let [tmp a] (do (set! a b) (set! b tmp)))))
        let rules = Sexp::List(im::vector![
            Sexp::Symbol("syntax-rules".into(), None),
            Sexp::List(im::vector![], None),
            Sexp::List(im::vector![
                Sexp::List(im::vector![
                    Sexp::Symbol("swap!".into(), None),
                    Sexp::Symbol("a".into(), None),
                    Sexp::Symbol("b".into(), None),
                ], None),
                Sexp::List(im::vector![
                    Sexp::Symbol("let".into(), None),
                    Sexp::Vector(im::vector![
                        Sexp::Symbol("tmp".into(), None),
                        Sexp::Symbol("a".into(), None),
                    ], None),
                    Sexp::List(im::vector![
                        Sexp::Symbol("set!".into(), None),
                        Sexp::Symbol("a".into(), None),
                        Sexp::Symbol("b".into(), None),
                    ], None),
                    Sexp::List(im::vector![
                        Sexp::Symbol("set!".into(), None),
                        Sexp::Symbol("b".into(), None),
                        Sexp::Symbol("tmp".into(), None),
                    ], None),
                ], None),
            ], None),
        ], None);

        let args = [Sexp::Symbol("x".into(), None), Sexp::Symbol("y".into(), None)];
        let expanded = expand_syntax_rules(&rules, &args).unwrap();
        if let Sexp::List(items, _) = &expanded {
            assert_eq!(items[0], Sexp::Symbol("let".into(), None));
            if let Sexp::Vector(bindings, _) = &items[1] {
                assert_eq!(bindings[1], Sexp::Symbol("x".into(), None));
                // Local variable tmp is hygienically renamed
                let renamed_tmp = bindings[0].to_string();
                assert!(renamed_tmp.starts_with("tmp__hyg_"));
            } else {
                panic!("expected bindings vector");
            }
            // Body reference to the introduced binder renames consistently,
            // while free references (set!) stay untouched.
            let renamed_tmp = items[1].to_string();
            let body = items[3].to_string();
            let binder = &renamed_tmp[renamed_tmp.find("tmp__hyg_").unwrap()..]
                .split(&[' ', ']'][..])
                .next()
                .unwrap()
                .to_string();
            assert!(body.starts_with("(set! y "), "body was {body:?}");
            assert!(body.ends_with(&format!("{binder})")), "body was {body:?}");
        } else {
            panic!("expected expanded list");
        }
    }

    fn read(src: &str) -> Vec<Sexp> {
        crate::reader::reader::read_program(src).expect("read test program")
    }

    fn eval_all(ctx: &EvalContext, src: &str) -> crate::value::Value {
        let mut last = crate::value::Value::Nil;
        for form in read(src) {
            last = crate::eval::eval_in_context(&form, ctx).expect("eval test form");
        }
        last
    }

    #[test]
    fn test_syntax_rules_zero_argument_macro() {
        let ctx = make_ctx();
        let val = eval_all(
            &ctx,
            "(defmacro answer (syntax-rules () ((answer) 42)))(answer)",
        );
        assert_eq!(val, crate::value::Value::Integer(42));
    }

    #[test]
    fn test_syntax_rules_free_references_resolve() {
        // Templates keep free references (builtins, def-site helpers) intact:
        // the old renamer produced list__hyg_N / helper__hyg_N and failed.
        let ctx = make_ctx();
        let val = eval_all(
            &ctx,
            "(defn helper [v] (* v 2))
             (defmacro pair-with (syntax-rules () ((pair-with x) (list (helper x) x))))
             (pair-with 21)",
        );
        assert_eq!(
            val,
            crate::value::Value::List(im::vector![
                crate::value::Value::Integer(42),
                crate::value::Value::Integer(21),
            ])
        );
    }

    #[test]
    fn test_syntax_rules_ellipsis_varargs() {
        let ctx = make_ctx();
        // Trailing ellipsis: prefix binds, rest collects every element.
        let val = eval_all(
            &ctx,
            "(defmacro mylist (syntax-rules () ((mylist head rest ...) (list head rest ...))))
             (mylist 1 2 3)",
        );
        assert_eq!(
            val,
            crate::value::Value::List(im::vector![
                crate::value::Value::Integer(1),
                crate::value::Value::Integer(2),
                crate::value::Value::Integer(3),
            ])
        );
    }

    #[test]
    fn test_syntax_rules_repeated_pattern_var_constrains() {
        // Outside an ellipsis, a repeated pattern variable is an equality
        // constraint: (eq2 5 5) matches, (eq2 5 6) must not.
        let ctx = make_ctx();
        let val = eval_all(
            &ctx,
            "(defmacro eq2 (syntax-rules () ((eq2 x x) 'same)))(eq2 5 5)",
        );
        assert_eq!(val, crate::value::Value::Symbol("same".into()));

        let ctx = make_ctx();
        let forms = read(
            "(defmacro eq2 (syntax-rules () ((eq2 x x) 'same)))(eq2 5 6)",
        );
        crate::eval::eval_in_context(&forms[0], &ctx).expect("define eq2");
        let err = crate::eval::eval_in_context(&forms[1], &ctx)
            .err()
            .expect("(eq2 5 6) must not match");
        assert!(format!("{err}").contains("no matching syntax-rules clause"));
    }
}
