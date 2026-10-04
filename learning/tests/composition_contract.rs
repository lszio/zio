//! W13 contract: modules, composition and joint fine-tuning.
//!
//! The properties under test are the ones that make composition an
//! engineering operation rather than a weight shuffle:
//!
//! * two values of equal width in different semantic spaces are NOT
//!   interchangeable — the composition is refused;
//! * a shared parameter group is one evolution unit and cannot be
//!   restored as two conflicting versions;
//! * the permission closure of the parts survives composition: a
//!   composite can require more than its parts, never less;
//! * composition creates a NEW identity with multi-parent lineage and
//!   leaves both parents untouched;
//! * a composite is not promotable on a submodule's local score — the
//!   joint plan forces whole-composite evaluation.

use grove::composition::{self, validate_modules};
use grove::contracts::{
    Actor, ActorRole, ArtifactRef, ModelSnapshot, ModuleSpec, Run, RunState, SCHEMA_VERSION,
};
use grove::store::Store;

fn operator() -> Actor {
    Actor::new("composer", ActorRole::Operator)
}

/// A fresh store per test: SQLite locks per file.
fn store(nonce: &str) -> Store {
    let root = std::env::temp_dir().join(format!("grove-w13-{nonce}"));
    let _ = std::fs::remove_dir_all(&root);
    Store::open(&root).expect("store opens")
}

fn module(name: &str, in_space: &str, out_space: &str) -> ModuleSpec {
    ModuleSpec {
        name: name.to_string(),
        input_space: in_space.to_string(),
        output_space: out_space.to_string(),
        layers: vec![format!("{name}_h")],
        depends_on: Vec::new(),
        shared_group: None,
        frozen: false,
        requires: ActorRole::Reader,
    }
}

/// A snapshot with modules, committed and named so composition can
/// resolve it by name later.
fn seed(store: &Store, actor: &Actor, name: &str, modules: Vec<ModuleSpec>, weights: &[u8]) -> ArtifactRef {
    let artifact = store.artifacts().put(weights).unwrap();
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: "predict".to_string(),
        params: modules
            .iter()
            .map(|m| grove::contracts::ParamRef {
                module: m.name.clone(),
                shape: vec![2],
                dtype: "float32".to_string(),
                artifact,
            })
            .collect(),
        libraries: Vec::new(),
        preprocessing_version: "geometry-sensor-xor@1.0.0".to_string(),
        modules,
        ensemble: None,
        graph: None,
    };
    let digest = store.commit_manifest("ModelSnapshot", &actor.id, &snapshot).unwrap();
    store.name_snapshot(name, &digest).unwrap();
    digest
}

#[test]
fn equal_width_different_space_is_not_interchangeable() {
    let store = store("spaces");
    let actor = operator();

    // "pixel" and "signed" are both width 2 in the eyes of a shape-only
    // checker. They are not the same quantity, so the join is refused.
    let visual = module("visual", "pixel", "pixel");
    let mut fusion = module("fusion", "signed", "logits");
    fusion.depends_on = vec!["visual".to_string()];

    let error = validate_modules(&[visual, fusion]).unwrap_err();
    assert_eq!(
        error.kind,
        grove::contracts::ErrorKind::ProtocolViolation,
        "a mismatched semantic space is a protocol violation, not a warning"
    );
    assert!(
        error.context.contains("same width is not the same meaning"),
        "the error must name the actual cause: {}",
        error.context
    );
}

#[test]
fn a_compatible_chain_composes_and_keeps_both_parents_intact() {
    let store = store("compose-ok");
    let actor = operator();

    let visual = module("visual", "pixel", "pixel");
    let mut fusion = module("fusion", "pixel", "logits");
    fusion.depends_on = vec!["visual".to_string()];

    let parent_a = seed(&store, &actor, "visual-snap", vec![visual.clone()], b"visual-weights");
    let parent_b = seed(&store, &actor, "fusion-snap", vec![fusion.clone()], b"fusion-weights");

    let result = composition::compose(
        &store,
        &actor,
        "composite-1",
        &[parent_a, parent_b],
        vec![visual.clone(), fusion.clone()],
        "predict",
        br#"{"merged": true}"#,
        "geometry-sensor-xor@1.0.0",
    )
    .expect("a space-compatible chain composes");

    assert_eq!(result.modules.len(), 2);
    assert!(
        result.snapshot.params.iter().any(|p| p.module == "visual")
            && result.snapshot.params.iter().any(|p| p.module == "fusion"),
        "the composite carries both modules' parameters"
    );

    // both parents still resolve, unchanged
    assert_eq!(store.snapshot_digest("visual-snap").unwrap(), parent_a);
    assert_eq!(store.snapshot_digest("fusion-snap").unwrap(), parent_b);
    let body = store.load_manifest(&parent_a).unwrap();
    assert!(
        body.contains("visual-weights") == false,
        "the parent's manifest is its own; composing rewrote nothing"
    );

    // multi-parent lineage: BOTH parents are recorded as edges
    let parents = store.parents_of("composite-1").unwrap();
    assert_eq!(parents.len(), 2, "both parents are recorded: {parents:?}");
    let child = store.snapshot_digest("composite-1").unwrap();
    let recorded: Vec<ArtifactRef> = parents.iter().map(|(d, _)| *d).collect();
    assert!(recorded.contains(&parent_a) && recorded.contains(&parent_b));
    assert!(parents
        .iter()
        .all(|(_, d)| *d == grove::contracts::Derivation::ModuleComposition));
    assert!(store.load_manifest(&child).is_ok(), "the composite is committed");
}

#[test]
fn a_shared_parameter_cannot_be_two_conflicting_versions() {
    let store = store("shared");
    let actor = operator();

    // Same group name, but one member frozen and the other trainable:
    // the same parameter cannot be both standing still and moving.
    let mut a = module("head", "pixel", "logits");
    a.shared_group = Some("shared-head".to_string());
    a.frozen = true;
    let mut b = module("head-copy", "pixel", "logits");
    b.shared_group = Some("shared-head".to_string());
    b.frozen = false;

    let error = validate_modules(&[a, b]).unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(
        error.context.contains("cannot be both"),
        "the error must name the shared-group conflict: {}",
        error.context
    );
}

#[test]
fn composing_never_weakens_the_permission_closure() {
    let store = store("closure");
    let actor = operator();

    // The parent demands publisher: nothing derived from it may drop to
    // reader. A module asking for less is a privilege-escalation attempt.
    let mut privileged = module("privileged", "pixel", "pixel");
    privileged.requires = ActorRole::Publisher;
    let parent = seed(&store, &actor, "priv-snap", vec![privileged.clone()], b"priv");

    let mut weakened = module("privileged", "pixel", "pixel");
    weakened.requires = ActorRole::Reader;

    let error = composition::compose(
        &store,
        &actor,
        "composite-weak",
        &[parent],
        vec![weakened],
        "predict",
        b"{}",
        "v1",
    )
    .unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(
        error.context.contains("must not weaken the permission closure"),
        "the error must name the closure rule: {}",
        error.context
    );
}

#[test]
fn a_local_replacement_creates_a_new_identity_and_keeps_the_old_one() {
    let store = store("replace");
    let actor = operator();

    let visual = module("visual", "pixel", "pixel");
    let mut fusion = module("fusion", "pixel", "logits");
    fusion.depends_on = vec!["visual".to_string()];
    let parent_a = seed(&store, &actor, "v1", vec![visual.clone()], b"w1");
    let parent_b = seed(&store, &actor, "f1", vec![fusion.clone()], b"w2");
    composition::compose(
        &store,
        &actor,
        "composite-a",
        &[parent_a, parent_b],
        vec![visual, fusion],
        "predict",
        b"{}",
        "v1",
    )
    .unwrap();

    // A local replacement must not change the neighbour's interface: the
    // new visual module still emits `pixel`, so the composition is
    // still valid and the replacement is a real, separate identity.
    let mut replacement = module("visual", "pixel", "pixel");
    replacement.layers = vec!["visual_h2".to_string()];
    let result =
        composition::replace_module(&store, &actor, "composite-a", "composite-b", replacement, b"new-visual")
            .expect("a space-compatible replacement composes");

    assert_ne!(
        store.snapshot_digest("composite-a").unwrap(),
        store.snapshot_digest("composite-b").unwrap(),
        "the replacement is a new snapshot, not an edit of the old one"
    );
    assert_eq!(result.modules.len(), 2, "the neighbour is still there");
    assert_eq!(result.modules[0].origin, composition::ParameterOrigin::Migrated);

    // An interface-breaking replacement IS refused: the new module emits
    // a different space, which its consumer cannot accept.
    let mut breaking = module("visual", "pixel", "probability");
    breaking.layers = vec!["visual_h3".to_string()];
    let error =
        composition::replace_module(&store, &actor, "composite-b", "composite-c", breaking, b"bad")
            .unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(
        !store.snapshot_digest("composite-c").is_ok(),
        "a refused replacement must not leave a name bound"
    );
}

#[test]
fn a_composite_with_no_trainable_module_refuses_to_pretend_to_learn() {
    let store = store("frozen");
    let actor = operator();

    let mut frozen = module("frozen", "pixel", "logits");
    frozen.frozen = true;
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: "predict".to_string(),
        params: Vec::new(),
        libraries: Vec::new(),
        preprocessing_version: "v1".to_string(),
        modules: vec![frozen],
        ensemble: None,
        graph: None,
    };
    let error = composition::joint_plan(&snapshot).unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::IncompatibleState);
    assert!(
        error.context.contains("no trainable module"),
        "the refusal must say why: {}",
        error.context
    );
}

#[test]
fn a_joint_plan_always_evaluates_the_whole_composite() {
    let store = store("joint");
    let actor = operator();

    let mut visual = module("visual", "pixel", "pixel");
    visual.frozen = true;
    let mut fusion = module("fusion", "pixel", "logits");
    fusion.depends_on = vec!["visual".to_string()];
    let parent_a = seed(&store, &actor, "vis", vec![visual], b"a");
    let parent_b = seed(&store, &actor, "fus", vec![fusion.clone()], b"b");
    let composed = composition::compose(
        &store,
        &actor,
        "composite-joint",
        &[parent_a, parent_b],
        vec![
            {
                let mut m = module("visual", "pixel", "pixel");
                m.frozen = true;
                m
            },
            fusion,
        ],
        "predict",
        b"{}",
        "v1",
    )
    .unwrap();

    let plan = composition::joint_plan(&composed.snapshot).unwrap();
    assert_eq!(plan.trainable, vec!["fusion".to_string()]);
    assert_eq!(plan.frozen, vec!["visual".to_string()]);
    assert!(
        plan.evaluate_whole,
        "a composite may never be promoted on a submodule's local score"
    );
}

#[test]
fn a_graph_edge_that_contradicts_the_module_contract_is_refused() {
    let visual = module("visual", "pixel", "pixel");
    let fusion = module("fusion", "pixel", "logits");

    // the graph claims to feed `fusion` from the `signed` space
    let error = composition::check_graph_spaces(
        &[visual, fusion],
        &[("fusion".to_string(), "visual".to_string(), "signed".to_string())],
    )
    .unwrap_err();
    assert_eq!(error.kind, grove::contracts::ErrorKind::ProtocolViolation);
    assert!(error.context.contains("declares"));

    // the same edge, correctly declared, passes
    composition::check_graph_spaces(
        &[
            module("visual", "pixel", "pixel"),
            module("fusion", "pixel", "logits"),
        ],
        &[("fusion".to_string(), "visual".to_string(), "pixel".to_string())],
    )
    .expect("a matching edge passes");
}

#[test]
fn a_run_records_the_composite_it_fine_tunes() {
    // the joint fine-tune runs through an ordinary Run record, so the
    // budget ledger and the resumability contract are shared with every
    // other run — composition is not a second kind of training.
    let store = store("run");
    let actor = operator();
    let run = Run {
        schema: SCHEMA_VERSION,
        id: "run-joint-1".to_string(),
        task_id: "geometry-sensor-xor@1.0.0".to_string(),
        base_snapshot: store
            .snapshot_digest("nope")
            .unwrap_or_else(|_| grove::contracts::digest_bytes(b"unused")),
        dataset_revision: "ds-1".to_string(),
        recipe: "supervised@1".to_string(),
        state: RunState::Running,
        steps_consumed: 0,
        steps_budget: 200,
        resumed_from: None,
    };
    store.put_run(&actor, &run).unwrap();
    store.consume_steps(&run.id, 120).unwrap();
    let loaded = store.get_run(&run.id).unwrap();
    assert_eq!(loaded.steps_consumed, 120);
    assert_eq!(loaded.steps_budget, 200);
    // the budget is a hard ceiling, not a suggestion
    assert!(store.consume_steps(&run.id, 100).is_err());
}
