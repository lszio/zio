//! G04 contract: a candidate is immutable and scope-checked, qualification
//! is not publication, and the deployment pointer moves only behind an
//! authenticated human approval.
//!
//! Every test here is a refusal that must *stay* refused. A governance
//! system whose tests only cover the happy path has tested that the
//! happy path works.

use std::path::PathBuf;

use grove::contracts::{
    Actor, ActorRole, ArtifactRef, ErrorKind, EvaluationRecord, SCHEMA_VERSION,
};
use grove::evaluation::{EvaluationProtocol, RepeatOutcome};
use grove::logic::{self, CandidateState, Governance, GovernanceMap, LogicCandidate};
use grove::store::Store;

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

fn publisher() -> Actor {
    Actor::new("release", ActorRole::Publisher)
}

fn annotator() -> Actor {
    Actor::new("labeller", ActorRole::Annotator)
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-g04-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn protocol() -> EvaluationProtocol {
    let mut p = EvaluationProtocol::new("logic-v1", "agent-code", "ds-1");
    p.seeds = vec![1, 2, 3];
    p.gates = vec![("pass-rate".to_string(), 0.95)];
    p
}

/// The repository's governance: the agent's own logic is governed, the
/// evaluator and the permission configuration are protected.
fn governance() -> GovernanceMap {
    GovernanceMap::new()
        .with("libs/loom/agent.zio", Governance::Governed)
        .with(
            "apps/grove/native/learning/src/evaluation.rs",
            Governance::Protected,
        )
        .with("apps/grove/native/app/src/api.rs", Governance::Protected)
}

const PARENT: &str = r#"
(defn agent-run [task max-turns]
  (if (> max-turns 0)
    {:status "candidate"}
    {:status "exhausted"}))
"#;

/// A candidate whose source genuinely differs from the parent: a new
/// definition, so `changed_regions` has something real to report.
const REVISED: &str = r#"
(defn agent-run [task max-turns]
  (if (> max-turns 0)
    {:status "candidate"}
    {:status "exhausted"}))

(defn validate-entry [source]
  (= "agent-entry" (str source)))
"#;

struct Candidate {
    candidate: LogicCandidate,
    source_ref: ArtifactRef,
}

fn candidate(store: &Store, id: &str, source: &str, path: &str, deps: &[&str]) -> Candidate {
    let source_ref = store.artifacts().put(source.as_bytes()).unwrap();
    let diff_ref = store
        .artifacts()
        .put(format!("--- a/libs/loom/agent.zio\n+++ b/{path}\n").as_bytes())
        .unwrap();
    let candidate = LogicCandidate {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        parent_logic_ref: Some(store.artifacts().put(PARENT.as_bytes()).unwrap()),
        module_path: path.to_string(),
        source_ref,
        declared_dependencies: deps.iter().map(|d| d.to_string()).collect(),
        diff_ref,
        evidence_refs: Vec::new(),
        created_at_ms: 1,
    };
    Candidate {
        candidate,
        source_ref,
    }
}

fn record(store: &Store, snapshot: &ArtifactRef, pass: f64, repeat: u32) -> EvaluationRecord {
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: format!("eval-logic-v1-{}-{repeat}", &snapshot.to_hex()[..12]),
        snapshot: *snapshot,
        protocol_id: "logic-v1".to_string(),
        dataset_revision: "ds-1".to_string(),
        metrics: vec![("pass-rate".to_string(), pass)],
        repeat_index: repeat,
        device: "cpu".to_string(),
        completed_at_ms: 1,
    };
    store.put_evaluation(&operator(), &record).unwrap();
    record
}

/// Distinct candidate sources in one test, so their evaluation ids do
/// not collide. Two candidates whose *proposed text* happens to be equal
/// are still two candidates — the identity is the artifact, and equal
/// bytes means the id is equal.
fn distinct_source(store: &Store, tag: &str) -> String {
    format!("{REVISED}\n;; {tag}\n")
}

// ── governance and scope ───────────────────────────────────────────

#[test]
fn the_longest_matching_prefix_wins_so_a_protected_file_inside_a_learnable_tree_stays_protected() {
    let map = GovernanceMap::new()
        .with("libs", Governance::Learnable)
        .with("libs/loom/agent.zio", Governance::Protected);
    assert_eq!(map.state_of("libs/loom/agent.zio"), Governance::Protected);
    assert_eq!(map.state_of("libs/loom/other.zio"), Governance::Learnable);
    // a prefix must match on whole components, not anywhere inside a name
    assert_eq!(
        map.state_of("libs/loom/agent.zio.bak"),
        Governance::Learnable
    );
    // and undeclared code is ordinary code
    assert_eq!(map.state_of("somewhere/else.zio"), Governance::Learnable);
}

#[test]
fn a_candidate_may_not_rewrite_a_protected_region() {
    let store = Store::open(temp_root("protected")).unwrap();
    let c = candidate(
        &store,
        "c-protected",
        REVISED,
        "apps/grove/native/learning/src/evaluation.rs",
        &[],
    );
    let err = logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation);
    assert!(
        err.to_string().contains("protected"),
        "the refusal must name the reason: {err}"
    );
    // nothing was written: a refused proposal is a claim never made
    assert!(store.candidate_state(&c.candidate.id).is_err());
}

#[test]
fn a_candidate_that_hides_a_new_dependency_is_refused() {
    let store = Store::open(temp_root("hidden-dep")).unwrap();
    // the source requires `nio/net`, and the proposal declares nothing
    let source = format!("{REVISED}\n(require :nio/net)\n");
    let c = candidate(&store, "c-hidden", &source, "libs/loom/agent.zio", &[]);
    let err = logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation);
    assert!(err.to_string().contains("nio/net"), "{err}");
}

#[test]
fn a_candidate_that_declares_a_dependency_it_does_not_require_is_refused() {
    let store = Store::open(temp_root("fake-dep")).unwrap();
    let c = candidate(
        &store,
        "c-fake",
        REVISED,
        "libs/loom/agent.zio",
        &["nio/net"],
    );
    let err = logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation);
    assert!(err.to_string().contains("does not require"), "{err}");
}

#[test]
fn a_newly_required_dependency_outside_a_learnable_region_is_refused() {
    let store = Store::open(temp_root("new-dep")).unwrap();
    let source = format!("{REVISED}\n(require :nio/net)\n");
    // declared honestly this time, but the region it reaches is protected
    let mut map = GovernanceMap::new().with("libs/loom", Governance::Governed);
    map = map.with("nio/net", Governance::Protected);
    let c = candidate(
        &store,
        "c-newdep",
        &source,
        "libs/loom/agent.zio",
        &["nio/net"],
    );
    let err =
        logic::propose(&store, &operator(), &map, &c.candidate, Some(PARENT), &[]).unwrap_err();
    assert_eq!(err.kind, ErrorKind::ProtocolViolation);
    assert!(err.to_string().contains("not learnable"), "{err}");
}

#[test]
fn a_governed_region_accepts_a_real_candidate_and_reports_what_changed() {
    let store = Store::open(temp_root("governed")).unwrap();
    let c = candidate(&store, "c-ok", REVISED, "libs/loom/agent.zio", &[]);
    let digest = logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    assert_eq!(
        store.candidate_state("c-ok").unwrap(),
        CandidateState::Proposed
    );
    // the recorded source is the real bytes, not a digest of them
    let body = store.get_candidate("c-ok").unwrap();
    assert_eq!(body.source_ref, c.source_ref);
    let bytes = store.artifacts().get(&body.source_ref).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("validate-entry"), "{text}");
    let _ = digest;
}

#[test]
fn an_operator_may_not_approve_and_a_candidate_may_not_be_proposed_by_an_annotator() {
    let store = Store::open(temp_root("roles")).unwrap();
    let c = candidate(&store, "c-roles", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &c.source_ref, 1.0, 0);
    let verdict =
        logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();
    assert_eq!(verdict.state, CandidateState::Qualified);

    // approval is a publisher grant, not an operator one
    let err = logic::record_approval(
        &store,
        &operator(),
        "c-roles",
        &c.source_ref,
        &verdict.evaluation_refs,
        None,
        2,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);

    // and proposing is an operator grant, not an annotator's
    let err = logic::propose(
        &store,
        &annotator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

// ── qualification is not publication ───────────────────────────────

#[test]
fn evaluating_a_candidate_leaves_the_active_publication_alone() {
    let store = Store::open(temp_root("accept-eq-publish")).unwrap();
    let c = candidate(&store, "c-accept", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &c.source_ref, 1.0, 0);
    let verdict =
        logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();
    assert_eq!(verdict.state, CandidateState::Qualified);
    // qualified, and nobody's deployment
    assert!(store.active_publication().unwrap().is_none());
}

#[test]
fn a_candidate_whose_program_says_it_passed_is_still_unqualified() {
    let store = Store::open(temp_root("self-report")).unwrap();
    let c = candidate(&store, "c-self", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    // The candidate "reports" success in its own text. That text is the
    // program's stdout, not an evaluation record, so it earns nothing.
    let bytes = store.artifacts().get(&c.source_ref).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("pass"),
        "the fixture must not already claim a pass"
    );

    let verdict =
        logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();
    assert_eq!(verdict.state, CandidateState::Rejected);
    assert!(
        verdict.gate_failures[0].contains("no evaluation"),
        "silence must not read as a pass: {:?}",
        verdict.gate_failures
    );
}

#[test]
fn a_regressed_candidate_is_rejected_and_the_record_stays() {
    let store = Store::open(temp_root("regress")).unwrap();
    let c = candidate(&store, "c-bad", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &c.source_ref, 0.20, 0);
    record(&store, &c.source_ref, 0.30, 1);
    let verdict =
        logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();
    assert_eq!(verdict.state, CandidateState::Rejected);
    assert!(
        verdict.gate_failures[0].contains("pass-rate"),
        "{:?}",
        verdict.gate_failures
    );
    assert_eq!(
        store.candidate_state("c-bad").unwrap(),
        CandidateState::Rejected
    );
    // and the evidence behind the refusal is still readable
    assert_eq!(store.evaluations_for(&c.source_ref).unwrap().len(), 2);
}

#[test]
fn a_rough_repeat_may_not_be_averaged_away_by_a_good_one() {
    let store = Store::open(temp_root("rough")).unwrap();
    let c = candidate(&store, "c-rough", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    // a timeout is recorded at the protocol's worst case, never dropped
    grove::evaluation::record_evaluation(
        &store,
        &operator(),
        &protocol(),
        &c.source_ref,
        0,
        &RepeatOutcome::Measured {
            metrics: vec![("pass-rate".to_string(), 1.0)],
        },
        1,
    )
    .unwrap();
    grove::evaluation::record_evaluation(
        &store,
        &operator(),
        &protocol(),
        &c.source_ref,
        1,
        &RepeatOutcome::Crashed("worker died".into()),
        1,
    )
    .unwrap();
    let verdict =
        logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();
    // mean 0.5 < 0.95: the crash is in the denominator
    assert_eq!(verdict.state, CandidateState::Rejected);
    assert!(
        verdict.gate_failures[0].contains("0.5000"),
        "{:?}",
        verdict.gate_failures
    );
}

#[test]
fn evaluations_from_another_protocol_do_not_qualify_a_candidate() {
    let store = Store::open(temp_root("wrong-proto")).unwrap();
    let c = candidate(&store, "c-proto", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    let mut easy = EvaluationProtocol::new("logic-v2-easier", "agent-code", "ds-1");
    easy.gates = vec![("pass-rate".to_string(), 0.10)];
    grove::evaluation::record_evaluation(
        &store,
        &operator(),
        &easy,
        &c.source_ref,
        0,
        &RepeatOutcome::Measured {
            metrics: vec![("pass-rate".to_string(), 0.5)],
        },
        1,
    )
    .unwrap();
    let verdict =
        logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();
    assert_eq!(
        verdict.state,
        CandidateState::Rejected,
        "the easy protocol's 0.5 must not leak in"
    );
}

// ── the approval boundary ──────────────────────────────────────────

#[test]
fn the_full_sequence_switches_the_deployment_and_keeps_the_approval() {
    let store = Store::open(temp_root("switch")).unwrap();
    let c = candidate(&store, "c-switch", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &c.source_ref, 1.0, 0);

    let (version, approval) =
        logic::approve_and_publish(&store, &publisher(), &protocol(), &c.candidate, None, 10)
            .unwrap();
    assert_eq!(version, 1);
    let active = store.active_publication().unwrap().unwrap();
    assert_eq!(active.0, 1);
    assert_eq!(
        active.1, c.source_ref,
        "the pointer names the proposed source, not a digest of it"
    );
    assert_eq!(
        store.candidate_state("c-switch").unwrap(),
        CandidateState::Active
    );

    // the approval is a record, not a boolean somebody passed in
    let stored = store.get_approval(&approval.id).unwrap();
    assert_eq!(stored.authenticated_actor, "release");
    assert_eq!(stored.subject_id, "c-switch");
    assert!(!stored.evaluation_refs.is_empty());
    assert_eq!(stored.decision, "approved");
}

#[test]
fn a_stale_approval_is_refused_after_the_pointer_moves() {
    let store = Store::open(temp_root("stale")).unwrap();
    let first = candidate(
        &store,
        "c-first",
        &distinct_source(&store, "first"),
        "libs/loom/agent.zio",
        &[],
    );
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &first.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &first.source_ref, 1.0, 0);
    logic::approve_and_publish(
        &store,
        &publisher(),
        &protocol(),
        &first.candidate,
        None,
        10,
    )
    .unwrap();

    // a second candidate qualifies against v1, and an approval is
    // recorded that commits to replacing v1
    let second = candidate(
        &store,
        "c-second",
        &distinct_source(&store, "second"),
        "libs/loom/agent.zio",
        &[],
    );
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &second.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &second.source_ref, 1.0, 0);
    let approval = logic::record_approval(
        &store,
        &publisher(),
        "c-second",
        &second.source_ref,
        &[store
            .commit_manifest(
                "EvaluationRecord",
                "trainer",
                &store.evaluations_for(&second.source_ref).unwrap()[0],
            )
            .unwrap()],
        Some(1),
        11,
    )
    .unwrap();

    // meanwhile the pointer moved on to v2
    let other = candidate(
        &store,
        "c-other",
        &distinct_source(&store, "other"),
        "libs/loom/agent.zio",
        &[],
    );
    record(&store, &other.source_ref, 1.0, 0);
    grove::evaluation::publish_snapshot(
        &store,
        &publisher(),
        &protocol(),
        &other.source_ref,
        Some(1),
    )
    .unwrap();
    assert_eq!(store.active_publication().unwrap().unwrap().0, 2);

    // the approval that committed to v1 can no longer be applied
    let err = logic::apply_approval(&store, &publisher(), &approval, Some(1)).unwrap_err();
    // A lost race is a `Conflict`, not a malformed approval: the
    // decision was fine and the world moved underneath it.
    assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
    assert_eq!(store.active_publication().unwrap().unwrap().0, 2);

    // And a *new* approval against a version that has moved is refused
    // before anything is written — no row claiming a human approved
    // something that was never applied.
    let third = candidate(
        &store,
        "c-third",
        &distinct_source(&store, "third"),
        "libs/loom/agent.zio",
        &[],
    );
    record(&store, &third.source_ref, 1.0, 0);
    let before = store.approval_log().unwrap().len();
    let err = logic::record_approval(
        &store,
        &publisher(),
        "c-third",
        &third.source_ref,
        &[third.source_ref],
        Some(1),
        12,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict, "{err}");
    assert_eq!(
        store.approval_log().unwrap().len(),
        before,
        "a refused race leaves no approval row"
    );
}

#[test]
fn a_candidate_that_fails_its_gates_is_never_approved_and_the_old_version_survives() {
    let store = Store::open(temp_root("no-approve")).unwrap();
    // publish a first, passing version
    let first = candidate(&store, "c-first", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &first.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &first.source_ref, 1.0, 0);
    logic::approve_and_publish(
        &store,
        &publisher(),
        &protocol(),
        &first.candidate,
        None,
        10,
    )
    .unwrap();

    // a second candidate that does not qualify
    let second = candidate(
        &store,
        "c-second",
        &distinct_source(&store, "second"),
        "libs/loom/agent.zio",
        &[],
    );
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &second.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &second.source_ref, 0.10, 0);
    let err = logic::approve_and_publish(
        &store,
        &publisher(),
        &protocol(),
        &second.candidate,
        Some(1),
        11,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert_eq!(
        store.active_publication().unwrap().unwrap().1,
        first.source_ref,
        "not approving means the old version keeps serving"
    );
    assert_eq!(
        store.active_publication().unwrap().unwrap().0,
        1,
        "and no new version is minted"
    );
}

#[test]
fn declining_records_the_reason_and_leaves_the_deployment_alone() {
    let store = Store::open(temp_root("decline")).unwrap();
    let c = candidate(&store, "c-decline", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &c.source_ref, 1.0, 0);
    logic::evaluate_candidate(&store, &operator(), &protocol(), &c.candidate, 1).unwrap();

    logic::decline(
        &store,
        &publisher(),
        "c-decline",
        "the diff needs a second reviewer",
        12,
    )
    .unwrap();
    assert_eq!(
        store.candidate_state("c-decline").unwrap(),
        CandidateState::Declined
    );
    let (actor, reason, _) = store.rejection("c-decline").unwrap().unwrap();
    assert_eq!(actor, "release");
    assert!(reason.contains("second reviewer"), "{reason}");
    assert!(
        store.active_publication().unwrap().is_none(),
        "declining publishes nothing"
    );
}

#[test]
fn an_annotator_cannot_decline() {
    let store = Store::open(temp_root("decline-role")).unwrap();
    let err = logic::decline(&store, &annotator(), "whatever", "no", 1).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied);
}

// ── in-flight pinning ──────────────────────────────────────────────

#[test]
fn an_in_flight_call_keeps_its_version_while_the_next_call_gets_the_new_one() {
    let store = Store::open(temp_root("inflight")).unwrap();
    let first = candidate(
        &store,
        "c-v1",
        &distinct_source(&store, "v1"),
        "libs/loom/agent.zio",
        &[],
    );
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &first.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &first.source_ref, 1.0, 0);

    // a call starts and pins the logic it began with
    let pinned = logic::ActiveLogic::pin(&store).unwrap();
    assert!(pinned.is_none(), "nothing is deployed yet");

    logic::approve_and_publish(
        &store,
        &publisher(),
        &protocol(),
        &first.candidate,
        None,
        10,
    )
    .unwrap();

    // the pinned value was taken before the switch, so it is unaffected
    assert_eq!(pinned, None);
    let next = logic::ActiveLogic::pin(&store).unwrap().unwrap();
    assert_eq!(next.snapshot, first.source_ref);
    assert_eq!(next.candidate_id.as_deref(), Some("c-v1"));

    // switching again does not reach into the earlier pin
    let second = candidate(
        &store,
        "c-v2",
        &distinct_source(&store, "v2"),
        "libs/loom/agent.zio",
        &[],
    );
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &second.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    record(&store, &second.source_ref, 1.0, 0);
    logic::approve_and_publish(
        &store,
        &publisher(),
        &protocol(),
        &second.candidate,
        Some(1),
        11,
    )
    .unwrap();
    assert_eq!(
        next.snapshot, first.source_ref,
        "the in-flight call keeps its version"
    );
    assert_eq!(
        logic::ActiveLogic::pin(&store).unwrap().unwrap().snapshot,
        second.source_ref,
        "the next call gets the new one"
    );
}

// ── candidate immutability ─────────────────────────────────────────

#[test]
fn a_second_proposal_under_one_id_is_a_conflict_not_an_overwrite() {
    let store = Store::open(temp_root("immutable")).unwrap();
    let c = candidate(&store, "c-same", REVISED, "libs/loom/agent.zio", &[]);
    logic::propose(
        &store,
        &operator(),
        &governance(),
        &c.candidate,
        Some(PARENT),
        &[],
    )
    .unwrap();
    let mut other = c.candidate.clone();
    other.source_ref = store.artifacts().put(b"(println \"different\")").unwrap();
    let err = logic::put_candidate(&store, &operator(), &other).unwrap_err();
    assert_eq!(
        err.kind,
        ErrorKind::Conflict,
        "a candidate id names one proposal"
    );
}

// ── model snapshots share the boundary ─────────────────────────────

#[test]
fn a_model_publication_goes_through_the_same_approval() {
    let store = Store::open(temp_root("model")).unwrap();
    let weights = store.artifacts().put(b"trained-weights").unwrap();
    let snapshot = store
        .put_snapshot(
            &operator(),
            &grove::contracts::ModelSnapshot {
                schema: SCHEMA_VERSION,
                owner: "trainer".to_string(),
                entrypoint: "predict".to_string(),
                params: vec![grove::contracts::ParamRef {
                    module: "fusion".into(),
                    shape: vec![1],
                    dtype: "float32".into(),
                    artifact: weights,
                }],
                libraries: vec![],
                preprocessing_version: "geometry-sensor-xor@1.0.0".into(),
                modules: vec![],
                ensemble: None,
                graph: None,
            },
        )
        .unwrap();
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: "eval-model-0".to_string(),
        snapshot,
        protocol_id: "logic-v1".to_string(),
        dataset_revision: "ds-1".to_string(),
        metrics: vec![("pass-rate".to_string(), 1.0)],
        repeat_index: 0,
        device: "cpu".to_string(),
        completed_at_ms: 1,
    };
    store.put_evaluation(&operator(), &record).unwrap();

    let version =
        grove::evaluation::publish_snapshot(&store, &publisher(), &protocol(), &snapshot, None)
            .unwrap()
            .0;
    assert_eq!(version, 1);
    assert_eq!(store.active_publication().unwrap().unwrap().1, snapshot);
    // and the approval it went through is on the record
    let approvals = store.approval_log().unwrap();
    let approval = approvals
        .iter()
        .find(|a| {
            a["subject_id"] == serde_json::Value::String(format!("{}@logic-v1", snapshot.to_hex()))
        })
        .expect("the approval that published this snapshot is recorded");
    assert_eq!(approval["authenticated_actor"], "release");
}

/// A snapshot whose gates fail is refused by the same comparison, before
/// any approval exists.
#[test]
fn an_underfit_snapshot_never_reaches_the_approval_boundary() {
    let store = Store::open(temp_root("underfit")).unwrap();
    let weights = store.artifacts().put(b"underfit").unwrap();
    let snapshot = store
        .put_snapshot(
            &operator(),
            &grove::contracts::ModelSnapshot {
                schema: SCHEMA_VERSION,
                owner: "trainer".to_string(),
                entrypoint: "predict".to_string(),
                params: vec![grove::contracts::ParamRef {
                    module: "fusion".into(),
                    shape: vec![1],
                    dtype: "float32".into(),
                    artifact: weights,
                }],
                libraries: vec![],
                preprocessing_version: "geometry-sensor-xor@1.0.0".into(),
                modules: vec![],
                ensemble: None,
                graph: None,
            },
        )
        .unwrap();
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: "eval-underfit-0".to_string(),
        snapshot,
        protocol_id: "logic-v1".to_string(),
        dataset_revision: "ds-1".to_string(),
        metrics: vec![("pass-rate".to_string(), 0.4)],
        repeat_index: 0,
        device: "cpu".to_string(),
        completed_at_ms: 1,
    };
    store.put_evaluation(&operator(), &record).unwrap();
    let err =
        grove::evaluation::publish_snapshot(&store, &publisher(), &protocol(), &snapshot, None)
            .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState);
    assert!(store.active_publication().unwrap().is_none());
    assert!(
        store.rejection(&snapshot.to_hex()).unwrap().is_none(),
        "a refusal is not a decline"
    );
}

// `Store::publish` is `pub(crate)`: whether anything outside the crate
// can move the pointer without an approval is answered by the compiler,
// not by a test. A test that asserted it would assert nothing.
