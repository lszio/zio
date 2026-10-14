//! The `js/*` language surface, exercised through a real interpreter:
//! the engine binding's contract lives in `js_contract.rs`; this file
//! proves the same rules hold once values cross as Zio data.

use std::rc::Rc;

use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
use zio_core::context::EvalContext;

fn zio_context() -> Rc<EvalContext> {
    let ctx = Rc::new(language_context(ModuleRoots::empty()).expect("language context"));
    zio_js::interop::register(Rc::clone(&ctx));
    ctx
}

fn run(ctx: &EvalContext, source: &str) -> zio_core::value::Value {
    eval_source(ctx, "test.zio", source).expect("evaluates")
}

#[test]
fn zio_calls_the_engine_and_sees_exact_values() {
    let ctx = zio_context();
    // js/new returns an engine id; js/eval runs source on it.
    assert_eq!(
        run(&ctx, "(js/eval (js/new 0) \"1 + 1\")"),
        zio_core::value::Value::Integer(2)
    );
    assert_eq!(
        run(&ctx, "(js/eval (js/new 0) \"Math.max(3, 9, 4)\")"),
        zio_core::value::Value::Integer(9)
    );
    // The contract number stays exact through the whole crossing.
    assert_eq!(
        run(&ctx, "(js/eval (js/new 0) \"9007199254740993n\")"),
        zio_core::value::Value::Integer(9_007_199_254_740_993)
    );
    // Strings cross as Zio strings, not as opaque handles.
    assert_eq!(
        run(&ctx, "(js/eval (js/new 0) \"'kept'\")"),
        zio_core::value::Value::String("kept".into())
    );
}

#[test]
fn zio_mutates_objects_and_calls_methods_through_handles() {
    let ctx = zio_context();
    let value = run(
        &ctx,
        r#"
        (let [e (js/new 0)
              obj (js/eval e "({ count: 1 })")
              _ (js/set e obj "count" 10)]
          (js/eval e "({ count: 1 }).count"))
        "#,
    );
    assert_eq!(value, zio_core::value::Value::Integer(1));

    let count = run(
        &ctx,
        r#"
        (let [e (js/new 0)
              obj (js/eval e "({ count: 1 })")]
          (do (js/set e obj "count" 42)
              (js/get e obj "count")))
        "#,
    );
    assert_eq!(count, zio_core::value::Value::Integer(42));
}

#[test]
fn zio_callbacks_bridge_both_directions() {
    let ctx = zio_context();
    let doubled = run(
        &ctx,
        r#"
        (let [e (js/new 0)
              _ (js/on e "hostDouble" (fn [n] (* n 2)))]
          (js/eval e "hostDouble(21)"))
        "#,
    );
    assert_eq!(doubled, zio_core::value::Value::Integer(42));

    // A failing Zio callback surfaces as an error, by name.
    let error = eval_source(
        &ctx,
        "cb.zio",
        r#"
        (let [e (js/new 0)
              _ (js/on e "hostFail" (fn [] (error "callback refused")))]
          (js/eval e "hostFail()"))
        "#,
    );
    let message = error
        .expect_err("callback failure must surface")
        .to_string();
    assert!(message.contains("callback refused"), "{message}");
}

#[test]
fn handles_do_not_cross_engines_in_zio_either() {
    let ctx = zio_context();
    let error = eval_source(
        &ctx,
        "cross.zio",
        r#"
        (let [a (js/new 0)
              b (js/new 0)
              obj (js/eval a "({ x: 1 })")]
          (js/get b obj "x"))
        "#,
    );
    let message = error
        .expect_err("cross-engine handle must fail")
        .to_string();
    assert!(
        message.contains("belongs to engine") && message.contains("cannot be used"),
        "{message}"
    );
}

#[test]
fn non_finite_and_bigint_overflows_are_named_in_zio() {
    let ctx = zio_context();
    let error = eval_source(&ctx, "nan.zio", r#"(js/eval (js/new 0) "NaN")"#)
        .expect_err("NaN must be refused");
    assert!(error.to_string().contains("non-finite"), "{error}");

    let error = eval_source(&ctx, "big.zio", r#"(js/eval (js/new 0) "2n ** 70n")"#)
        .expect_err("an i64-overflow BigInt must be refused");
    assert!(error.to_string().contains("i64"), "{error}");
}
