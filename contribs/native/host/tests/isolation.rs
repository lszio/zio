//! The isolated-execution boundary, exercised through the public API.
//!
//! The claims under test are about what a candidate can *reach*, not
//! about what it computes: a fresh context with no host handles, a
//! refusal that names the capability, and a step ceiling that actually
//! stops a loop rather than merely counting it.

use std::sync::Arc;

use zio_core::context::EvalContext;
use zio_core::env::Env;
use zio_core::io::StdIoHost;
use zio_core::value::Value;

fn context() -> EvalContext {
    let env = Arc::new(Env::new(None));
    zio_core::builtins::setup_env(&env);
    EvalContext::with_io(env, Arc::new(StdIoHost))
}

fn field(envelope: &Value, key: &str) -> String {
    let Value::Map(map) = envelope else {
        panic!("envelope must be a keyword-keyed map, got {envelope:?}");
    };
    match map.get(&Value::Keyword(key.into())) {
        Some(Value::String(text)) => text.clone(),
        Some(other) => format!("{other}"),
        None => "<missing>".to_string(),
    }
}

fn run(source: &str) -> Value {
    let ctx = context();
    let policy = zio_host::HostPolicy::default();
    zio_host::install(&ctx, policy).expect("host install");
    zio_host::execution::evaluate(
        source,
        "candidate.zio",
        None,
        None,
        zio_host::execution::ExecutionLimits::default(),
        Default::default(),
    )
    .expect("evaluate returns an envelope, not a transport error")
}

#[test]
fn a_candidate_computes_without_any_host_grant() {
    let envelope = run("(+ 1 2)");
    assert_eq!(field(&envelope, "status"), "completed");
}

#[test]
fn a_candidate_cannot_reach_a_host_handle() {
    // Every host/* binding lives in the embedding context's environment.
    // The candidate gets a fresh one, so the name is simply absent —
    // which is a different failure from "present but refused", and the
    // first is the stronger guarantee.
    for name in ["host/db-open", "host/process-start", "host/tensor-start"] {
        let source = format!("({name} \"x\")");
        let envelope = run(&source);
        let status = field(&envelope, "status");
        assert!(
            status == "failed" || status == "refused",
            "{name} should not succeed, got {status}: {}",
            field(&envelope, "error")
        );
    }
}

#[test]
fn a_candidate_cannot_re_enter_the_runner() {
    // Recursion through the runner would let a candidate grant itself
    // capabilities by asking a context that has them.
    let envelope = run("(host/evaluate-isolated {:source \"(+ 1 1)\"})");
    assert_ne!(field(&envelope, "status"), "completed");
}

#[test]
fn a_step_ceiling_stops_a_loop() {
    // The claim is that the loop stops, not that it is slow.
    let envelope = run("(loop [n 0] (recur (+ n 1)))");
    let status = field(&envelope, "status");
    assert_eq!(
        status, "failed",
        "a non-terminating program must not complete"
    );
    assert!(
        field(&envelope, "error").contains("step") || field(&envelope, "error").contains("ceiling"),
        "the error should name the ceiling, got {}",
        field(&envelope, "error")
    );
}

#[test]
fn a_frozen_module_loads_and_a_non_frozen_one_does_not() {
    use zio_host::execution::FrozenModules;
    let ctx = context();
    zio_host::install(&ctx, zio_host::HostPolicy::default()).expect("host install");

    let mut modules = FrozenModules::new();
    // A module owns its exports: a bare `defn` is private to the module,
    // so the declaration has to say what leaves. That is the same rule
    // the real loader applies, and skipping it here would test a
    // different language than the one the program runs.
    modules.insert(
        "helper".to_string(),
        b"(defn twice [x] (* 2 x))\n(export twice)".to_vec(),
    );

    let loaded = zio_host::execution::evaluate(
        "(require :helper :refer [twice]) (twice 21)",
        "candidate.zio",
        None,
        None,
        zio_host::execution::ExecutionLimits::default(),
        modules.clone(),
    )
    .expect("evaluate");
    assert_eq!(
        field(&loaded, "status"),
        "completed",
        "a granted dependency must load: {}",
        field(&loaded, "error")
    );

    let refused = zio_host::execution::evaluate(
        "(require :notgranted)",
        "candidate.zio",
        None,
        None,
        zio_host::execution::ExecutionLimits::default(),
        modules,
    )
    .expect("evaluate");
    assert_ne!(
        field(&refused, "status"),
        "completed",
        "an ungranted module must not load"
    );
}
