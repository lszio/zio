//! G02 execution contract: what a generated program is allowed to do.
//!
//! Every check here is a real program the host has to refuse, or a real
//! boundary that has to hold. A source-string comparison proves nothing
//! about whether a program can reach something it was not granted, so
//! the cases are the ones that actually escape if a gate is missing:
//! dynamic evaluation, loading from outside the frozen dependency set,
//! a macro that smuggles a capability in through its expansion, a
//! program that never stops, and one that floods its output.

use std::path::{Path, PathBuf};
use std::time::Duration;

use grove::contracts::{Actor, ActorRole};
use grove::execution::{
    Capability, ExecutionLimits, ExecutionRequest, FrozenSource, GrantProfile, execute,
};
use grove::store::Store;

fn store(label: &str) -> (Store, PathBuf) {
    // Per-test, not per-process: two threads sharing a store root race
    // on the same database and one of them reads the other's events.
    let dir = std::env::temp_dir().join(format!("grove-exec-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Store::open(&dir).expect("open store");
    (store, dir)
}

/// The candidate profile G02 ships: no dynamic evaluation, and loads
/// limited to the frozen dependency set.
fn candidate_grant(frozen: &[FrozenSource]) -> GrantProfile {
    GrantProfile {
        capabilities: vec![
            Capability::Arithmetic,
            Capability::Collections,
            Capability::Output,
        ],
        frozen: frozen.to_vec(),
        allowed_dependencies: frozen.iter().map(|f| f.module.clone()).collect(),
        allow_dynamic_eval: false,
        limits: ExecutionLimits::default(),
    }
}

fn request(source: &str, grant: &GrantProfile) -> ExecutionRequest {
    ExecutionRequest {
        execution_id: "exec-1".into(),
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        epoch: 1,
        source: source.to_string(),
        grant: grant.clone(),
        entrypoint: "agent-entry".into(),
        inputs: Vec::new(),
        deadline: std::time::Instant::now() + Duration::from_secs(30),
    }
}

// ── the refusals ──────────────────────────────────────────────────

#[test]
fn dynamic_eval_is_refused_not_merely_unreached() {
    let (store, dir) = store("dynamic-eval");
    let grant = candidate_grant(&[]);
    // The call is real; only the permission is missing. If a gate were
    // missing this would evaluate and print.
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(r#"(eval (read-string "(println \"escaped\")"))"#, &grant),
    )
    .expect("run");
    assert_eq!(
        outcome.status,
        grove::execution::ExecutionStatus::Refused,
        "dynamic eval must be refused, got {:?}",
        outcome.status
    );
    assert!(
        outcome.error.contains("dynamic-eval"),
        "refusal must name the capability, got {:?}",
        outcome.error
    );
    assert!(
        !outcome.output.contains("escaped"),
        "refused program must not have run"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_outside_the_frozen_dependency_set_is_refused() {
    let (store, dir) = store("dyn-load");
    // A module that exists on disk and is readable by the process, but
    // not part of what the grant froze.
    let outside = dir.join("secret.zio");
    std::fs::write(&outside, "(println \"leaked\")\n").unwrap();
    let grant = candidate_grant(&[]);
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(&format!("(load \"{}\")", outside.display()), &grant),
    )
    .expect("run");
    assert_eq!(
        outcome.status,
        grove::execution::ExecutionStatus::Refused,
        "loading an undeclared module must be refused"
    );
    assert!(
        !outcome.output.contains("leaked"),
        "refused load must not have run the module"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_macro_cannot_smuggle_a_capability_through_its_expansion() {
    let (store, dir) = store("macro-smuggle");
    // The *call site* mentions no forbidden form. The macro body does,
    // and the check that matters runs on the expansion, not the source.
    let grant = candidate_grant(&[]);
    let source = r#"
(defmacro boom [] (list (quote eval) (list (quote read-string) "(println \"smuggled\")")))
(boom)
"#;
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(source, &grant),
    )
    .expect("run");
    assert_ne!(
        outcome.status,
        grove::execution::ExecutionStatus::Completed,
        "a macro expanding into dynamic eval must not complete"
    );
    assert!(
        !outcome.output.contains("smuggled"),
        "the smuggled program must not have run"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_non_terminating_program_is_stopped() {
    let (store, dir) = store("fuel");
    let mut grant = candidate_grant(&[]);
    grant.limits.max_steps = 50_000;
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request("(loop [i 0] (recur (+ i 1)))", &grant),
    )
    .expect("run");
    assert_eq!(
        outcome.status,
        grove::execution::ExecutionStatus::Failed,
        "an endless program must fail, not hang"
    );
    assert!(
        outcome.error.contains("step limit"),
        "failure must name the step limit, got {:?}",
        outcome.error
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_over_the_cap_is_capped_and_reported() {
    let (store, dir) = store("output-cap");
    let mut grant = candidate_grant(&[]);
    grant.limits.max_output_bytes = 1_024;
    // A program that prints far more than the cap allows.
    let source = "(defn agent-entry [] (loop [i 0] (if (>= i 5000) nil (do (println \"0123456789abcdef\") (recur (+ i 1))))))";
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(source, &grant),
    )
    .expect("run");
    assert!(
        outcome.output.len() <= 1_024,
        "host must not buffer past the cap, got {} bytes",
        outcome.output.len()
    );
    assert!(
        outcome.output_truncated,
        "a capped output must say so rather than look complete"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_that_raises_reports_the_error_and_claims_nothing() {
    let (store, dir) = store("raises");
    let grant = candidate_grant(&[]);
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(r#"(error "deliberate")"#, &grant),
    )
    .expect("run");
    assert_eq!(outcome.status, grove::execution::ExecutionStatus::Failed);
    assert!(outcome.result.is_none(), "a failure has no result value");
    assert!(outcome.error.contains("deliberate"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ── what a candidate may do ───────────────────────────────────────

#[test]
fn a_plain_transformation_completes_with_its_real_output() {
    let (store, dir) = store("transform");
    let grant = candidate_grant(&[]);
    let source = "(defn sum-list [xs] (loop [i 0 total 0] (if (>= i (count xs)) total (recur (+ i 1) (+ total (get xs i)))))) (defn agent-entry [] (println (sum-list [1 2 3 4])))";
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(source, &grant),
    )
    .expect("run");
    assert_eq!(
        outcome.status,
        grove::execution::ExecutionStatus::Completed,
        "a plain transformation must complete; error was {:?}",
        outcome.error
    );
    assert!(
        outcome.output.contains("10"),
        "the real computed value must be in the output, got {:?}",
        outcome.output
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_frozen_dependency_may_be_required() {
    let (store, dir) = store("frozen-dep");
    // A real module on disk, declared in the grant's frozen set.
    let lib_dir = dir.join("lib");
    std::fs::create_dir_all(&lib_dir).unwrap();
    std::fs::write(
        lib_dir.join("helper.zio"),
        "(export twice)\n(defn twice [x] (* x 2))\n",
    )
    .unwrap();
    let grant = candidate_grant(&[FrozenSource {
        module: "helper".into(),
        path: lib_dir.join("helper.zio"),
    }]);
    let mut req = request(
        "(require :helper) (defn agent-entry [] (println (twice 21)))",
        &grant,
    );
    req.inputs = vec![lib_dir.clone()];
    let outcome = execute(&store, &Actor::new("runner", ActorRole::Operator), req).expect("run");
    assert_eq!(
        outcome.status,
        grove::execution::ExecutionStatus::Completed,
        "a declared dependency must load; error was {:?}",
        outcome.error
    );
    assert!(outcome.output.contains("42"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_trace_survives_into_the_event_log() {
    let (store, dir) = store("trace");
    let grant = candidate_grant(&[]);
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(
            "(defn pick [x] (if (> x 0) (+ x 1) (error \"negative\")))\n(defn agent-entry [] (println (pick 2)))",
            &grant,
        ),
    )
    .expect("run");
    assert_eq!(outcome.status, grove::execution::ExecutionStatus::Completed);
    // What ran is recorded, bound to the run the host named. Read from
    // the start of the log: `read_events(store, run, 0)` is *exclusive*
    // of sequence 0, so it would quietly skip the first event.
    let events = grove::events::read_events_from_start(&store, "run-1").expect("read events");
    assert!(!events.is_empty(), "an executed program leaves a trace");
    assert!(
        events
            .iter()
            .any(|e| e.run_id == "run-1" && e.attempt_id == "attempt-1"),
        "every event must carry the host's run identity"
    );
    assert!(
        events.iter().any(|e| e.detail.contains("pick")),
        "the trace must name what was called, got {:?}",
        events.iter().map(|e| e.detail.clone()).collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ── identity ──────────────────────────────────────────────────────

#[test]
fn the_result_is_bound_to_the_request_that_produced_it() {
    let (store, dir) = store("identity");
    let grant = candidate_grant(&[]);
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request("(defn agent-entry [] (println \"identity\"))", &grant),
    )
    .expect("run");
    assert_eq!(outcome.execution_id, "exec-1");
    assert_eq!(outcome.run_id, "run-1");
    assert_eq!(outcome.attempt_id, "attempt-1");
    assert_eq!(outcome.epoch, 1);
    // The source digest is the *source that ran*, so a record cannot be
    // filed against code that was never executed.
    assert_eq!(
        outcome.source_digest,
        grove::contracts::digest_bytes(b"(defn agent-entry [] (println \"identity\"))")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unknown_dependency_is_refused_before_any_execution() {
    let (store, dir) = store("unknown-dep");
    let grant = candidate_grant(&[]);
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request("(require :not-in-the-grant)", &grant),
    )
    .expect("run");
    assert_eq!(outcome.status, grove::execution::ExecutionStatus::Refused);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_real_missing_path_is_an_error_not_a_panic() {
    let (store, dir) = store("missing-index");
    let grant = candidate_grant(&[]);
    // Nothing here should unwind the host: an execution path that panics
    // takes the service down with it.
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request("(get [1 2] 99)", &grant),
    )
    .expect("run");
    assert_eq!(outcome.status, grove::execution::ExecutionStatus::Failed);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The frozen set is what the *host* declared. A program cannot widen
/// it, and a program that reads a path it was not given is refused by
/// the I/O host, not by a filter over its text.
#[test]
fn the_sandbox_has_no_ambient_filesystem() {
    let (store, dir) = store("no-fs");
    let grant = candidate_grant(&[]);
    let canary = dir.join("canary.txt");
    std::fs::write(&canary, "trusted").unwrap();
    let outcome = execute(
        &store,
        &Actor::new("runner", ActorRole::Operator),
        request(
            &format!("(println (slurp \"{}\"))", canary.display()),
            &grant,
        ),
    )
    .expect("run");
    assert_ne!(
        outcome.status,
        grove::execution::ExecutionStatus::Completed,
        "reading outside the granted inputs must not succeed"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn _unused(_: &Path) {}
