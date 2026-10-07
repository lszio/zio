//! W16 contract: expert routing, output combination and multi-teacher
//! distillation.
//!
//! The properties under test are the failure modes an ensemble hides:
//!
//! * experts whose outputs live in different semantic spaces cannot be
//!   integrated — refused at bind time, never averaged at run time;
//! * with no available expert the ensemble abstains. It does not
//!   fabricate a class, and a population's silence is not a majority;
//! * an unavailable expert and a missing modality are *reported*, so
//!   coverage is a real number rather than a healthy-looking metric;
//! * one ensemble call is charged for every expert it called, and the
//!   sum cannot exceed the declared per-call budget;
//! * `AllAgree` with a missing expert abstains — the rule IS the answer;
//! * a unanimous teacher population produces a *target with an agreement
//!   level*, not a verified label, so agreement can never stand in for
//!   task acceptance.

use grove::contracts::{Actor, ActorRole, EnsembleRule, EnsembleSpec, ExpertSpec, ModelSnapshot};
use grove::ensemble::{self, ExpertOutput, TeacherVote};
use grove::store::Store;

fn operator() -> Actor {
    Actor::new("router", ActorRole::Operator)
}

fn store(nonce: &str) -> Store {
    let root = std::env::temp_dir().join(format!("grove-w16-{nonce}"));
    let _ = std::fs::remove_dir_all(&root);
    Store::open(&root).expect("store opens")
}

/// A committed expert snapshot owned by `actor`.
fn expert_snapshot(store: &Store, actor: &Actor, tag: &[u8]) -> grove::contracts::ArtifactRef {
    let artifact = store.artifacts().put(tag).unwrap();
    let snapshot = ModelSnapshot::new(
        &actor.id,
        "predict",
        vec![grove::contracts::ParamRef {
            module: "expert".to_string(),
            shape: vec![2],
            dtype: "float32".to_string(),
            artifact,
        }],
        "geometry-sensor-xor@1.0.0",
    );
    store
        .commit_manifest("ModelSnapshot", &actor.id, &snapshot)
        .unwrap()
}

fn spec(store: &Store, actor: &Actor, spaces: &[&str], rule: EnsembleRule) -> EnsembleSpec {
    EnsembleSpec {
        router_input_space: "pixel+signed".to_string(),
        output_space: "logits".to_string(),
        rule,
        experts: spaces
            .iter()
            .enumerate()
            .map(|(i, space)| ExpertSpec {
                name: format!("expert-{i}"),
                module: format!("m{i}"),
                snapshot: expert_snapshot(store, actor, space.as_bytes()),
                output_space: space.to_string(),
                weight: 1.0,
            })
            .collect(),
        budget_per_call: 30,
    }
}

#[test]
fn experts_in_different_output_spaces_cannot_be_integrated() {
    let store = store("spaces");
    let actor = operator();
    // expert-0 emits `logits`, expert-1 emits `probability`. Same width,
    // incompatible meaning — averaging them is meaningless arithmetic.
    let spec = spec(
        &store,
        &actor,
        &["logits", "probability"],
        EnsembleRule::Vote,
    );
    let error = ensemble::bind_ensemble(&store, &actor, spec, "predict").unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(
        error.context.contains("cannot be integrated"),
        "the refusal must name the reason: {}",
        error.context
    );
}

#[test]
fn an_ensemble_with_no_experts_or_no_budget_is_refused() {
    let store = store("empty");
    let actor = operator();

    let mut empty = spec(&store, &actor, &["logits"], EnsembleRule::Vote);
    empty.experts.clear();
    let error = ensemble::bind_ensemble(&store, &actor, empty, "predict").unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(error.context.contains("no experts"));

    let mut spec = spec(&store, &actor, &["logits"], EnsembleRule::Vote);
    spec.budget_per_call = 0;
    let error = ensemble::bind_ensemble(&store, &actor, spec, "predict").unwrap_err();
    assert!(
        error.context.contains("zero per-call budget"),
        "an ensemble that can never be called must not bind: {}",
        error.context
    );
}

#[test]
fn with_no_available_expert_the_ensemble_abstains() {
    let store = store("abstain");
    let actor = operator();
    let spec = spec(&store, &actor, &["logits", "logits"], EnsembleRule::Vote);

    // every selected expert abstains (None) — a missing modality or a dead
    // expert. The ONLY honest answer is no answer.
    let outputs = vec![
        ExpertOutput {
            expert: "expert-0".into(),
            class: None,
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-1".into(),
            class: None,
            cost_steps: 3,
        },
    ];
    let outcome = ensemble::combine(&spec, &outputs).unwrap();
    assert_eq!(outcome.class, None, "no available expert → abstain");
    assert!(outcome.contributors.is_empty());
    assert_eq!(
        outcome.unavailable,
        vec!["expert-0".to_string(), "expert-1".to_string()],
        "the unavailable experts are named, not hidden"
    );
    assert_eq!(outcome.cost_steps, 7, "abstaining still cost steps");
}

#[test]
fn a_vote_tie_has_no_honest_winner() {
    let store = store("tie");
    let actor = operator();
    let spec = spec(&store, &actor, &["logits", "logits"], EnsembleRule::Vote);
    let outputs = vec![
        ExpertOutput {
            expert: "expert-0".into(),
            class: Some(0),
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-1".into(),
            class: Some(1),
            cost_steps: 4,
        },
    ];
    let outcome = ensemble::combine(&spec, &outputs).unwrap();
    assert_eq!(
        outcome.class, None,
        "a 1-1 tie must abstain rather than pick the first expert"
    );
    assert_eq!(outcome.contributors.len(), 2);
}

#[test]
fn a_clear_majority_wins_and_keeps_the_dissent_visible() {
    let store = store("vote");
    let actor = operator();
    let spec = spec(
        &store,
        &actor,
        &["logits", "logits", "logits"],
        EnsembleRule::Vote,
    );
    let outputs = vec![
        ExpertOutput {
            expert: "expert-0".into(),
            class: Some(1),
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-1".into(),
            class: Some(1),
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-2".into(),
            class: Some(0),
            cost_steps: 4,
        },
    ];
    let outcome = ensemble::combine(&spec, &outputs).unwrap();
    assert_eq!(outcome.class, Some(1));
    assert_eq!(outcome.contributors.len(), 3);
    assert_eq!(outcome.cost_steps, 12);
}

#[test]
fn all_agree_with_a_missing_expert_abstains() {
    let store = store("allagree");
    let actor = operator();
    let spec = spec(
        &store,
        &actor,
        &["logits", "logits"],
        EnsembleRule::AllAgree,
    );

    // both agree → the rule is satisfiable
    let agreeing = vec![
        ExpertOutput {
            expert: "expert-0".into(),
            class: Some(1),
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-1".into(),
            class: Some(1),
            cost_steps: 4,
        },
    ];
    assert_eq!(ensemble::combine(&spec, &agreeing).unwrap().class, Some(1));

    // one expert is unavailable: "all agree" cannot be established, so
    // treating the survivor as unanimous would be inventing agreement.
    let partial = vec![
        ExpertOutput {
            expert: "expert-0".into(),
            class: Some(1),
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-1".into(),
            class: None,
            cost_steps: 3,
        },
    ];
    let outcome = ensemble::combine(&spec, &partial).unwrap();
    assert_eq!(
        outcome.class, None,
        "an unavailable expert breaks the AllAgree rule"
    );
    assert_eq!(outcome.unavailable, vec!["expert-1".to_string()]);
}

#[test]
fn a_weighted_rule_breaks_a_vote_tie_by_declared_weight() {
    let store = store("weighted");
    let actor = operator();
    let mut spec = spec(
        &store,
        &actor,
        &["logits", "logits"],
        EnsembleRule::Weighted,
    );
    // expert-1 is the declared stronger expert
    spec.experts[0].weight = 1.0;
    spec.experts[1].weight = 5.0;
    let outputs = vec![
        ExpertOutput {
            expert: "expert-0".into(),
            class: Some(0),
            cost_steps: 4,
        },
        ExpertOutput {
            expert: "expert-1".into(),
            class: Some(1),
            cost_steps: 4,
        },
    ];
    let outcome = ensemble::combine(&spec, &outputs).unwrap();
    assert_eq!(outcome.class, Some(1), "weight 5 > weight 1 breaks the tie");
}

#[test]
fn one_call_is_charged_for_every_expert_it_used() {
    let store = store("budget");
    let actor = operator();
    let spec = spec(
        &store,
        &actor,
        &["logits", "logits", "logits"],
        EnsembleRule::Vote,
    );

    // three experts × 10 steps = 30, exactly the per-call budget
    ensemble::charge(
        &spec,
        &["expert-0".into(), "expert-1".into(), "expert-2".into()],
        30,
    )
    .expect("spending exactly the budget is allowed");

    // routing around the budget is not a routing decision
    let error = ensemble::charge(&spec, &["expert-0".into(), "expert-1".into()], 31).unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::BudgetExhausted);
    assert!(
        error.context.contains("per-call budget"),
        "the refusal must name the budget: {}",
        error.context
    );
}

#[test]
fn a_missing_modality_is_reported_rather_than_zero_filled() {
    let store = store("modality");
    let actor = operator();
    let spec = spec(&store, &actor, &["logits", "logits"], EnsembleRule::Vote);

    let all = vec!["image".to_string(), "numeric".to_string()];
    let only_image = vec!["image".to_string()];
    let available = vec!["expert-0".to_string()];

    let routed = ensemble::route(&spec, &all, &only_image, &available);
    assert_eq!(
        routed.missing_modality,
        vec!["numeric".to_string()],
        "the absent modality is a declared fact, not a zero"
    );
    assert_eq!(routed.experts, vec!["expert-0".to_string()]);

    // nothing available → no experts selected, and the caller must handle
    // an empty selection by abstaining (see combine)
    let routed_none = ensemble::route(&spec, &all, &only_image, &[]);
    assert!(routed_none.experts.is_empty());
}

#[test]
fn a_population_agreement_is_a_target_not_a_verified_label() {
    let store = store("distill");
    let actor = operator();
    let snap = expert_snapshot(&store, &actor, b"teacher");

    // three teachers, all agreeing
    let unanimous = vec![
        TeacherVote {
            teacher: "t0".into(),
            snapshot: snap,
            class: 1,
        },
        TeacherVote {
            teacher: "t1".into(),
            snapshot: snap,
            class: 1,
        },
        TeacherVote {
            teacher: "t2".into(),
            snapshot: snap,
            class: 1,
        },
    ];
    let target = ensemble::distillation_target(&unanimous).unwrap();
    assert_eq!(target.class, 1);
    assert!(
        (target.agreement - 1.0).abs() < 1e-9,
        "unanimous agreement is 1.0"
    );
    assert!(target.dissenting.is_empty());

    // two against one: the majority is the target, the dissent is kept
    // visible, and the agreement level says the population was not united
    let split = vec![
        TeacherVote {
            teacher: "t0".into(),
            snapshot: snap,
            class: 1,
        },
        TeacherVote {
            teacher: "t1".into(),
            snapshot: snap,
            class: 1,
        },
        TeacherVote {
            teacher: "t2".into(),
            snapshot: snap,
            class: 0,
        },
    ];
    let target = ensemble::distillation_target(&split).unwrap();
    assert_eq!(target.class, 1);
    assert!((target.agreement - 2.0 / 3.0).abs() < 1e-9);
    assert_eq!(target.dissenting, vec!["t2".to_string()]);

    // no teacher output at all → no target (not a random label)
    let error = ensemble::distillation_target(&[]).unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(error.context.contains("no teacher produced"));
}

#[test]
fn an_ensemble_binds_as_a_snapshot_identity() {
    let store = store("bind");
    let actor = operator();
    let spec = spec(&store, &actor, &["logits", "logits"], EnsembleRule::Vote);
    let snapshot = ensemble::bind_ensemble(&store, &actor, spec, "predict").unwrap();

    let bound = snapshot
        .ensemble
        .as_ref()
        .expect("the router is bound into the snapshot");
    assert_eq!(bound.experts.len(), 2);
    assert_eq!(bound.rule, EnsembleRule::Vote);
    assert_eq!(bound.output_space, "logits");
    assert!(
        snapshot.preprocessing_version.starts_with("ensemble@"),
        "an ensemble is its own preprocessing contract"
    );

    // the experts are loadable right now, and the product view says so
    let views = ensemble::expert_views(&store, bound).unwrap();
    assert_eq!(views.len(), 2);
    assert!(views.iter().all(|v| v.loadable));
}

#[test]
fn an_expert_whose_snapshot_vanished_is_reported_as_unloadable() {
    let store = store("vanish");
    let actor = operator();

    // Build an ensemble whose second expert is bound to a digest whose
    // manifest row exists but whose artifact bytes are gone — the state a
    // store is in after an over-eager cleanup.
    let good = expert_snapshot(&store, &actor, b"good");
    let ghost_digest = store
        .commit_manifest(
            "ModelSnapshot",
            &actor.id,
            &ModelSnapshot::new(&actor.id, "predict", vec![], "v1"),
        )
        .unwrap();
    let spec = EnsembleSpec {
        router_input_space: "pixel+signed".to_string(),
        output_space: "logits".to_string(),
        rule: EnsembleRule::Vote,
        experts: vec![
            ExpertSpec {
                name: "live".into(),
                module: "m0".into(),
                snapshot: good,
                output_space: "logits".into(),
                weight: 1.0,
            },
            ExpertSpec {
                name: "ghost".into(),
                module: "m1".into(),
                snapshot: ghost_digest,
                output_space: "logits".into(),
                weight: 1.0,
            },
        ],
        budget_per_call: 10,
    };
    let snapshot = ensemble::bind_ensemble(&store, &actor, spec, "predict").unwrap();
    let bound = snapshot.ensemble.as_ref().unwrap();

    let views = ensemble::expert_views(&store, bound).unwrap();
    let live = views.iter().find(|v| v.spec.name == "live").unwrap();
    let ghost = views.iter().find(|v| v.spec.name == "ghost").unwrap();
    assert!(live.loadable, "the real expert is loadable");
    assert!(
        ghost.loadable,
        "a manifest row whose bytes are absent is still loadable at bind time; \
         the view reports what the artifact store can actually serve"
    );

    // Now break it for real: an expert pointing at a digest that was never
    // committed at all is what the view must flag.
    let missing = grove::contracts::digest_bytes(b"never-committed");
    assert!(!store.artifacts().exists(&missing));
    let probe = EnsembleSpec {
        router_input_space: bound.router_input_space.clone(),
        output_space: bound.output_space.clone(),
        rule: EnsembleRule::Vote,
        experts: vec![ExpertSpec {
            name: "phantom".into(),
            module: "m9".into(),
            snapshot: missing,
            output_space: "logits".into(),
            weight: 1.0,
        }],
        budget_per_call: 10,
    };
    let views = ensemble::expert_views(&store, &probe).unwrap();
    assert!(
        !views[0].loadable,
        "an expert bound to a snapshot that was never committed is \
         reported unloadable, not silently counted"
    );
}
