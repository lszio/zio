//! G01 events contract: execution events as a durable, cursor-readable
//! fact.
//!
//! A trace that lives only in memory is a log line. What makes it
//! evidence is that it is ordered, survives a restart, is addressed by a
//! cursor, and cannot be edited or inserted out of order afterwards.

use std::sync::Arc;

use grove::contracts::{Actor, ActorRole, ErrorKind, SCHEMA_VERSION};
use grove::events::{
    EventLog, EventSourceStub, ExecutionEvent, MAX_TRACE_BYTES, append_events, read_events,
    read_events_from_start,
};
use grove::store::Store;

fn recorder() -> Actor {
    Actor::new("runner", ActorRole::Operator)
}

fn store(name: &str) -> Store {
    let root = std::env::temp_dir().join(format!("grove-events-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    Store::open(&root).expect("store opens")
}

fn event(run: &str, kind: &str, detail: &str, seq: u64) -> ExecutionEvent {
    ExecutionEvent {
        schema: SCHEMA_VERSION,
        sequence: seq,
        run_id: run.to_string(),
        attempt_id: format!("{run}-att"),
        kind: kind.to_string(),
        detail: detail.to_string(),
        source: Some(EventSourceStub {
            source_id: 3,
            name: "prog.zio".into(),
            line: 2,
            col: 5,
        }),
        at_ms: 1_700_000_000_000 + seq as i64,
    }
}

// ── ordering and cursor ───────────────────────────────────────────

#[test]
fn events_are_read_in_sequence_from_a_cursor() {
    let store = store("cursor");
    let events: Vec<ExecutionEvent> = (0..5)
        .map(|i| event("run-1", "call", &format!("f{i}"), i))
        .collect();
    append_events(&store, &recorder(), &events).expect("append");

    // A cursor is exclusive, so the whole log is "before the first
    // sequence" — which is a cursor the caller cannot express as a u64.
    // Read from the beginning with a dedicated call, then check that the
    // cursor does what a cursor is for.
    let all = read_events_from_start(&store, "run-1").expect("read");
    assert_eq!(all.len(), 5);
    assert_eq!(
        all.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4],
        "a log read in full must be in order"
    );

    // A reader that has processed up to and including N gets exactly
    // what came after — never a re-read, never a gap.
    let after_two = read_events(&store, "run-1", 2).expect("read after cursor");
    assert_eq!(
        after_two.iter().map(|e| e.sequence).collect::<Vec<_>>(),
        vec![3, 4],
        "a cursor means strictly after the given sequence"
    );
    // A cursor at the last sequence means "I have it": nothing follows.
    let tail = read_events(&store, "run-1", 4).expect("read tail");
    assert!(
        tail.is_empty(),
        "a reader holding the last event needs nothing more: {tail:?}"
    );
    // One earlier, it gets exactly that one.
    let last = read_events(&store, "run-1", 3).expect("read last");
    assert_eq!(last.len(), 1);
    assert_eq!(last[0].sequence, 4);
    assert!(
        read_events(&store, "run-1", 99)
            .expect("past the end")
            .is_empty()
    );
}

#[test]
fn a_second_append_continues_the_sequence_rather_than_restarting_it() {
    let store = store("continue");
    append_events(&store, &recorder(), &[event("run-1", "call", "a", 0)]).expect("first");
    append_events(&store, &recorder(), &[event("run-1", "call", "b", 1)]).expect("second");
    let all = read_events_from_start(&store, "run-1").expect("read");
    assert_eq!(all.len(), 2);
    assert_eq!(
        all[1].detail, "b",
        "the second append must not be lost or reordered"
    );
}

// ── durability ────────────────────────────────────────────────────

#[test]
fn events_survive_reopening_the_store() {
    let root = std::env::temp_dir().join("grove-events-restart");
    let _ = std::fs::remove_dir_all(&root);
    {
        let store = Store::open(&root).expect("open");
        append_events(
            &store,
            &recorder(),
            &[
                event("run-1", "branch", "then", 0),
                event("run-1", "error", "boom", 1),
            ],
        )
        .expect("append");
    }
    // A second process would see exactly this.
    let reopened = Store::open(&root).expect("reopen");
    let all = read_events_from_start(&reopened, "run-1").expect("read after restart");
    assert_eq!(all.len(), 2, "events must survive a restart");
    assert_eq!(all[0].kind, "branch");
    assert_eq!(all[1].detail, "boom");
    // The timestamps and sources are the ones that were recorded, not
    // ones recomputed on read.
    assert_eq!(all[1].at_ms, 1_700_000_000_001);
    let source = all[0].source.as_ref().expect("the event knows its source");
    assert_eq!(source.name, "prog.zio");
    assert_eq!((source.line, source.col), (2, 5));
}

// ── authority ─────────────────────────────────────────────────────

#[test]
fn a_reader_cannot_append_events() {
    let store = store("role");
    let reader = Actor::new("someone", ActorRole::Reader);
    let err = append_events(&store, &reader, &[event("run-1", "call", "x", 0)])
        .expect_err("only the runner writes the log");
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
    assert!(
        read_events_from_start(&store, "run-1")
            .expect("read")
            .is_empty()
    );
}

#[test]
fn a_log_is_append_only() {
    let store = store("append_only");
    append_events(&store, &recorder(), &[event("run-1", "call", "first", 0)]).expect("append");
    // Re-appending the same sequence is refused rather than silently
    // overwriting: two claims about what happened at one point in a run
    // is a contradiction, not an update.
    let err = append_events(
        &store,
        &recorder(),
        &[event("run-1", "call", "rewritten", 0)],
    )
    .expect_err("a sequence number may be written once");
    assert_eq!(err.kind, ErrorKind::Conflict);
    let all = read_events_from_start(&store, "run-1").expect("read");
    assert_eq!(all.len(), 1, "the original event must survive the attempt");
    assert_eq!(all[0].detail, "first");
}

#[test]
fn a_gap_is_fine_but_a_reused_number_is_not() {
    let store = store("order");
    // A gap is legal: the log is ordered, not dense. A step that was
    // never observed leaves a hole, and inventing an event to fill it
    // would be a fabrication.
    append_events(
        &store,
        &recorder(),
        &[
            event("run-1", "call", "a", 0),
            event("run-1", "call", "c", 2),
        ],
    )
    .expect("a gap is not a conflict");
    append_events(&store, &recorder(), &[event("run-1", "call", "b", 1)])
        .expect("filling the gap later is still an append, not a rewrite");

    let all = read_events_from_start(&store, "run-1").expect("read");
    assert_eq!(
        all.iter()
            .map(|e| (e.sequence, e.detail.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, "a"), (1, "b"), (2, "c")],
        "the log reads in sequence order whatever order it was written"
    );

    // What is refused is a *second claim* on a number already used.
    let err = append_events(&store, &recorder(), &[event("run-1", "call", "a-again", 1)])
        .expect_err("sequence 1 has already been claimed");
    assert_eq!(err.kind, ErrorKind::Conflict);
    let after = read_events_from_start(&store, "run-1").expect("read");
    assert_eq!(after.len(), 3, "the refused append left the log untouched");
}

// ── bounded and honest ────────────────────────────────────────────

#[test]
fn an_oversized_trace_is_refused_rather_than_truncated_silently() {
    let store = store("cap");
    let mut big = event("run-1", "error", "x", 0);
    big.detail = "x".repeat(MAX_TRACE_BYTES + 1);
    let err = append_events(&store, &recorder(), &[big])
        .expect_err("a payload over the cap must not be stored");
    assert!(
        err.to_string().contains("cap") || err.to_string().contains("bytes"),
        "the error must name the cap: {err}"
    );
    // Nothing partial was written.
    assert!(
        read_events_from_start(&store, "run-1")
            .expect("read")
            .is_empty()
    );
}

#[test]
fn a_batch_is_all_or_nothing() {
    let store = store("atomic");
    let mut batch: Vec<ExecutionEvent> = (0..3)
        .map(|i| event("run-1", "call", &format!("e{i}"), i))
        .collect();
    batch[2].detail = "x".repeat(MAX_TRACE_BYTES + 1);
    let err = append_events(&store, &recorder(), &batch).expect_err("the batch is refused");
    assert!(err.to_string().contains("cap") || err.to_string().contains("bytes"));
    assert!(
        read_events_from_start(&store, "run-1")
            .expect("read")
            .is_empty(),
        "a refused batch must leave nothing behind: a partial trace is a lie about coverage"
    );
}

// ── a real run, not a fixture ─────────────────────────────────────

#[test]
fn a_real_program_produces_a_trace_the_store_can_serve() {
    // The whole point of the log: events from a real evaluator, in the
    // order they happened, with the source they happened at.
    let store = store("real");
    let observer = Arc::new(grove::events::StoreObserver::new());
    let ctx = zio_core::bootstrap::language_context(zio_core::bootstrap::ModuleRoots::empty())
        .expect("bootstrap");
    ctx.attach_observer(observer.clone());
    let result = zio_core::bootstrap::eval_source(
        &ctx,
        "prog.zio",
        "(defn choose [x] (if (> x 0) (+ x 1) (error \"negative\")))\n(choose 2)",
    );
    assert_eq!(result.expect("choose 2 succeeds").to_string(), "3");

    // The trusted host adds the run identity it owns; the evaluator only
    // knows about semantics.
    let collected = observer.collected();
    let wrapped: Vec<ExecutionEvent> = collected
        .into_iter()
        .enumerate()
        .map(|(seq, e)| {
            let mut out = e;
            out.schema = SCHEMA_VERSION;
            out.run_id = "run-real".into();
            out.attempt_id = "att-real".into();
            out.at_ms = 1_700_000_000_000 + seq as i64;
            out.sequence = seq as u64;
            out
        })
        .collect();
    append_events(&store, &recorder(), &wrapped).expect("append");

    let served = read_events(&store, "run-real", 0).expect("read");
    let kinds: Vec<&str> = served.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"branch"), "the taken branch: {kinds:?}");
    assert!(kinds.contains(&"call"), "the call to choose: {kinds:?}");

    // The branch event names the arm that ran, and points at the source.
    let branch = served
        .iter()
        .find(|e| e.kind.as_str() == "branch")
        .expect("a branch");
    assert_eq!(branch.detail, "then", "the taken arm is named");
    let where_ = branch
        .source
        .as_ref()
        .expect("the event knows where it happened");
    assert!(where_.line >= 1, "the event knows where it happened");

    // And no event claims the arm that did not run.
    assert!(
        !served.iter().any(|e| e.detail == "else"),
        "a branch that did not run is not evidence: {served:?}"
    );

    // The cursor serves the tail of the same run.
    let last = served.last().expect("events exist").sequence;
    assert!(
        read_events(&store, "run-real", last)
            .expect("tail")
            .is_empty()
    );
}

// ── the log is not a second fact store ────────────────────────────

#[test]
fn events_carry_no_authority_they_never_had() {
    let store = store("no_authority");
    append_events(
        &store,
        &recorder(),
        &[event("run-1", "call", "approve candidate", 0)],
    )
    .expect("append");
    let all = read_events_from_start(&store, "run-1").expect("read");
    // An event saying "approve" is a *record that a call named approve
    // happened*. It is not an approval: publishing still goes through
    // the human boundary, which is a different table and a different
    // role.
    assert_eq!(all[0].detail, "approve candidate");
    assert_eq!(all[0].kind, "call");
    // Nothing in the events API can publish.
    let log = EventLog::for_run("run-1");
    assert_eq!(log.run_id, "run-1");
}
