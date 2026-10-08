//! G03 runner contract: the queue actually runs work, and the ledger
//! only accepts what the current owner may say.
//!
//! Four properties, each checked against a real store rather than a
//! source string:
//!
//! 1. **Transitions are validated.** `set_run_state` today accepts any
//!    pair, so a run can jump Queued → Accepted. A state machine that
//!    does not exist is not a state machine.
//! 2. **A claim is one-winner and epoch-stamped.** Two owners racing for
//!    the same queued run: exactly one gets it, and the loser learns it
//!    lost rather than running the same work twice.
//! 3. **A superseded owner cannot land a result.** The epoch a claim was
//!    stamped with must be checked at commit, or a zombie receipt from
//!    before a restart overwrites newer progress.
//! 4. **Budget moves once.** Reserve in the same transaction as the
//!    claim; a replayed receipt must not double-spend, and a run that
//!    asks past its grant is refused rather than truncated.
//!
//! The execution itself is covered by G02's `execution_contract`; this
//! file is about the *queue* around it.

use std::path::PathBuf;
use std::sync::Arc;

use grove::contracts::{Actor, ActorRole, ArtifactRef, ErrorKind, Run, RunState, SCHEMA_VERSION};
use grove::store::Store;
use grove_app::runner::{RunClaim, RunRequest, Runner, RunnerConfig, claim_next, transition};

fn store(label: &str) -> (Arc<Store>, PathBuf) {
    let dir = std::env::temp_dir().join(format!("grove-runner-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(Store::open(&dir).expect("open store"));
    (store, dir)
}

fn operator() -> Actor {
    Actor::new("runner", ActorRole::Operator)
}

/// A run record with a real base snapshot digest. The runner never
/// reads the snapshot, so the digest is only here to make the record
/// structurally valid.
fn run(id: &str, budget: u32) -> Run {
    Run {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        task_id: "task-1".into(),
        base_snapshot: ArtifactRef::parse_hex(
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap(),
        dataset_revision: "ds-1".into(),
        recipe: "recipe-1".into(),
        state: RunState::Queued,
        steps_consumed: 0,
        steps_budget: budget,
        resumed_from: None,
    }
}

// ── 1. the transition rule exists ─────────────────────────────────

#[test]
fn a_run_cannot_jump_from_queued_to_accepted() {
    let (store, dir) = store("transition-jump");
    // The machine the plan requires, spelled out as a table so a
    // permissive branch has to be deliberate.
    let legal: &[(RunState, RunState)] = &[
        (RunState::Queued, RunState::Running),
        (RunState::Queued, RunState::Cancelled),
        (RunState::Queued, RunState::Failed),
        (RunState::Running, RunState::Evaluating),
        (RunState::Running, RunState::Paused),
        (RunState::Running, RunState::Failed),
        (RunState::Running, RunState::Cancelled),
        (RunState::Evaluating, RunState::Accepted),
        (RunState::Evaluating, RunState::Rejected),
        (RunState::Evaluating, RunState::Failed),
        (RunState::Evaluating, RunState::Cancelled),
    ];
    for (from, to) in legal {
        assert!(
            transition(*from, *to).is_ok(),
            "{from:?} -> {to:?} must be allowed by the plan"
        );
    }
    // And the edges that must not exist. Queued -> Accepted is the one
    // that would let a run look finished without ever running.
    for (from, to) in [
        (RunState::Queued, RunState::Accepted),
        (RunState::Queued, RunState::Evaluating),
        (RunState::Queued, RunState::Paused),
        (RunState::Running, RunState::Accepted),
        (RunState::Accepted, RunState::Running),
        (RunState::Paused, RunState::Running),
        (RunState::Failed, RunState::Running),
    ] {
        let err = transition(from, to).expect_err("illegal transition");
        assert_eq!(
            err.kind,
            ErrorKind::InvalidInput,
            "{from:?} -> {to:?} must be refused as a malformed request"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn an_illegal_transition_is_refused_by_the_store_not_just_the_helper() {
    let (store, dir) = store("transition-store");
    let mut queued = run("run-1", 10);
    store.put_run(&operator(), &queued).expect("put");
    // The run has not run, so it cannot be accepted. Writing that state
    // through the store directly is the hole this closes.
    let err = grove_app::runner::set_run_state_checked(
        &store,
        &operator(),
        &mut queued,
        RunState::Accepted,
    )
    .expect_err("queued -> accepted must be refused");
    assert_eq!(err.kind, ErrorKind::InvalidInput);
    // The record is unchanged: a refused write is not a partial write.
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Queued);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

// ── 2. claiming is one-winner ─────────────────────────────────────

#[test]
fn only_one_owner_claims_a_queued_run() {
    let (store, dir) = store("claim");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");

    let first = claim_next(&runner, &operator(), "attempt-a")
        .expect("first claim")
        .expect("the queued run is claimable");
    assert_eq!(first.run_id, "run-1");
    assert!(first.epoch > 0, "a claim is stamped with the owner epoch");

    // A second owner asking for the same work must be told there is
    // none, not handed the same run. Running it twice is how one
    // training run becomes two.
    let second = claim_next(&runner, &operator(), "attempt-b").expect("second claim");
    assert!(
        second.is_none(),
        "a run that is already claimed must not be claimable again, got {second:?}"
    );
    // And the state really moved.
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Running);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_claim_reserves_the_budget_in_the_same_breath() {
    let (store, dir) = store("claim-budget");
    store.put_run(&operator(), &run("run-1", 40)).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let claim = claim_next(&runner, &operator(), "attempt-a")
        .expect("claim")
        .expect("the queued run is claimable");
    // The reservation is visible immediately: a crash between claim and
    // execution must not hand the same steps out again.
    assert_eq!(
        store.get_run("run-1").unwrap().steps_consumed,
        claim.reserved_steps,
        "a claim must move the ledger, not just flip a state"
    );
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_run_whose_grant_is_exhausted_is_refused_not_truncated() {
    let (store, dir) = store("claim-exhausted");
    let mut over = run("run-1", 10);
    over.steps_consumed = 10; // already spent its whole grant
    store.put_run(&operator(), &over).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let err = claim_next(&runner, &operator(), "attempt-a").expect_err("exhausted grant");
    assert_eq!(err.kind, ErrorKind::BudgetExhausted);
    // Refused means refused: the run stays queued for a decision.
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Queued);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

// ── 3. a superseded owner cannot land a result ────────────────────

#[test]
fn a_result_stamped_with_a_superseded_epoch_is_refused() {
    let (store, dir) = store("epoch");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    let first = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let claim = claim_next(&first, &operator(), "attempt-a")
        .expect("claim")
        .expect("the queued run is claimable");

    // A restart: a new owner claims, which bumps the epoch and reclaims
    // the lease the first one held.
    let second = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("restart");
    assert!(
        second.epoch() > claim.epoch,
        "a restart must be a new epoch: {} then {}",
        claim.epoch,
        second.epoch()
    );

    // The first owner's result now lands. It must not.
    let err = first
        .complete(&operator(), &claim, "done")
        .expect_err("a superseded owner's result must not land");
    assert_eq!(err.kind, ErrorKind::Conflict);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_claim_can_only_be_completed_once() {
    let (store, dir) = store("double-complete");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let claim = claim_next(&runner, &operator(), "attempt-a")
        .expect("claim")
        .expect("the queued run is claimable");
    runner
        .complete(&operator(), &claim, "done")
        .expect("first completion");
    // A retried receipt for the same attempt is a replay, not progress.
    let err = runner
        .complete(&operator(), &claim, "done again")
        .expect_err("an attempt completes once");
    assert_eq!(err.kind, ErrorKind::Conflict);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

// ── 4. the queue is real work, not a state label ──────────────────

#[test]
fn the_owner_runs_a_queued_run_to_a_real_outcome() {
    let (store, dir) = store("run-to-outcome");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    // The work here is a real Zio transformation through the G02
    // execution path: the point is that the queue moved the run to
    // Running and then to a real outcome, not that a label changed.
    let request = RunRequest {
        run_id: "run-1".into(),
        kind: WorkKind::Zio {
            source: "(defn agent-entry [] (println \"queued-work-ran\"))".into(),
        },
    };
    let outcome = runner
        .drive(&operator(), &request)
        .expect("drive the queued run");
    assert_eq!(outcome.status, "completed", "error was {:?}", outcome.error);
    assert!(
        outcome.output.contains("queued-work-ran"),
        "the real program output must be what the run reports, got {:?}",
        outcome.output
    );
    // The trace is durable, not asserted in prose.
    let events = grove::events::read_events_from_start(&store, "run-1").expect("events");
    assert!(!events.is_empty(), "what ran is recorded");
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_failing_program_ends_the_run_failed_and_says_why() {
    let (store, dir) = store("run-fails");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let outcome = runner
        .drive(
            &operator(),
            &RunRequest {
                run_id: "run-1".into(),
                kind: WorkKind::Zio {
                    source: "(defn agent-entry [] (error \"the work failed\"))".into(),
                },
            },
        )
        .expect("drive");
    assert_eq!(outcome.status, "failed");
    assert!(outcome.error.contains("the work failed"));
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Failed);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_cancelled_run_is_not_picked_up() {
    let (store, dir) = store("cancelled");
    let mut queued = run("run-1", 100);
    store.put_run(&operator(), &queued).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    // Cancellation is an intent recorded before the owner looks, so a
    // run cancelled while queued is never started.
    runner.cancel(&operator(), "run-1").expect("cancel");
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Cancelled);
    let claim = claim_next(&runner, &operator(), "attempt-a").expect("claim");
    assert!(
        claim.is_none(),
        "a cancelled run must not be claimable, got {claim:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_zombie_receipt_from_the_previous_owner_cannot_write() {
    let (store, dir) = store("zombie");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    // The old owner claims and starts work.
    let old = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("old owner");
    let claim = claim_next(&old, &operator(), "attempt-old")
        .expect("claim")
        .expect("claimable");

    // The process dies and a new owner takes over. This is what a
    // `kill -9` of the service looks like from the ledger's side.
    let new = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("new owner");
    assert!(new.epoch() > claim.epoch, "a restart is a new epoch");

    // The zombie reports success. Refused, twice over: the epoch moved
    // and the lease was reclaimed.
    for attempt in [0, 1] {
        let err = old
            .complete(&operator(), &claim, "zombie result")
            .expect_err("a superseded owner's result must not land");
        assert_eq!(
            err.kind,
            ErrorKind::Conflict,
            "attempt {attempt}: a zombie receipt is a conflict, not a result"
        );
    }
    // And the run is still Running, waiting for whoever owns it now —
    // not silently marked finished by a process that died.
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Running);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

// ── the claim record ──────────────────────────────────────────────

#[test]
fn a_claim_names_the_work_it_took_and_the_epoch_it_was_stamped_with() {
    let (store, dir) = store("claim-shape");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let claim: RunClaim = claim_next(&runner, &operator(), "attempt-1")
        .expect("claim")
        .expect("the queued run is claimable");
    assert_eq!(claim.run_id, "run-1");
    assert_eq!(claim.attempt_id, "attempt-1");
    assert_eq!(claim.epoch, runner.epoch());
    assert!(claim.reserved_steps > 0, "a claim reserves something");
    assert!(claim.lease_expires_ms > 0, "a claim carries a lease");
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_resumed_run_is_actually_executed_rather_than_left_queued() {
    let Some(env) = training_env() else {
        eprintln!("SKIP: torch worker or namespaces unavailable; resumed execution unverified");
        return;
    };
    let (store, dir) = store("resume-executes");
    store
        .put_run(&operator(), &run("run-resumed", 100))
        .expect("put");
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf();
    let data = root.join("examples/self-learning/data");
    let runner = Runner::with_training(Arc::clone(&store), RunnerConfig::default(), Some(env))
        .expect("runner");

    // The work descriptor a resume attaches. Without it the run would
    // sit in `queued` forever and the owner would skip it silently.
    // Seed weights are a JSON parameter file, not the binary dataset:
    // the worker parses them, and a binary seed fails the parse rather
    // than training.
    let seed = dir.join("seed.json");
    std::fs::write(&seed, "{}").expect("seed weights");
    let run = store.get_run("run-resumed").expect("run");
    runner
        .enqueue(
            &operator(),
            &run,
            &RunRequest {
                run_id: "run-resumed".into(),
                kind: WorkKind::Training {
                    spec: grove_app::runner::TrainingSpec {
                        graph: serde_json::json!({
                            "inputs": {
                                "image": {"shape": [256], "dtype": "float32", "space": "pixel"},
                                "numeric": {"shape": [2], "dtype": "float32", "space": "signed"}
                            },
                            "ops": [
                                {"kind": "normalize", "inputs": ["image"], "output": "image_n", "attrs": {}},
                                {"kind": "normalize", "inputs": ["numeric"], "output": "numeric_n", "attrs": {}},
                                {"kind": "concat", "inputs": ["image_n", "numeric_n"], "output": "fused",
                                 "attrs": {"spaces": ["pixel", "signed"]}},
                                {"kind": "linear", "inputs": ["fused"], "output": "h0", "attrs": {}}
                            ],
                            "outputs": {"logits": "h0"},
                            "trainable": ["h0"]
                        }),
                        weights: seed.clone(),
                        data: data.join("train.bin"),
                        val_data: Some(data.join("val.bin")),
                        steps: 2,
                        seed: 7,
                        save_at: None,
                        resume: None,
                    },
                },
            },
        )
        .expect("enqueue carries the work");

    // The owner finds it, claims it, and runs it.
    let outcome = runner
        .tick(&operator(), "resume")
        .expect("tick")
        .expect("a queued run is waiting");
    assert_eq!(
        outcome.status, "completed",
        "the resumed run must actually execute, error was {:?}",
        outcome.error
    );
    assert_eq!(
        store.get_run("run-resumed").unwrap().state,
        RunState::Evaluating
    );
    // And the work descriptor is what made it runnable: an owner that
    // did not receive the request can still read what to build.
    let (kind, _) = store
        .queued_work("run-resumed")
        .unwrap()
        .expect("the run carries its work");
    assert_eq!(kind, "training");
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

// ── CPU training through the queue ────────────────────────────────

/// A training run needs a real torch worker. Skip is a reported missing
/// prerequisite, never a pass: the CPU path is the one that must
/// actually train.
fn training_env() -> Option<grove_app::runner::TrainingEnvironment> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf();
    let environment = grove_app::runner::TrainingEnvironment::probe(&root)?;
    grove::worker::Isolation::probe("unshare")?
        .namespaces
        .then_some(environment)
}

#[test]
fn a_training_run_without_a_worker_is_refused_not_reported_done() {
    let (store, dir) = store("training-unprovisioned");
    store.put_run(&operator(), &run("run-1", 100)).expect("put");
    // Deliberately no training environment.
    let runner = Runner::new(Arc::clone(&store), RunnerConfig::default()).expect("runner");
    let outcome = runner
        .drive(
            &operator(),
            &RunRequest {
                run_id: "run-1".into(),
                kind: WorkKind::Training {
                    spec: grove_app::runner::TrainingSpec {
                        graph: serde_json::json!({}),
                        weights: std::path::PathBuf::from("/nonexistent"),
                        data: std::path::PathBuf::from("/nonexistent"),
                        val_data: None,
                        steps: 1,
                        seed: 0,
                        save_at: None,
                        resume: None,
                    },
                },
            },
        )
        .expect("the refusal is a result, not a crash");
    // The run fails, and says why. Reporting `completed` for work that
    // never ran is the failure mode this whole package exists to stop.
    assert_eq!(outcome.status, "failed");
    assert!(
        outcome.error.contains("torch") || outcome.error.contains("training run"),
        "the refusal must name the missing prerequisite, got {:?}",
        outcome.error
    );
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

#[test]
fn a_real_cpu_training_run_actually_trains_and_logs_its_progress() {
    let Some(env) = training_env() else {
        eprintln!("SKIP: torch worker or namespaces unavailable; CPU training unverified");
        return;
    };
    let (store, dir) = store("training-real");
    store
        .put_run(&operator(), &run("run-1", 10_000))
        .expect("put");
    let runner = Runner::with_training(Arc::clone(&store), RunnerConfig::default(), Some(env))
        .expect("runner");

    // Real data, real weights, and the real operator graph vocabulary —
    // the same inputs the demo trains on, so this is the CPU path and
    // not a stub. A made-up graph would be refused by the worker's own
    // validator, which would prove nothing about training.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf();
    let data = root.join("examples/self-learning/data");
    let weights = dir.join("seed.json");
    std::fs::write(&weights, "{}").expect("seed weights");

    let outcome = runner
        .drive(
            &operator(),
            &RunRequest {
                run_id: "run-1".into(),
                kind: WorkKind::Training {
                    spec: grove_app::runner::TrainingSpec {
                        graph: serde_json::json!({
                            "inputs": {
                                "image": {"shape": [256], "dtype": "float32", "space": "pixel"},
                                "numeric": {"shape": [2], "dtype": "float32", "space": "signed"}
                            },
                            "ops": [
                                {"kind": "normalize", "inputs": ["image"], "output": "image_n", "attrs": {}},
                                {"kind": "normalize", "inputs": ["numeric"], "output": "numeric_n", "attrs": {}},
                                {"kind": "concat", "inputs": ["image_n", "numeric_n"], "output": "fused",
                                 "attrs": {"spaces": ["pixel", "signed"]}},
                                {"kind": "linear", "inputs": ["fused"], "output": "h0", "attrs": {}}
                            ],
                            "outputs": {"logits": "h0"},
                            "trainable": ["h0"]
                        }),
                        weights: weights.clone(),
                        data: data.join("train.bin"),
                        val_data: Some(data.join("val.bin")),
                        steps: 5,
                        seed: 7,
                        save_at: None,
                        resume: None,
                    },
                },
            },
        )
        .expect("drive");
    assert_eq!(
        outcome.status, "completed",
        "the worker must actually train, error was {:?}",
        outcome.error
    );
    // The loss curve is in the durable log, not just in the summary: a
    // reader following the cursor sees the run happen.
    let events = grove::events::read_events_from_start(&store, "run-1").expect("events");
    assert!(
        events.iter().any(|e| e.detail.contains("progress step")),
        "progress must be recorded, got {:?}",
        events.iter().map(|e| e.detail.clone()).collect::<Vec<_>>()
    );
    assert_eq!(store.get_run("run-1").unwrap().state, RunState::Evaluating);
    let _ = std::fs::remove_dir_all(&dir);
    drop(store);
}

use grove_app::runner::WorkKind;
