use std::cell::Cell;
use std::rc::Rc;
use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
use zio_core::context::EvalContext;
use zio_core::im::{Vector, vector};
use zio_core::value::{NativeFn, Value};

fn context() -> EvalContext {
    language_context(ModuleRoots::empty()).unwrap()
}

fn run(ctx: &EvalContext, source: &str) -> Value {
    eval_source(ctx, "query-contract.zio", source).unwrap()
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    let Value::Map(map) = value else {
        panic!("expected map, got {value:?}")
    };
    map.get(&Value::Keyword(key.into()))
        .unwrap_or_else(|| panic!("missing {key}"))
}

fn keyword(name: &str) -> Value {
    Value::Keyword(name.into())
}

#[test]
fn queries_do_not_invoke_targets_examples_or_module_loaders() {
    let ctx = context();
    let calls = Rc::new(Cell::new(0));
    let observed = calls.clone();
    ctx.env.set(
        "query-native-probe".into(),
        Value::NativeFunction(NativeFn::new("query-native-probe", move |_, _| {
            observed.set(observed.get() + 1);
            Ok(Value::Nil)
        })),
    );
    let loads = Rc::new(Cell::new(0));
    let observed = loads.clone();
    *ctx.loader.borrow_mut() = Some(Box::new(move |_, _, _| {
        observed.set(observed.get() + 1);
        Err(zio_core::error::EvalError::custom("loader must not run"))
    }));
    let value = run(
        &ctx,
        r#"
(def query-touched 0)
;; doc: Dangerous example, documentation only.
;; doc:
;; doc:   (set! query-touched 99)
(defn query-danger [x] (set! query-touched x))
(defmacro query-macro [x] (do (set! query-touched 88) x))
(describe-symbol 'query-danger)
(describe-symbol 'query-macro)
(describe-symbol 'query-native-probe)
(apropos "query-")
[(get (module-exports :query.unloaded) :status) query-touched]
"#,
    );
    assert_eq!(
        value,
        Value::Vector(vector![keyword("not-loaded"), Value::Integer(0)])
    );
    assert_eq!(calls.get(), 0);
    assert_eq!(loads.get(), 0);
    let description = run(&ctx, "(describe-symbol 'query-danger)");
    let Value::Vector(examples) = field(&description, "examples") else {
        panic!("examples must be a vector")
    };
    assert_eq!(examples.len(), 1);
    assert_eq!(
        field(&examples[0], "text"),
        &Value::String("(set! query-touched 99)".into())
    );
    assert_eq!(field(&examples[0], "source"), field(&description, "source"));
}

#[test]
fn module_queries_expose_only_owned_exports_and_distinguish_namespaces() {
    let ctx = context();
    run(
        &ctx,
        "(def query-scope 7) (module :query-scope (defn public [x] x) (defn secret [] 9) (export public))",
    );
    let result = run(&ctx, "(module-exports :query-scope)");
    let Value::Vector(exports) = field(&result, "exports") else {
        panic!("exports must be a vector")
    };
    assert_eq!(exports.len(), 1);
    assert_eq!(
        field(&exports[0], "name"),
        &Value::String("query-scope/public".into())
    );
    assert_eq!(
        run(&ctx, "(get (describe-symbol 'query-scope/secret) :status)"),
        keyword("not-found")
    );
    assert_eq!(
        run(
            &ctx,
            "(get (describe-symbol 'query-scope/identity) :status)"
        ),
        keyword("not-found")
    );
    assert_eq!(
        run(&ctx, "(get (describe-symbol :query-scope) :kind)"),
        keyword("module")
    );
    assert_eq!(
        run(&ctx, "(get (describe-symbol 'query-scope) :kind)"),
        keyword("value")
    );
}

#[test]
fn documentation_keeps_original_revision_across_redefinition_and_aliases() {
    let ctx = context();
    eval_source(
        &ctx,
        "same.zio",
        ";; doc: Original π documentation.\n(defn π [x] x)\n(def saved-π π)",
    )
    .unwrap();
    let old = run(&ctx, "(describe-symbol 'saved-π)");
    assert_eq!(field(&old, "documentation"), &keyword("available"));
    assert!(matches!(field(&old, "doc"), Value::String(text) if text.contains("Original π")));
    eval_source(&ctx, "same.zio", "(defn π [y & more] y)").unwrap();
    let new = run(&ctx, "(describe-symbol 'π)");
    let saved = run(&ctx, "(describe-symbol 'saved-π)");
    assert_eq!(field(&saved, "doc"), field(&old, "doc"));
    assert_eq!(field(&saved, "source"), field(&old, "source"));
    assert_ne!(
        field(field(&new, "source"), "revision"),
        field(field(&old, "source"), "revision")
    );
    assert_eq!(field(&new, "doc"), &Value::Nil);
    assert_eq!(field(&new, "documentation"), &keyword("missing"));
    assert_eq!(
        field(&new, "params"),
        &Value::Vector(vector![Value::String("y".into())])
    );
    assert_eq!(field(&new, "rest"), &Value::String("more".into()));
}

#[test]
fn search_is_sorted_paged_and_matches_docs_without_leaking_stale_metadata() {
    let ctx = context();
    run(
        &ctx,
        r#"
(defn qpage-c [] 3)
;; doc: unique-query-needle
(defn qpage-a [] 1)
(defn qpage-b [] 2)
"#,
    );
    let first = run(&ctx, "(apropos \"QPAGE-\" 0 2)");
    let Value::Vector(items) = field(&first, "items") else {
        panic!("items must be a vector")
    };
    assert_eq!(
        items
            .iter()
            .map(|item| field(item, "name").clone())
            .collect::<Vector<_>>(),
        vector![
            Value::String("qpage-a".into()),
            Value::String("qpage-b".into())
        ]
    );
    assert_eq!(field(&first, "next-offset"), &Value::Integer(2));
    let second = run(&ctx, "(apropos \"qpage-\" 2 2)");
    let Value::Vector(items) = field(&second, "items") else {
        panic!("items must be a vector")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(field(&items[0], "name"), &Value::String("qpage-c".into()));
    assert_eq!(field(&second, "next-offset"), &Value::Nil);
    let found = run(&ctx, "(apropos \"unique-query-needle\")");
    let Value::Vector(items) = field(&found, "items") else {
        panic!("items must be a vector")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(field(&items[0], "name"), &Value::String("qpage-a".into()));
    run(&ctx, "(def qpage-a 42)");
    let found = run(&ctx, "(apropos \"unique-query-needle\")");
    assert_eq!(field(&found, "items"), &Value::Vector(Vector::new()));
    for invalid in [
        "(apropos \"x\" -1 2)",
        "(apropos \"x\" 0 0)",
        "(apropos \"x\" 0 101)",
        "(apropos \"x\" 1.5 2)",
        "(describe-symbol 12)",
        "(module-exports [])",
    ] {
        assert!(
            eval_source(&ctx, "invalid-query.zio", invalid).is_err(),
            "accepted {invalid}"
        );
    }
}

#[test]
fn unannotated_native_signature_is_unknown_not_zero_arity() {
    let ctx = context();
    ctx.env.set(
        "undocumented-native".into(),
        Value::NativeFunction(NativeFn::new("map", |_, _| Ok(Value::Nil))),
    );
    let result = run(&ctx, "(describe-symbol 'undocumented-native)");
    assert_eq!(field(&result, "kind"), &keyword("native"));
    assert_eq!(field(&result, "params"), &Value::Nil);
    assert_eq!(field(&result, "documentation"), &keyword("unknown"));
    assert_eq!(field(&result, "doc"), &Value::Nil);
    assert_eq!(field(&result, "requires"), &Value::Nil);
}

#[test]
fn query_semantics_are_shared_by_ast_meta_and_explicit_vm() {
    let source = r#"
(defn query-f [x & xs] x)
(defmacro query-m [x] x)
[(get (describe-symbol 'query-f) :kind)
 (get (describe-symbol 'query-f) :params)
 (get (describe-symbol 'query-f) :rest)
 (get (describe-symbol 'query-m) :kind)
 (get (describe-symbol 'map) :kind)
 (get (describe-symbol 'and) :kind)
 (get (describe-symbol 'missing-query) :status)
 (get (language-capabilities) :remote-restart)]
"#;
    let expected = Value::Vector(vector![
        keyword("function"),
        Value::Vector(vector![Value::String("x".into())]),
        Value::String("xs".into()),
        keyword("macro"),
        keyword("native"),
        keyword("special-form"),
        keyword("not-found"),
        Value::Boolean(false)
    ]);
    assert_eq!(
        eval_source(&context(), "shared-query.zio", source).unwrap(),
        expected
    );
    assert_eq!(
        zio_core::interpreter::eval_source(&context(), "shared-query.zio", source).unwrap(),
        expected
    );
    let ctx = context();
    let module = zio_core::bytecode::compile_source(&ctx, "shared-query.zio", source).unwrap();
    assert_eq!(
        zio_core::bytecode::execute_module(&ctx, &module).unwrap(),
        expected
    );
}

#[test]
fn help_uses_host_output_without_invoking_the_described_function() {
    let output = std::sync::Arc::new(zio_core::io::BufferIoHost::new());
    let ctx = zio_core::bootstrap::language_context_with_io(ModuleRoots::empty(), output.clone())
        .unwrap();
    let result = run(
        &ctx,
        ";; doc: Human-readable identity description.\n(defn query-help [x] (error \"must not run\"))\n(help 'query-help)",
    );
    assert_eq!(result, Value::Nil);
    let text = output.get_output();
    assert!(text.contains("query-help"));
    assert!(text.contains("Human-readable identity description."));
    assert!(!text.contains("must not run"));
}

#[test]
fn portable_callable_docs_use_embedded_source_without_running_the_zio_reader() {
    let original = ";; doc: Portable original documentation.\n(defn portable-q [x] x)\n(def saved-portable-q portable-q)";
    let producer = context();
    let module =
        zio_core::bytecode::compile_source(&producer, "portable-docs.zio", original).unwrap();
    let wire = zio_core::bytecode::serialize_module(&module).unwrap();
    let ctx = context();
    let module = zio_core::bytecode::deserialize_module(&wire).unwrap();
    zio_core::bytecode::execute_module(&ctx, &module).unwrap();
    eval_source(
        &ctx,
        "portable-docs.zio",
        ";; doc: Replacement, not original.\n(defn portable-q [y] y)",
    )
    .unwrap();
    let reader_calls = Rc::new(Cell::new(0));
    let observed = reader_calls.clone();
    ctx.env.set(
        "zi-read".into(),
        Value::NativeFunction(NativeFn::new("zi-read", move |_, _| {
            observed.set(observed.get() + 1);
            Err(zio_core::error::EvalError::custom(
                "query must not invoke Zi reader",
            ))
        })),
    );
    let description = run(&ctx, "(describe-symbol 'saved-portable-q)");
    assert_eq!(field(&description, "documentation"), &keyword("available"));
    assert!(
        matches!(field(&description, "doc"), Value::String(doc) if doc.contains("Portable original") && !doc.contains("Replacement"))
    );
    assert_eq!(
        field(&description, "params"),
        &Value::Vector(vector![Value::String("x".into())])
    );
    assert_eq!(
        field(field(&description, "source"), "name"),
        &Value::String("portable-docs.zio".into())
    );
    assert_eq!(reader_calls.get(), 0);
}

#[test]
fn enclosing_function_docs_are_not_taken_from_its_function_valued_body() {
    let ctx = context();
    run(
        &ctx,
        ";; doc: Outer factory documentation.\n(defn query-factory [] (fn [x] x))",
    );
    let outer = run(&ctx, "(describe-symbol 'query-factory)");
    assert!(matches!(field(&outer, "doc"), Value::String(doc) if doc.contains("Outer factory")));
    assert_eq!(field(&outer, "params"), &Value::Vector(Vector::new()));
}

#[test]
fn plain_comment_docs_are_equivalent_and_headers_and_inner_functions_do_not_inherit_them() {
    let ctx = context();
    run(
        &ctx,
        "(def doc-prefix 0)\n;; Plain documented function.\n;;   (plain-doc 2)\n(defn plain-doc [x] x)",
    );
    let plain = run(&ctx, "(describe-symbol 'plain-doc)");
    assert_eq!(field(&plain, "documentation"), &keyword("available"));
    assert!(matches!(field(&plain, "doc"), Value::String(doc) if doc.contains("Plain documented")));
    run(
        &ctx,
        ";; doc: Only outer factory.\n(defn factory-doc [] (fn [x] x))\n(def inner-doc (factory-doc))",
    );
    let inner = run(&ctx, "(describe-symbol 'inner-doc)");
    assert_eq!(field(&inner, "doc"), &Value::Nil);
    eval_source(
        &ctx,
        "header.zio",
        ";; File header, not symbol documentation.\n(defn header-only [x] x)",
    )
    .unwrap();
    assert_eq!(
        run(&ctx, "(get (describe-symbol 'header-only) :documentation)"),
        keyword("missing")
    );
}

#[test]
fn class_bindings_and_generic_functions_are_not_reported_as_instances() {
    let ctx = context();
    let kinds = run(
        &ctx,
        "(defclass QueryClass nil ())\n(def query-instance (make-instance QueryClass))\n(defgeneric query-generic (x))\n[(get (describe-symbol 'QueryClass) :kind) (get (describe-symbol 'query-instance) :kind) (get (describe-symbol 'query-generic) :kind)]",
    );
    assert_eq!(
        kinds,
        Value::Vector(vector![
            keyword("class"),
            keyword("value"),
            keyword("generic-function")
        ])
    );
}
