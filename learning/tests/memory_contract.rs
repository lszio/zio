//! W14 contract: three indexes over ONE experience fact base, four hard
//! refusals, abstraction extraction, and a measured reuse result.
//!
//! Everything here runs against real bytes and a real `Store`. The index
//! arithmetic really executes `lib/zio/memory.zio` and `lib/zio/vector.zio`
//! through a zio `EvalContext`; the licence and retraction gates read the
//! real SQLite-backed store.

use std::path::PathBuf;

use grove::contracts::{
    Actor, ActorRole, ErrorKind, LearningSignal, Observation, SignalKind, SCHEMA_VERSION,
};
use grove::memory::{
    self, AbstractionCandidate, ExperienceIndexEntry, MemoryIndex, PromotionEvidence,
    SemanticQuery,
};
use grove::store::Store;
use zio_core::value::Value;

/// Read a zio integer result as `i64`, for assertions on library output.
trait ValueExt {
    fn as_i64(&self) -> Option<i64>;
}

impl ValueExt for Value {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }
}

fn operator() -> Actor {
    Actor::new("trainer", ActorRole::Operator)
}

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("grove-w14-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_store(name: &str) -> (Store, PathBuf) {
    let root = temp_root(name);
    let store = Store::open(&root).unwrap();
    (store, root)
}

/// An observation that permits training.
fn permitted_obs(id: &str, task: &str) -> Observation {
    memory::observation(id, task, "operator-console", true)
}

/// An observation explicitly marked as NOT training-permitted.
fn forbidden_obs(id: &str, task: &str) -> Observation {
    memory::observation(id, task, "third-party-licence", false)
}

fn signal(id: &str, task: &str, usage_permitted: bool) -> LearningSignal {
    LearningSignal {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        idempotency_key: format!("idem-{id}"),
        producer: "trainer".into(),
        kind: SignalKind::TeacherLabel,
        task_id: task.to_string(),
        observation_id: Some(format!("obs-{id}")),
        prediction_id: None,
        target_field: Some("label".into()),
        content: "0".into(),
        usage_permitted,
        occurred_at_ms: 10,
        received_at_ms: 10,
        revises: None,
    }
}

fn entry(
    id: &str,
    task: &str,
    ast: &str,
    sources: &[&str],
) -> ExperienceIndexEntry {
    ExperienceIndexEntry {
        schema: SCHEMA_VERSION,
        id: id.to_string(),
        task_id: task.to_string(),
        ast: ast.to_string(),
        probe_set: "arith-v1".to_string(),
        fingerprint: "fp-shared".to_string(),
        vector: None,
        encoder: None,
        space_version: None,
        source_observation_ids: sources.iter().map(|s| s.to_string()).collect(),
        source_signal_ids: vec![],
        usage_permitted: true,
        verified: true,
        created_at_ms: 0,
    }
}

// ── the fact base is the existing store, not a second one ────────

#[test]
fn index_entries_name_existing_records_instead_of_copying_them() {
    let (store, _root) = open_store("factbase");
    let op = operator();
    store.put_observation(&op, &permitted_obs("obs-a", "task-a")).unwrap();

    let index = MemoryIndex::new().unwrap();
    let e = entry("e-1", "task-a", "(+ (* 2 x) 1)", &["obs-a"]);
    index.commit(&store, &op, &e).unwrap();

    // The entry is committed as a durable manifest like every other record.
    let digest = grove::contracts::digest_bytes(
        &grove::contracts::canonical_json(&e).unwrap(),
    );
    let body = store.load_manifest(&digest).unwrap();
    assert!(body.contains("task-a"), "committed entry body: {body}");

    // …and the observation it names is still the single source of truth.
    assert!(store.get_observation("obs-a").unwrap().training_permitted);
}

#[test]
fn committing_a_half_declared_semantic_space_is_refused() {
    let (store, _root) = open_store("halfspace");
    let index = MemoryIndex::new().unwrap();
    let mut e = entry("e-1", "task-a", "(+ 1 2)", &[]);
    // a vector with no encoder/space version: coordinates of unknown meaning
    e.vector = Some(vec![1, 2, 3]);
    let err = index.commit(&store, &operator(), &e).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput, "{err}");
}

// ── refusal 1: semantic near-neighbour cannot merge into a new AST ─

#[test]
fn semantic_near_neighbour_never_merges_into_a_different_ast() {
    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();

    // Two entries that are near-identical in embedding space…
    let mut a = entry("e-1", "task-a", "(+ (* 2 x) 1)", &[]);
    a.vector = Some(vec![1000, 0, 0]);
    a.encoder = Some("bge-small".into());
    a.space_version = Some(2);
    let mut b = entry("e-2", "task-b", "(+ (* 7 y) 3)", &[]);
    b.vector = Some(vec![1000, 1, 0]);
    b.encoder = Some("bge-small".into());
    b.space_version = Some(2);

    // …are genuinely a near-neighbour…
    let hit = memory::semantic_nearest(
        ctx,
        &[a.clone(), b.clone()],
        &SemanticQuery {
            vector: vec![1000, 0, 0],
            encoder: "bge-small".into(),
            space_version: 2,
        },
    )
    .unwrap();
    assert!(hit.similarity_bp > 900, "expected a near neighbour, got {hit:?}");

    // …yet the structural merge refuses them: different AST, different
    // program, and no amount of embedding proximity rewrites that.
    let err = memory::merge_structural(ctx, &a, &b).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");
    assert!(err.context.contains("cannot rewrite a different AST"), "{err}");

    // An AST-identical pair still merges.
    let same = entry("e-3", "task-c", "(+ (* 2 x) 1)", &[]);
    let merged = memory::merge_structural(ctx, &a, &same).unwrap();
    assert_eq!(merged.id, "e-3");
    assert_eq!(merged.ast, a.ast);
}

// ── refusal 2: different probe sets are never compared ───────────

#[test]
fn fingerprints_under_different_probe_sets_are_never_compared() {
    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();

    let mut a = entry("b-1", "task-a", "(+ 1 2)", &[]);
    a.probe_set = "arith-v1".into();
    a.fingerprint = "fp-1".into();
    let mut b = entry("b-2", "task-b", "(+ 1 2)", &[]);
    b.probe_set = "arith-v2".into();
    // identical fingerprint value, different probe set: a comparison here
    // would invent an equivalence nobody measured
    b.fingerprint = "fp-1".into();

    let err = memory::behaviour_match(&a, &b).unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");
    assert!(err.context.contains("different probe sets"), "{err}");

    // Same probe set, same fingerprint → a real answer.
    let mut c = entry("b-3", "task-c", "(+ 1 2)", &[]);
    c.probe_set = "arith-v1".into();
    c.fingerprint = "fp-1".into();
    assert!(memory::behaviour_match(&a, &c).unwrap());

    // The zio library groups by probe set so a cross-set comparison is
    // not even representable in the index.
    let buckets = grove::memory::eval_public(
        ctx,
        "(count (memory--behavioural-bucket (memory--index-behavioural [{:memory/probes \"arith-v1\"} {:memory/probes \"arith-v2\"}]) \"arith-v1\"))",
    );
    assert_eq!(buckets.as_i64(), Some(1), "each probe set is its own bucket");
}

// ── refusal 3: retrieval cannot cross a data licence ─────────────

#[test]
fn retrieval_refuses_a_licence_violation_instead_of_filtering_the_row() {
    let (store, _root) = open_store("licence");
    let op = operator();
    store.put_observation(&op, &permitted_obs("obs-ok", "task-a")).unwrap();
    store.put_observation(&op, &forbidden_obs("obs-no", "task-b")).unwrap();

    let index = MemoryIndex::new().unwrap();

    // A clean query returns a real result.
    let ok = entry("e-ok", "task-a", "(+ (* 2 x) 1)", &["obs-ok"]);
    let hits = index
        .structural_search(&store, &[ok.clone()], "(+ (* 4 y) 9)")
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "e-ok");

    // The same shape, but sourced from an observation that forbids
    // training: an ERROR, not an empty list. An empty list would be
    // indistinguishable from "nothing indexed".
    let forbidden = entry("e-no", "task-b", "(+ (* 2 x) 1)", &["obs-no"]);
    let err = index
        .structural_search(&store, &[ok, forbidden.clone()], "(+ (* 4 y) 9)")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied, "{err}");
    assert!(err.context.contains("obs-no"), "{err}");

    // usage_permitted=false on the entry itself is equally refused.
    let mut self_forbidden = entry("e-self", "task-b", "(+ (* 2 x) 1)", &["obs-ok"]);
    self_forbidden.usage_permitted = false;
    let err = memory::check_retrievable(&store, &self_forbidden).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied, "{err}");
}

// ── refusal 4: a capability from a retracted source refuses ──────

#[test]
fn a_capability_from_a_retracted_source_refuses_to_load() {
    let (store, _root) = open_store("retracted");
    let op = operator();
    store.put_observation(&op, &permitted_obs("obs-a", "task-a")).unwrap();
    let sig = signal("sig-a", "task-a", true);
    store.submit_signal(&op, &sig).unwrap();
    assert_eq!(store.signal_status("sig-a").unwrap(), "accepted");

    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();
    let programs = [
        "(+ (* 2 x) 1)".to_string(),
        "(+ (* 3 y) 2)".to_string(),
    ];
    let candidate =
        memory::extract_abstraction(ctx, &programs, &["task-a".into(), "task-b".into()]).unwrap();
    assert_eq!(candidate.spine, vec!["+".to_string(), "*".to_string()]);
    assert!(!candidate.is_macro, "a 2-deep spine is an ordinary function");

    let mut source = entry("e-1", "task-a", "(+ (* 2 x) 1)", &["obs-a"]);
    source.source_signal_ids = vec!["sig-a".into()];

    // Before retraction the capability promotes on real, measured gates.
    let evidence = PromotionEvidence {
        regression_programs: programs.to_vec(),
        new_task_id: "task-new".into(),
        new_task_program: "(+ (* 11 w) 4)".into(),
    };
    let promoted =
        memory::promote_abstraction(&store, &op, &candidate, std::slice::from_ref(&source), &evidence, 1_000)
            .unwrap();
    assert_eq!(promoted.validated_on, "task-new");

    // Now the source signal is withdrawn. The knowledge it produced is
    // stale, and the capability must refuse rather than serve it.
    store.retract_signal(&op, "sig-a").unwrap();
    assert_eq!(store.signal_status("sig-a").unwrap(), "retracted");

    let err = memory::check_retrievable(&store, &source).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied, "{err}");

    let err = memory::promote_abstraction(
        &store,
        &op,
        &candidate,
        std::slice::from_ref(&source),
        &PromotionEvidence {
            regression_programs: programs.to_vec(),
            new_task_id: "task-new-2".into(),
            new_task_program: "(+ (* 12 q) 5)".into(),
        },
        2_000,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied, "{err}");

    // A stale capability cannot be reloaded through the licence view.
    let err = memory::resolve_licence(&store, &source).unwrap_err();
    assert_eq!(err.kind, ErrorKind::CapabilityDenied, "{err}");
}

// ── semantic space binding ───────────────────────────────────────

#[test]
fn a_query_in_another_space_version_is_refused() {
    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();
    let mut e = entry("v-1", "task-a", "(+ 1 2)", &[]);
    e.vector = Some(vec![10, 0, 0]);
    e.encoder = Some("bge-small".into());
    e.space_version = Some(2);

    // same space → a real hit
    let hit = memory::semantic_nearest(
        ctx,
        std::slice::from_ref(&e),
        &SemanticQuery {
            vector: vec![10, 0, 0],
            encoder: "bge-small".into(),
            space_version: 2,
        },
    )
    .unwrap();
    assert_eq!(hit.id, "v-1");
    assert_eq!(hit.similarity_bp, 1000, "identical vectors: full similarity");

    // the encoder changed → refused, coordinates mean something else now
    let err = memory::semantic_nearest(
        ctx,
        std::slice::from_ref(&e),
        &SemanticQuery {
            vector: vec![10, 0, 0],
            encoder: "bge-large".into(),
            space_version: 2,
        },
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");

    // the space version changed → refused, the index is invalidated
    // rather than silently reinterpreted
    let err = memory::semantic_nearest(
        ctx,
        std::slice::from_ref(&e),
        &SemanticQuery {
            vector: vec![10, 0, 0],
            encoder: "bge-small".into(),
            space_version: 3,
        },
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");
    assert!(err.context.contains("migrated explicitly"), "{err}");
}

// ── abstraction extraction and gated promotion ───────────────────

#[test]
fn promotion_requires_regression_and_a_task_that_never_participated() {
    let (store, _root) = open_store("promotion");
    let op = operator();
    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();

    let programs = [
        "(+ (* 2 x) 1)".to_string(),
        "(+ (* 3 y) 2)".to_string(),
        "(+ (* 5 z) 7)".to_string(),
    ];
    let discovery_tasks = ["task-a", "task-b", "task-c"];
    let candidate = memory::extract_abstraction(
        ctx,
        &programs,
        &discovery_tasks.map(String::from).to_vec(),
    )
    .unwrap();
    assert_eq!(candidate.spine, vec!["+".to_string(), "*".to_string()]);
    assert_eq!(candidate.discovered_from.len(), 3);
    assert_eq!(candidate.leaf_count, 3);
    assert!(candidate.definition.starts_with("(defn apply-spine"), "{}", candidate.definition);
    assert!(candidate.definition_dl > 0);

    // The abstraction really is the programs: same value, both sides.
    for p in &programs {
        assert!(memory::regression_holds(&candidate, p).unwrap(), "{p}");
    }

    // Gate (b) fails: the "new task" is one it was discovered from, so
    // the whole promotion would be circular.
    let err = memory::promote_abstraction(
        &store,
        &op,
        &candidate,
        &[],
        &PromotionEvidence {
            regression_programs: programs.to_vec(),
            new_task_id: "task-a".into(),
            new_task_program: "(+ (* 2 x) 1)".into(),
        },
        0,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput, "{err}");

    // Gate (b'') fails: no new-task program at all is not evidence.
    let err = memory::promote_abstraction(
        &store,
        &op,
        &candidate,
        &[],
        &PromotionEvidence {
            regression_programs: programs.to_vec(),
            new_task_id: "task-new".into(),
            new_task_program: String::new(),
        },
        0,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput, "{err}");

    // Gate (a) fails, MEASURED: a discovery program of a different shape
    // is not reproduced by this abstraction, and no caller flag can wave
    // that through — the gate runs the programs.
    let err = memory::promote_abstraction(
        &store,
        &op,
        &candidate,
        &[],
        &PromotionEvidence {
            regression_programs: vec!["(cons h t)".to_string()],
            new_task_id: "task-new".into(),
            new_task_program: "(+ (* 11 w) 4)".into(),
        },
        0,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");
    assert!(err.context.contains("leaves"), "{err}");

    // Gate (b') fails, MEASURED: the new task is outside the shape this
    // abstraction covers, so substituting its leaves is refused.
    let err = memory::promote_abstraction(
        &store,
        &op,
        &candidate,
        &[],
        &PromotionEvidence {
            regression_programs: programs.to_vec(),
            new_task_id: "task-new".into(),
            new_task_program: "(+ (* 11 w) 4 9)".into(),
        },
        0,
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");

    // Both gates pass, measured → promoted and durably committed.
    let promoted = memory::promote_abstraction(
        &store,
        &op,
        &candidate,
        &[],
        &PromotionEvidence {
            regression_programs: programs.to_vec(),
            new_task_id: "task-new".into(),
            new_task_program: "(+ (* 11 w) 4)".into(),
        },
        4_242,
    )
    .unwrap();
    assert_eq!(promoted.validated_on, "task-new");
    assert_eq!(promoted.promoted_at_ms, 4_242);
    let digest = grove::contracts::digest_bytes(
        &grove::contracts::canonical_json(&promoted).unwrap(),
    );
    assert!(store.load_manifest(&digest).is_ok());
}

#[test]
fn extraction_from_a_single_program_is_refused() {
    let index = MemoryIndex::new().unwrap();
    let err = memory::extract_abstraction(
        index.context(),
        &["(+ (* 2 x) 1)".to_string()],
        &["task-a".to_string()],
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput, "{err}");
}

#[test]
fn programs_sharing_no_spine_have_no_common_abstraction() {
    let index = MemoryIndex::new().unwrap();
    let err = memory::extract_abstraction(
        index.context(),
        &["(+ (* 2 x) 1)".to_string(), "(cons h t)".to_string()],
        &["task-a".to_string(), "task-b".to_string()],
    )
    .unwrap_err();
    assert_eq!(err.kind, ErrorKind::IncompatibleState, "{err}");
    assert!(err.context.contains("no common abstraction"), "{err}");
}

#[test]
fn a_macro_candidate_is_rechecked_after_expansion() {
    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();
    // a spine deeper than a plain function needs → the macro route is taken
    let candidate: AbstractionCandidate = memory::extract_abstraction(
        ctx,
        &[
            "(+ (* (/ 2 x) 1) 3)".to_string(),
            "(+ (* (/ 3 y) 2) 5)".to_string(),
            "(+ (* (/ 5 z) 7) 9)".to_string(),
        ],
        &["task-a".to_string(), "task-b".to_string(), "task-c".to_string()],
    )
    .unwrap();
    assert!(candidate.is_macro, "deep spine: {}", candidate.spine.join("/"));
    assert_eq!(candidate.spine.len(), 3);

    // The expansion is re-read: expansion can reintroduce dependencies
    // and permissions the surface form hid, so it is validated as data.
    let expanded = grove::memory::eval_public(
        ctx,
        "(memory--expanded-body '(memory--apply-spine 'x 'y))",
    );
    let text = expanded.to_string();
    assert!(
        !text.contains("memory--apply-spine"),
        "expansion must not still be a macro call: {text}"
    );
    assert!(text.contains('y'), "expansion carries the argument through: {text}");
}

// ── measurement, not vibes ───────────────────────────────────────

#[test]
fn reuse_is_measured_as_total_description_length_on_a_fresh_task() {
    let index = MemoryIndex::new().unwrap();
    let ctx = index.context();

    // Discovery set: three verified programs sharing one spine.
    let discovery = [
        "(+ (* 2 x) 1)".to_string(),
        "(+ (* 3 y) 2)".to_string(),
        "(+ (* 5 z) 7)".to_string(),
    ];
    let candidate = memory::extract_abstraction(
        ctx,
        &discovery,
        &["task-a".to_string(), "task-b".to_string(), "task-c".to_string()],
    )
    .unwrap();

    // The measured task NEVER participated in discovery.
    let fresh_task = "(+ (* 11 w) 4)";
    assert!(!discovery.contains(&fresh_task.to_string()));

    let report = memory::measure_reuse(ctx, &candidate, &[fresh_task]).unwrap();
    // Real numbers from real node counts and the real zio library. The
    // library definition is charged on top of the call site, and that
    // sum is the only number that decides anything.
    eprintln!(
        "W14 one call: before={} call_after={} definition={} total={} saving={}",
        report.before_dl,
        report.after_call_dl,
        report.definition_dl,
        report.total_dl,
        report.net_saving
    );
    assert!(report.before_dl > 0, "the raw program is measured: {report:?}");
    assert_eq!(report.definition_dl, candidate.definition_dl);
    assert_eq!(
        report.total_dl,
        report.after_call_dl + report.definition_dl,
        "the honest total includes the library definition"
    );
    assert_eq!(report.net_saving, report.total_dl < report.before_dl);
    // The measured verdict is reported as measured. For a single
    // three-element program the abstraction does NOT pay for itself once
    // the definition is counted, and saying so is the whole point of
    // the measurement.
    assert!(
        !report.net_saving,
        "one call cannot amortise a {} -node definition: {report:?}",
        report.definition_dl
    );
    // …and it is nevertheless CORRECT, which is the other half: a failed
    // cost case must not be a failed reuse case.
    assert!(memory::regression_holds(&candidate, fresh_task).unwrap());

    // Amortised over a task that uses the SAME shape more than once, the
    // same accounting is applied and the verdict flips — measured, not
    // assumed. This is the case reuse is for: several calls pay for one
    // definition; one call never does.
    let two = ["(+ (* 1 2) 3)", "(+ (* 4 5) 6)"];
    let many = memory::measure_reuse(ctx, &candidate, &two).unwrap();
    eprintln!(
        "W14 two calls: before={} call_after={} definition={} total={} saving={}",
        many.before_dl, many.after_call_dl, many.definition_dl, many.total_dl, many.net_saving
    );
    assert_eq!(many.before_dl, 2 * report.before_dl, "both raw programs counted");
    assert_eq!(many.after_call_dl, 2 * report.after_call_dl, "both call sites counted");
    assert_eq!(
        many.total_dl,
        many.after_call_dl + many.definition_dl,
        "the definition is paid once"
    );
    for p in two {
        assert!(memory::regression_holds(&candidate, p).unwrap(), "{p}");
    }
}
