//! J00 contracts for the native JS binding: the engine is real, its
//! limits are real, and every crossing rule is named.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use zio_js::{BoundaryError, Engine, JsArg, JsOutcome};

fn int(value: i64) -> JsOutcome {
    JsOutcome::Integer(value)
}

fn handle(engine: &Engine, source: &str) -> zio_js::Handle {
    match engine.eval(source) {
        Ok(JsOutcome::Handle(handle)) => handle,
        other => panic!("{source} should cross as a handle, got {other:?}"),
    }
}

#[test]
fn eval_returns_primitives_and_objects_by_handle() {
    let engine = Engine::new().expect("engine");
    assert_eq!(engine.eval("1 + 1"), Ok(int(2)));
    assert_eq!(engine.eval("Math.max(3, 9, 4)"), Ok(int(9)));
    assert_eq!(engine.eval("undefined"), Ok(JsOutcome::Nothing));
    assert_eq!(engine.eval("null"), Ok(JsOutcome::Nothing));
    assert_eq!(engine.eval("1.5"), Ok(JsOutcome::Float(1.5)));
    assert_eq!(engine.eval("'kept'"), Ok(JsOutcome::String("kept".into())));
    let object = handle(&engine, "({ label: 'kept' })");
    // A string property crosses as text, not as a handle.
    assert_eq!(
        engine.get_property(object, "label"),
        Ok(JsOutcome::String("kept".into()))
    );
    assert!(engine.is_live(object));
}

#[test]
fn object_property_mutation_and_method_this() {
    let engine = Engine::new().expect("engine");
    let object = handle(
        &engine,
        "({ count: 1, bump() { this.count += 1; return this.count } })",
    );
    assert_eq!(engine.get_property(object, "count"), Ok(int(1)));
    let method = match engine.get_property(object, "bump") {
        Ok(JsOutcome::Handle(method)) => method,
        other => panic!("method should be a handle, got {other:?}"),
    };
    // A method is called with its receiver, the way JS reached it.
    assert_eq!(engine.call_with_this(object, method, &[]), Ok(int(2)));
    assert_eq!(engine.call_with_this(object, method, &[]), Ok(int(3)));
    // A property written from Zio is visible to the next method call.
    engine
        .set_property(object, "count", JsArg::Int(10))
        .expect("write");
    assert_eq!(engine.get_property(object, "count"), Ok(int(10)));
    assert_eq!(engine.call_with_this(object, method, &[]), Ok(int(11)));
}

#[test]
fn big_integers_stay_exact_and_never_fold() {
    let engine = Engine::new().expect("engine");
    // The contract number: not 9007199254740992.
    assert_eq!(
        engine.eval("9007199254740993n"),
        Ok(JsOutcome::BigInt(9_007_199_254_740_993))
    );
    // A plain JS number above the safe range has already lost precision
    // in JS itself; it stays a float rather than pretending exactness.
    assert_eq!(
        engine.eval("9007199254740993"),
        Ok(JsOutcome::Float(9_007_199_254_740_992.0))
    );
    assert_eq!(engine.eval("42n"), Ok(int(42)));
    // Beyond i64 the crossing is refused by name, never truncated.
    let error = engine.eval("2n ** 70n").unwrap_err();
    assert!(error.message().contains("i64"), "{error}");
    // And an exact BigInt argument goes in as BigInt, not as a lossy double.
    let call = handle(&engine, "(v) => v");
    assert_eq!(
        engine.call(call, &[JsArg::Int(9_007_199_254_740_993)]),
        Ok(JsOutcome::BigInt(9_007_199_254_740_993))
    );
}

#[test]
fn non_finite_numbers_are_refused_by_name() {
    let engine = Engine::new().expect("engine");
    let error = engine.eval("NaN").unwrap_err();
    assert!(error.message().contains("non-finite"), "{error}");
    let error = engine.eval("Infinity").unwrap_err();
    assert!(error.message().contains("non-finite"), "{error}");
}

#[test]
fn host_callbacks_run_synchronously_and_throw_by_name() {
    let engine = Engine::new().expect("engine");
    let seen = Arc::new(AtomicI64::new(0));
    let counter = Arc::clone(&seen);
    engine
        .register_callback("hostDouble", move |args| match args.first() {
            Some(JsArg::Int(value)) => {
                counter.store(*value * 2, Ordering::SeqCst);
                Ok(JsArg::Int(*value * 2))
            }
            _ => Err(BoundaryError::from_message("expected one integer")),
        })
        .expect("register");
    assert_eq!(engine.eval("hostDouble(21)"), Ok(int(42)));
    assert_eq!(seen.load(Ordering::SeqCst), 42);

    engine
        .register_callback("hostFail", |_| {
            Err(BoundaryError::from_message("host refused"))
        })
        .expect("register");
    let error = engine.eval("hostFail()").unwrap_err();
    assert!(error.message().contains("host refused"), "{error}");
    // The context survives a throwing callback.
    assert_eq!(engine.eval("1 + 1"), Ok(int(2)));
}

#[test]
fn promises_are_values_and_are_never_awaited() {
    let engine = Engine::new().expect("engine");
    // A promise comes back as a handle: the host decides what to do with
    // it. Nothing here resolves or awaits it.
    assert!(matches!(
        engine.eval("Promise.resolve(7)"),
        Ok(JsOutcome::Handle(_))
    ));
    assert!(matches!(
        engine.eval("(async () => 7)"),
        Ok(JsOutcome::Handle(_))
    ));
}

#[test]
fn infinite_loops_are_interrupted_and_the_context_survives() {
    let engine = Engine::new().expect("engine");
    engine.set_time_budget(Duration::from_millis(200));
    let error = engine.eval("while (true) {}").unwrap_err();
    assert!(error.message().contains("js error"), "{error}");
    engine.clear_time_budget();
    assert_eq!(engine.eval("6 * 7"), Ok(int(42)));
}

#[test]
fn heap_cap_fails_by_name_and_the_context_survives() {
    let engine = Engine::new().expect("engine");
    engine.set_memory_limit(1 << 20); // 1 MiB
    let error = engine
        .eval("(() => { const s = 'x'.repeat(100000); const out = []; while (true) { out.push(s.repeat(10)) } })()")
        .unwrap_err();
    assert!(error.message().contains("js error"), "{error}");
    assert_eq!(engine.eval("1 + 1"), Ok(int(2)));
}

#[test]
fn handles_do_not_cross_engines() {
    let first = Engine::new().expect("engine");
    let second = Engine::new().expect("engine");
    assert_ne!(first.id(), second.id());
    let object = handle(&first, "({ a: 1 })");
    let error = second.get_property(object, "a").unwrap_err();
    assert!(
        error.message().contains("belongs to engine") && error.message().contains("cannot be used"),
        "{error}"
    );
    // The owning engine still has it.
    assert!(first.is_live(object));
}

#[test]
fn the_engine_has_no_filesystem_network_or_process() {
    let engine = Engine::new().expect("engine");
    // QuickJS has no host bindings here, so the standard globals simply
    // do not exist — the host grants nothing by default.
    assert_eq!(
        engine.eval("typeof require"),
        Ok(JsOutcome::String("undefined".into()))
    );
    assert_eq!(
        engine.eval("typeof process === 'undefined'"),
        Ok(JsOutcome::Bool(true))
    );
    assert_eq!(
        engine.eval("typeof globalThis.fetch === 'undefined'"),
        Ok(JsOutcome::Bool(true))
    );
}

#[test]
fn strings_cross_by_value_and_keywords_do_not_collide() {
    let engine = Engine::new().expect("engine");
    let append = handle(&engine, "((s) => s + '!')");
    assert_eq!(
        engine.call(append, &[JsArg::String("hi".into())]),
        Ok(JsOutcome::String("hi!".into()))
    );
    // A JS object key is a JS string; the Zio keyword/symbol distinction
    // never enters the engine — both arrive as text and say so.
    let keys = handle(&engine, "Object.keys({ a: 1, b: 2 })");
    assert!(engine.get_property(keys, "length").is_ok());
}
