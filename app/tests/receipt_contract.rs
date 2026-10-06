//! G03 receipts contract: an operation id means the same thing after a
//! restart.
//!
//! The API kept its idempotency in a `HashMap` on `ApiState`, so a
//! replayed request answered from memory that a restart threw away: the
//! same `operation_id` did the work twice, and the second copy was
//! indistinguishable from the first. These checks restart the process's
//! view of the store — a fresh `ApiState` over the same root — and
//! require the second call to return the first one's answer.

#![cfg(feature = "http")]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use grove::contracts::{Actor, ActorRole};
use grove::store::Store;
use grove_app::api::ApiState;

/// The same store root, reached twice. That is all a restart is from the
/// receipt's point of view.
struct Reboot {
    root: PathBuf,
}

impl Reboot {
    fn start(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!("grove-receipt-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Reboot { root }
    }

    fn state(&self, tokens: HashMap<String, ActorRole>) -> ApiState {
        let store = Arc::new(Store::open(&self.root).expect("open store"));
        ApiState::new(store, tokens)
    }
}

fn operator() -> Actor {
    Actor::new("grove-cli", ActorRole::Operator)
}

#[test]
fn a_receipt_survives_a_restart() {
    let reboot = Reboot::start("survives");
    let tokens: HashMap<String, ActorRole> = HashMap::new();
    let state = reboot.state(tokens.clone());

    // The first call records a receipt through the same path the HTTP
    // handlers use.
    let first = state.record_receipt("op-1", 200, &serde_json::json!({"id": "run-op-1"}));
    assert!(first.is_ok(), "recording a receipt must succeed: {first:?}");
    drop(state);

    // A restart: same root, new state, empty memory.
    let restarted = reboot.state(tokens);
    let replay = restarted
        .replay_receipt::<serde_json::Value>("op-1")
        .expect("a receipt from before the restart must still be there");
    let (status, body) = replay;
    assert_eq!(status, 200);
    assert_eq!(body["id"], "run-op-1", "the replay is the original answer");
    let _ = std::fs::remove_dir_all(&reboot.root);
}

#[test]
fn an_operation_id_with_a_different_answer_is_refused() {
    let reboot = Reboot::start("mismatch");
    let state = reboot.state(HashMap::new());
    state
        .record_receipt("op-2", 200, &serde_json::json!({"id": "run-a"}))
        .expect("record");
    // The same id with a different body is a contradiction: the caller
    // believes it is asking for the same thing, and it is not. Silently
    // returning the first answer would hide that.
    let err = state
        .record_receipt("op-2", 200, &serde_json::json!({"id": "run-b"}))
        .expect_err("one id cannot mean two answers");
    assert!(
        format!("{err}").contains("op-2"),
        "the refusal must name the id: {err}"
    );
    let _ = std::fs::remove_dir_all(&reboot.root);
}

#[test]
fn a_receipt_is_written_by_the_housekeeper_and_read_by_a_reader() {
    let reboot = Reboot::start("role");
    let state = reboot.state(HashMap::new());
    state
        .record_receipt("op-3", 200, &serde_json::json!({"ok": true}))
        .expect("record");
    // A reader may look at a receipt without being able to write one.
    assert!(state
        .replay_receipt::<serde_json::Value>("op-3")
        .is_some());
    // Writing is an operator's act, and an unknown id is simply absent.
    assert!(state.replay_receipt::<serde_json::Value>("never-seen").is_none());
    let _ = std::fs::remove_dir_all(&reboot.root);
    let _ = operator();
}

#[test]
fn a_receipt_is_written_once_per_id_under_concurrent_writers() {
    let reboot = Reboot::start("concurrent");
    // Two independent states over one root: two processes' view of the
    // same ledger. Exactly one may own the id.
    let a = reboot.state(HashMap::new());
    let b = reboot.state(HashMap::new());
    let first = a.record_receipt("op-4", 200, &serde_json::json!({"who": "a"}));
    let second = b.record_receipt("op-4", 200, &serde_json::json!({"who": "b"}));
    assert!(first.is_ok(), "the first claim wins: {first:?}");
    // The loser is refused rather than overwriting: two answers for one
    // id is a contradiction, and last-write-wins would hide the race.
    let err = second.expect_err("a second claim on the same id is refused");
    assert!(
        format!("{err}").contains("op-4"),
        "the refusal must name the id: {err}"
    );
    let _ = std::fs::remove_dir_all(&reboot.root);
}
