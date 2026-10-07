//! W13: modules, composition and joint fine-tuning.
//!
//! The rule this module defends is that **composition is a new model, not
//! an edit of two old ones**. Building one from module snapshots:
//!
//! * refuses to join values whose semantic spaces disagree — equal width
//!   is not equal meaning, and a lucky pairing is a coincidence the next
//!   data split will break;
//! * refuses to resolve a shared group into two versions — a shared
//!   parameter is ONE evolution unit and can be restored only as one;
//! * carries each module's permission closure forward: a composition is
//!   not allowed to require *less* than its parts, so composing can
//!   never quietly drop a capability check;
//! * leaves both parents' records and artifacts byte-for-byte untouched,
//!   and records a multi-parent derivation for the new snapshot.
//!
//! Joint fine-tuning then optimizes every non-frozen module of the
//! composition in one optimiser step. A frozen module is context: its
//! parameters are carried, not moved, and the store refuses to claim the
//! composite is a locally-improved candidate just because one submodule
//! scored well — the composite is evaluated whole.

use std::path::Path;

use crate::contracts::{
    Actor, ActorRole, ArtifactRef, Derivation, Error, ErrorKind, ModelSnapshot, ModuleSpec,
    ParamRef, Result, SCHEMA_VERSION,
};
use crate::store::Store;

/// How a child module's parameters were obtained from its parents'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterOrigin {
    /// Carried unchanged from one parent.
    Inherited,
    /// Weights migrated from a parent with a compatible shape.
    Migrated,
    /// Freshly initialized (the module is new, or its shape changed).
    Reinitialized,
    /// Moved by joint fine-tuning.
    JointTuned,
}

impl ParameterOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherited => "inherited",
            Self::Migrated => "migrated",
            Self::Reinitialized => "reinitialized",
            Self::JointTuned => "joint-tuned",
        }
    }
}

/// One module of a composed snapshot, with its provenance and the
/// permission it demands.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedModule {
    pub spec: ModuleSpec,
    pub parents: Vec<ArtifactRef>,
    pub origin: ParameterOrigin,
}

/// The whole composition decision, reviewable before anything is trained.
#[derive(Debug, Clone, PartialEq)]
pub struct Composition {
    pub modules: Vec<ComposedModule>,
    /// The snapshot id the new parameters are committed under. The
    /// parents keep theirs.
    pub composite_id: String,
    pub derivation: Derivation,
}

/// The result of building a composition: the new snapshot plus the
/// parameter artifact that carries every module's weights.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositionResult {
    pub snapshot: ModelSnapshot,
    pub params_digest: ArtifactRef,
    pub modules: Vec<ComposedModule>,
}

/// Load a committed snapshot manifest, refusing one whose owner does not
/// match the composing actor — composing someone else's module is still
/// a cross-actor reference.
fn load_snapshot(store: &Store, actor: &Actor, digest: &ArtifactRef) -> Result<ModelSnapshot> {
    store.require_owner(digest, actor)?;
    let body = store.load_manifest(digest)?;
    let snapshot: ModelSnapshot = serde_json::from_str(&body).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("snapshot manifest {} does not parse: {e}", digest.to_hex()),
        )
    })?;
    if snapshot.schema != SCHEMA_VERSION {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!("snapshot schema {} is not supported", snapshot.schema),
        ));
    }
    Ok(snapshot)
}

/// Validate a module list in isolation. This is the check the tensor-graph
/// compiler and the product UI both rely on, so it is the same function
/// for all three rather than three drifting copies.
pub fn validate_modules(modules: &[ModuleSpec]) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    for module in modules {
        if module.name.is_empty() {
            return Err(Error::invalid("a module must have a name"));
        }
        if seen.contains(&module.name.as_str()) {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!("module {:?} is declared twice", module.name),
            ));
        }
        seen.push(&module.name);
        if module.input_space.is_empty() || module.output_space.is_empty() {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "module {:?} does not declare its input/output semantic space",
                    module.name
                ),
            ));
        }
    }
    for module in modules {
        for dep in &module.depends_on {
            if !seen.contains(&dep.as_str()) {
                return Err(Error::new(
                    ErrorKind::ProtocolViolation,
                    format!("module {:?} depends on unknown module {dep:?}", module.name),
                ));
            }
            // The edge is checked HERE, not at each call site: the product
            // UI, the tensor-graph compiler and `compose` all validate
            // through this one function, so a mismatched space cannot
            // slip past a caller that forgot to look.
            let producer = modules
                .iter()
                .find(|m| &m.name == dep)
                .expect("the dependency's existence was just proved");
            require_compatible(module, producer)?;
        }
    }
    // A shared group must be ONE unit: every member of a group shares a
    // parameter identity, so two members declaring different frozen
    // states would mean the same parameter is both moving and standing
    // still.
    let mut groups: Vec<(&str, bool)> = Vec::new();
    for module in modules {
        if let Some(group) = &module.shared_group {
            match groups.iter_mut().find(|(name, _)| *name == group.as_str()) {
                Some((_, frozen)) => {
                    if *frozen != module.frozen {
                        return Err(Error::new(
                            ErrorKind::ProtocolViolation,
                            format!(
                                "shared group {group:?} mixes a frozen and a trainable member; \
                                 a shared parameter cannot be both"
                            ),
                        ));
                    }
                }
                None => groups.push((group.as_str(), module.frozen)),
            }
        }
    }
    Ok(())
}

/// Join two module values by semantic space, not by width. Returns the
/// error a composition must surface when the spaces disagree.
pub fn require_compatible(consumer: &ModuleSpec, producer: &ModuleSpec) -> Result<()> {
    if consumer.input_space != producer.output_space {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            format!(
                "module {:?} consumes space {:?} but module {:?} emits {:?}; \
                 same width is not the same meaning",
                consumer.name, consumer.input_space, producer.name, producer.output_space
            ),
        ));
    }
    Ok(())
}

/// Compose module snapshots into one new snapshot.
///
/// `params_artifact` is a plain JSON artifact the host wrote (the tensor
/// compiler owns its contents); the composition validates the *contract*
/// and commits the identity. Parents are never modified.
pub fn compose(
    store: &Store,
    actor: &Actor,
    composite_id: &str,
    parents: &[ArtifactRef],
    modules: Vec<ModuleSpec>,
    entrypoint: &str,
    params_bytes: &[u8],
    preprocessing_version: &str,
) -> Result<CompositionResult> {
    actor.require(ActorRole::Operator, "composing modules")?;
    if parents.is_empty() {
        return Err(Error::invalid(
            "a composition needs at least one parent snapshot",
        ));
    }
    validate_modules(&modules)?;

    // The permission closure is a MAXIMUM, never a minimum: composing may
    // require more, never less. Taking the max of the parents' declared
    // roles keeps the strongest check alive.
    let mut closure = ActorRole::Reader;
    let mut carried: Vec<ParamRef> = Vec::new();
    for parent_digest in parents {
        let parent = load_snapshot(store, actor, parent_digest)?;
        for spec in &parent.modules {
            if spec.requires.rank() > closure.rank() {
                closure = spec.requires;
            }
        }
        for param in parent.params {
            // a carried parameter keeps its name; a composed module may
            // not silently adopt a name another module already owns
            if carried.iter().any(|p| p.module == param.module) {
                continue;
            }
            carried.push(param);
        }
    }
    for module in &modules {
        if module.requires.rank() < closure.rank() {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "module {:?} requires {:?} but the composition inherits {:?}; \
                     composing must not weaken the permission closure",
                    module.name, module.requires, closure
                ),
            ));
        }
    }

    let params_digest = store.artifacts().put(params_bytes)?;
    let mut params = carried;
    for module in &modules {
        if !params.iter().any(|p| p.module == module.name) {
            params.push(ParamRef {
                module: module.name.clone(),
                shape: Vec::new(),
                dtype: "float32".to_string(),
                artifact: params_digest,
            });
        }
    }

    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: entrypoint.to_string(),
        params,
        libraries: Vec::new(),
        preprocessing_version: preprocessing_version.to_string(),
        modules,
        ensemble: None,
        graph: None,
    };
    // Multi-parent lineage: the new snapshot records every parent it came
    // from, so history keeps the whole story. The name is bound to the
    // digest it resolved to — the name is a handle, the digest is the
    // identity, so a later `replace_module` resolves the same bytes.
    let digest = store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)?;
    store.name_snapshot(composite_id, &digest)?;
    store.record_derivation(
        actor,
        composite_id,
        parents.to_vec(),
        Derivation::ModuleComposition,
    )?;

    let modules = snapshot
        .modules
        .iter()
        .map(|spec| ComposedModule {
            spec: spec.clone(),
            parents: parents.to_vec(),
            origin: ParameterOrigin::Inherited,
        })
        .collect();
    Ok(CompositionResult {
        snapshot,
        params_digest,
        modules,
    })
}

/// Replace one module of a composition, leaving the rest alone. The
/// result is a NEW snapshot with a new identity; the composite it came
/// from is untouched and still loadable.
pub fn replace_module(
    store: &Store,
    actor: &Actor,
    composite_id: &str,
    new_composite_id: &str,
    module: ModuleSpec,
    new_params_bytes: &[u8],
) -> Result<CompositionResult> {
    actor.require(ActorRole::Operator, "replacing a module")?;
    let parent_digest = digest_of(store, composite_id)?;
    let parent = load_snapshot(store, actor, &parent_digest)?;
    let mut modules = parent.modules.clone();
    let Some(slot) = modules.iter_mut().find(|m| m.name == module.name) else {
        return Err(Error::new(
            ErrorKind::ArtifactUnavailable,
            format!("composite {composite_id} has no module {:?}", module.name),
        ));
    };
    *slot = module.clone();
    // A replaced module's interface is a contract with its neighbours: if
    // the new module's output space changed, every consumer must be
    // re-checked, and the caller is told rather than left to discover it
    // at run time.
    validate_modules(&modules)?;
    let params_digest = store.artifacts().put(new_params_bytes)?;
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: parent.entrypoint.clone(),
        params: vec![ParamRef {
            module: module.name,
            shape: Vec::new(),
            dtype: "float32".to_string(),
            artifact: params_digest,
        }],
        libraries: parent.libraries.clone(),
        preprocessing_version: parent.preprocessing_version.clone(),
        modules,
        ensemble: None,
        graph: None,
    };
    let digest = store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)?;
    store.name_snapshot(new_composite_id, &digest)?;
    store.record_derivation(
        actor,
        new_composite_id,
        vec![parent_digest],
        Derivation::WeightMigration,
    )?;
    let modules = snapshot
        .modules
        .iter()
        .map(|spec| ComposedModule {
            spec: spec.clone(),
            parents: vec![parent_digest],
            origin: ParameterOrigin::Migrated,
        })
        .collect();
    Ok(CompositionResult {
        snapshot,
        params_digest,
        modules,
    })
}

fn digest_of(store: &Store, composite_id: &str) -> Result<ArtifactRef> {
    store.snapshot_digest(composite_id)
}

/// A joint fine-tuning plan: which modules move, which are frozen
/// context, and what the single evaluation must cover.
#[derive(Debug, Clone, PartialEq)]
pub struct JointPlan {
    pub trainable: Vec<String>,
    pub frozen: Vec<String>,
    /// Modules that must be evaluated *together* afterwards. A composite
    /// is never promotable on a submodule's local score.
    pub evaluate_whole: bool,
}

/// Plan a joint fine-tune over a composed snapshot.
pub fn joint_plan(snapshot: &ModelSnapshot) -> Result<JointPlan> {
    validate_modules(&snapshot.modules)?;
    let mut trainable = Vec::new();
    let mut frozen = Vec::new();
    for module in &snapshot.modules {
        if module.frozen {
            frozen.push(module.name.clone());
        } else {
            trainable.push(module.name.clone());
        }
    }
    if trainable.is_empty() {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "every module is frozen: a joint fine-tune with no trainable module \
             would be a no-op pretending to be learning",
        ));
    }
    Ok(JointPlan {
        trainable,
        frozen,
        evaluate_whole: true,
    })
}

/// Persist the joint fine-tune's parameters and hand back the snapshot
/// identity it produced. The composite's frozen members' bytes are
/// verified, not assumed: the host reads both sides and refuses a result
/// whose frozen parameters moved.
pub fn commit_joint(
    store: &Store,
    actor: &Actor,
    composite_id: &str,
    new_composite_id: &str,
    params_bytes: &[u8],
    val_accuracy: f64,
) -> Result<ArtifactRef> {
    actor.require(ActorRole::Operator, "committing a joint fine-tune")?;
    let parent = load_snapshot(store, actor, &digest_of(store, composite_id)?)?;
    let plan = joint_plan(&parent)?;
    crate::contracts::require_finite_metrics(&[("accuracy".to_string(), val_accuracy)])?;
    // The frozen check runs on the proposed bytes, before anything lands.
    // "The caller says it wrote the frozen state plus the updated layers"
    // is not evidence: both sides are read back and compared.
    let proposed: serde_json::Value = serde_json::from_slice(params_bytes).map_err(|e| {
        Error::new(
            ErrorKind::ProtocolViolation,
            format!("joint parameter artifact is not JSON: {e}"),
        )
    })?;
    for name in &plan.frozen {
        let after = proposed.get(name).ok_or_else(|| {
            Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "the joint result does not carry frozen module {name:?}; \
                     a composite cannot drop a frozen member"
                ),
            )
        })?;
        // The parent's frozen bytes are the artifact its ParamRef names.
        // Both sides are canonicalized to a comparable value first, so a
        // difference in whitespace or key order is not mistaken for a
        // change, and a real change is not hidden by reformatting.
        let Some(before_ref) = parent.params.iter().find(|p| p.module == *name) else {
            continue;
        };
        let before_bytes = store.artifacts().get(&before_ref.artifact)?;
        let before: serde_json::Value = serde_json::from_slice(&before_bytes).map_err(|e| {
            Error::new(
                ErrorKind::ProtocolViolation,
                format!("frozen module {name:?} is not JSON in the parent: {e}"),
            )
        })?;
        // The artifact may hold one module or a map of them; compare the
        // slice that names this module.
        let before_module = before.get(name).cloned().unwrap_or_else(|| before.clone());
        if &before_module != after {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "frozen module {name:?} changed during a joint fine-tune \
                     (was {before_module}, now {after}); a frozen member is a promise, not a suggestion"
                ),
            ));
        }
    }
    let params_digest = store.artifacts().put(params_bytes)?;
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: parent.entrypoint.clone(),
        params: vec![ParamRef {
            module: "joint".to_string(),
            shape: Vec::new(),
            dtype: "float32".to_string(),
            artifact: params_digest,
        }],
        libraries: parent.libraries.clone(),
        preprocessing_version: parent.preprocessing_version.clone(),
        modules: parent.modules.clone(),
        ensemble: None,
        graph: None,
    };
    let digest = store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)?;
    store.name_snapshot(new_composite_id, &digest)?;
    store.record_derivation(
        actor,
        new_composite_id,
        vec![digest_of(store, composite_id)?],
        Derivation::ParameterTraining,
    )?;
    Ok(params_digest)
}

/// Read a worker's composite graph description and check it against the
/// module contract before any tensor is touched. Same rule as the worker's
/// own validator, one level up: a graph that joins mismatched spaces is
/// refused here, so a bad composition never reaches execution.
pub fn check_graph_spaces(
    modules: &[ModuleSpec],
    edges: &[(String, String, String)],
) -> Result<()> {
    validate_modules(modules)?;
    for (consumer, input, space) in edges {
        let Some(module) = modules.iter().find(|m| &m.name == consumer) else {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!("graph edge names unknown module {consumer:?}"),
            ));
        };
        if &module.input_space != space {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "graph feeds module {consumer:?} from space {space:?} but it declares {:?}",
                    module.input_space
                ),
            ));
        }
        if !input.is_empty() && !modules.iter().any(|m| &m.name == input) {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!("graph edge reads unknown module {input:?}"),
            ));
        }
    }
    Ok(())
}

/// Read the composite state the worker wrote and commit it as a
/// checkpoint, so a joint fine-tune is resumable like any other run.
pub fn commit_joint_checkpoint(
    store: &Store,
    actor: &Actor,
    run_id: &str,
    state_path: &Path,
    now_ms: i64,
) -> Result<crate::contracts::Checkpoint> {
    crate::checkpoint::commit(
        store,
        actor,
        run_id,
        None,
        crate::contracts::ResumeLevel::LearningContinuation,
        state_path,
        store.get_run(run_id)?.steps_consumed,
        now_ms,
    )
}
