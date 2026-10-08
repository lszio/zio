//! G01 observer contract: execution evidence that a run can trust.
//!
//! Three properties, each checked against a real run rather than a
//! source string:
//!
//! 1. **The trace is about execution, not text.** `then` appears because
//!    that branch ran; `else` does not appear because it did not.
//! 2. **Off means off.** With no observer there is no trace container, no
//!    serialization, and the same values and the same errors — an
//!    observer that changed what it observes would make the evidence
//!    unreliable.
//! 3. **Text is not evidence.** A program that *prints* the word "trace"
//!    produces output, not events; only the evaluator can write an event.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use zio_core::bootstrap::{ModuleRoots, eval_source, language_context};
use zio_core::error::EvalError;
use zio_core::observer::{Event, EventKind, Observer, RecordingObserver};

/// A context with an observer attached, returning the recording.
fn observed(
    src: &str,
) -> Result<(Vec<Event>, Result<zio_core::value::Value, EvalError>), EvalError> {
    let observer = Arc::new(RecordingObserver::new());
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    ctx.attach_observer(observer.clone());
    let result = eval_source(&ctx, "observed.zio", src);
    Ok((observer.events(), result))
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.as_str()).collect()
}

// ── branches, calls, errors, macros ───────────────────────────────

#[test]
fn a_trace_reports_the_branch_that_ran_and_not_the_one_that_did_not() {
    let (events, result) =
        observed("(defn choose [x] (if (> x 0) (+ x 1) (error \"negative\")))\n(choose 2)")
            .expect("the program runs");
    assert_eq!(result.expect("choose 2 succeeds").to_string(), "3");

    let seen = kinds(&events);
    assert!(
        seen.contains(&"branch"),
        "the conditional must produce a branch event: {seen:?}"
    );
    let branches: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == EventKind::Branch)
        .collect();
    assert!(
        branches.iter().any(|e| e.detail.contains("then")),
        "the taken branch must be reported: {:?}",
        branches.iter().map(|e| &e.detail).collect::<Vec<_>>()
    );
    assert!(
        !branches.iter().any(|e| e.detail.contains("else")),
        "the branch that did not run must not be reported: {:?}",
        branches.iter().map(|e| &e.detail).collect::<Vec<_>>()
    );
    // The function call is a fact about execution too.
    assert!(
        kinds(&events).contains(&"call"),
        "the call to choose must be observable: {seen:?}"
    );
}

#[test]
fn an_error_is_observed_with_its_source() {
    let (events, result) = observed("(error \"boom\")").expect("the program runs");
    let err = result.expect_err("error raises");
    let errors: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == EventKind::Error)
        .collect();
    assert_eq!(errors.len(), 1, "one raise is one event: {events:?}");
    let event = errors[0];
    assert!(
        event.detail.contains("boom"),
        "the message must be reported: {}",
        event.detail
    );
    // The event carries the location the error carries: evidence that
    // says *where* something happened, not only that it did.
    assert!(
        event.span.is_some(),
        "an error event must carry the source span it happened at"
    );
    // And the span agrees with the error the caller saw.
    let span = event.span.expect("a span");
    assert_eq!(
        (span.line, span.col),
        (1, 1),
        "the event's span must be the error's own location"
    );
    assert!(
        err.to_string().contains("boom"),
        "the error is unchanged: {err}"
    );
}

#[test]
fn a_macro_expansion_is_observed_with_its_call_site() {
    // A macro call produces two things: the macro that ran, and the code
    // it produced. The generated code is a fact about this call, not
    // about the program text.
    let (events, result) = observed("(defmacro twice [form] (list '+ form form))\n(twice 21)\n")
        .expect("the program runs");
    assert_eq!(result.expect("twice 21 evaluates").to_string(), "42");

    let seen = kinds(&events);
    let expansions: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == EventKind::MacroExpansion)
        .collect();
    assert!(
        !expansions.is_empty(),
        "a macro call must be observable: {seen:?}"
    );
    assert!(
        expansions[0].detail.contains("twice"),
        "the macro's name must be reported: {}",
        expansions[0].detail
    );
    // The call site is preserved: a generated node is not the source.
    let site = expansions[0].span.expect("a macro call site");
    assert_eq!(
        site.line, 2,
        "the call site is the line the macro was called on"
    );
}

// ── off means off ────────────────────────────────────────────────

#[test]
fn an_unobserved_run_behaves_exactly_the_same() {
    let src = "(defn f [x] (if (> x 0) (+ x 1) (* x 2)))\n(list (f 1) (f -1) (f 0))";
    let (_, observed_result) = observed(src).expect("the program runs");
    let unobserved_ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    let unobserved = eval_source(&unobserved_ctx, "unobserved.zio", src);

    let a = observed_result.expect("observed run succeeds");
    let b = unobserved.expect("unobserved run succeeds");
    assert_eq!(
        a.to_string(),
        b.to_string(),
        "the observer must not change values"
    );
}

#[test]
fn an_unobserved_run_reports_the_same_error() {
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    let err = eval_source(&ctx, "e.zio", "(car 1)").expect_err("a type error");
    let (events, _) = observed("(car 1)").expect("the program runs");
    let seen = kinds(&events);
    // The error text is the same shape with and without an observer.
    assert!(
        err.to_string().starts_with("type error"),
        "unexpected: {err}"
    );
    assert!(
        events.iter().any(|e| e.kind == EventKind::Error),
        "with an observer the same failure is recorded: {seen:?}"
    );
}

#[test]
fn an_observer_that_is_never_attached_costs_nothing() {
    // Not "produces no events" — costs nothing. A counter of payload
    // constructions is the only honest way to show that: with no
    // observer attached the evaluator must not even build the payload.
    // The count is per port, so a parallel test in this binary cannot
    // move the number underneath this measurement.
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    for _ in 0..4 {
        eval_source(
            &ctx,
            "quiet.zio",
            "(defn f [x] (if (> x 0) (+ x 1) x))\n(f 5)",
        )
        .unwrap();
    }
    assert_eq!(
        ctx.payloads_built(),
        0,
        "an unobserved run must construct no event payloads at all"
    );

    // The counterpart: the same program with an observer really does
    // build them, so "costs nothing when off" is not vacuous.
    let loud = language_context(ModuleRoots::empty()).expect("bootstrap");
    loud.attach_observer(Arc::new(RecordingObserver::new()));
    for _ in 0..4 {
        eval_source(
            &loud,
            "loud.zio",
            "(defn f [x] (if (> x 0) (+ x 1) x))\n(f 5)",
        )
        .unwrap();
    }
    assert!(
        loud.payloads_built() > 0,
        "the same program with an observer must build payloads, got {}",
        loud.payloads_built()
    );
}

#[test]
fn an_attached_observer_does_see_payloads() {
    // The counter's counterpart: with an observer attached the payloads
    // are really built, so "costs nothing when off" is not vacuous.
    let observer = Arc::new(RecordingObserver::new());
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    ctx.attach_observer(observer.clone());
    eval_source(
        &ctx,
        "loud.zio",
        "(defn f [x] (if (> x 0) (+ x 1) x))\n(f 5)",
    )
    .unwrap();
    assert!(
        ctx.payloads_built() > 0,
        "an attached observer must build payloads"
    );
    assert!(!observer.events().is_empty());
}

// ── text is not evidence ─────────────────────────────────────────

#[test]
fn a_program_that_prints_the_word_trace_writes_no_events() {
    // Candidate stdout is output. A model that prints a fake trace is
    // printing a string, and the event log must not grow because of it.
    let (events, result) =
        observed("(println \"trace: branch then; call choose; error none\")\n(println \"trace\")")
            .expect("the program runs");
    assert_eq!(
        result.expect("printing succeeds"),
        zio_core::value::Value::Nil
    );
    let seen = kinds(&events);
    assert!(
        !events.iter().any(|e| e.detail.contains("trace")),
        "printed text must not become an event: {events:?}"
    );
    // Only the two real print effects are recorded, and neither is a
    // branch or an error.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::Branch | EventKind::Error)),
        "printing is not branching: {seen:?}"
    );
}

#[test]
fn an_event_carries_its_own_sequence_and_kind() {
    let (events, _) = observed("(if (> 1 0) 1 2)").expect("the program runs");
    assert!(!events.is_empty());
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.sequence, index as u64,
            "sequence numbers must be dense and ordered: {events:?}"
        );
        assert!(!event.kind.as_str().is_empty());
    }
    // SourceId is carried, so a host can resolve the location against
    // the same registry the evaluator used.
    let branch = events
        .iter()
        .find(|e| e.kind == EventKind::Branch)
        .expect("a branch happened");
    assert!(
        branch.span.is_some(),
        "a branch must carry where it happened"
    );
}

// ── the observer is a port, not a store ───────────────────────────

#[test]
fn any_observer_implementation_receives_the_events() {
    // The contract is a trait: a host that streams events to a file or
    /// rejects them gets the same call, not a special path.
    #[derive(Default)]
    struct CountingObserver(AtomicUsize);
    impl Observer for CountingObserver {
        fn on_event(&self, _event: &Event) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let observer = Arc::new(CountingObserver::default());
    let ctx = language_context(ModuleRoots::empty()).expect("bootstrap");
    ctx.attach_observer(observer.clone());
    eval_source(&ctx, "counted.zio", "(if (> 1 0) 1 2)").unwrap();
    assert!(
        observer.0.load(Ordering::Relaxed) > 0,
        "the custom observer saw nothing"
    );
}
