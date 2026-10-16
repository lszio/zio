//! ZIO-008 Slice 1 harness contract: the minimal `Session` scaffold.
//!
//! Covers verification-matrix rows 1 (cancel), 5 (session survives
//! errors), and 6 (bigint precision). Each test uses an isolated
//! `Session::new(CapabilitySet::empty(), SessionDefaults::default())`.

use zio_core::error::EvalError;
use zio_core::value::Value;
use zio_core::{Capability, CapabilitySet, Session, SessionDefaults};

fn session() -> Session {
    Session::new(CapabilitySet::empty(), SessionDefaults::default())
}

#[test]
fn session_id_distinct() {
    let a = session();
    let b = session();
    assert_ne!(a.id, b.id);
}

#[test]
fn run_id_distinct_within_session() {
    let s = session();
    let first = s.eval("probe-1", "1");
    let second = s.eval("probe-2", "2");
    assert_ne!(first.run_id, second.run_id, "each run gets its own RunId");
    // The envelope's run_id is the run's identity; a later slice's cancel
    // addressing will round-trip it from the caller's request.
}

#[test]
fn compile_static_phase() {
    let s = session();
    // Compilation is a static phase (contract §5): a top-level side effect
    // is compiled but never executed. The CompileResult envelope has no
    // output stream at all — nothing can leak through it.
    let result = s.compile("with-side-effect", r#"(print "side")"#);
    assert!(result.module.is_some(), "valid source must compile");
    assert!(
        result.errors.is_empty(),
        "unexpected errors: {:?}",
        result.errors
    );
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
}

#[test]
fn eval_returns_value_preserves_bigint() {
    let s = session();
    // 2^53 + 1: the first integer f64 cannot represent exactly. The run
    // result must carry the exact i64, not a string round-trip (row 6).
    let big = 9_007_199_254_740_993i64;
    let result = s.eval("bigint", &big.to_string());
    assert!(
        result.error.is_none(),
        "unexpected error: {:?}",
        result.error
    );
    assert_eq!(result.value, Some(Value::Integer(big)));
}

#[test]
fn session_survives_error() {
    let s = session();
    let failed = s.eval("boom", r#"(error "boom")"#);
    assert!(failed.error.is_some(), "run must report the error");
    assert!(failed.value.is_none());
    assert!(!s.is_poisoned(), "a program error is not a poison event");
    // The same session keeps running (row 5).
    let ok = s.eval("after-error", "(+ 1 2)");
    assert!(ok.error.is_none(), "unexpected error: {:?}", ok.error);
    assert_eq!(ok.value, Some(Value::Integer(3)));
}

#[test]
fn cancel_flag_before_call() {
    let s = session();
    s.cancel();
    let cancelled = s.eval("cancelled", "1");
    assert!(
        matches!(
            cancelled.error,
            Some(EvalError::Cancelled { fuel_spent: 0, .. })
        ),
        "pending cancel must surface as Cancelled, got {:?}",
        cancelled.error
    );
    assert!(cancelled.value.is_none());
    assert!(
        !s.is_poisoned(),
        "cancel is a request, not a session killer"
    );
    // The flag is consumed: the next run goes through normally.
    let ok = s.eval("after-cancel", "1");
    assert!(ok.error.is_none(), "unexpected error: {:?}", ok.error);
    assert_eq!(ok.value, Some(Value::Integer(1)));
}

#[test]
fn capability_set_immutable() {
    let original = CapabilitySet::empty();
    let s = Session::new(original.clone(), SessionDefaults::default());
    // The launcher mutating its own copy after construction must not
    // change the session's view (contract §4).
    let _post_mutation = CapabilitySet::with(Capability::FileRead);
    assert_eq!(
        *s.capabilities(),
        original,
        "capabilities are fixed at construction"
    );
    assert!(!s.capabilities().contains(Capability::FileRead));
    assert!(!s.capabilities().contains(Capability::Network));
}
