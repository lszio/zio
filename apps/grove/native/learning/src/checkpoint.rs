//! W06: the checkpoint boundary.
//!
//! Three contracts live here:
//!
//! 1. **Commit** — a checkpoint enters history only after its state
//!    artifact is durable *and* self-describing (schema, protocol, run).
//!    A state file the worker wrote atomically, but with an unknown
//!    schema, is refused: resuming blind is how training silently forks
//!    into something else.
//! 2. **Resume** — a checkpoint declares what it can restore. A resume
//!    creates a *new* run; the parent record and its budget ledger stay
//!    exactly as they were. Learning-continuation inherits spent steps;
//!    model-initialization starts a fresh ledger by explicit choice;
//!    controlled replay needs a declared deterministic host — without
//!    one the host refuses rather than promising replay it cannot keep.
//! 3. **Pause and fork** — pause marks the run paused only after the
//!    checkpoint is committed; fork creates a new branch pointing at the
//!    parent checkpoint and never touches the parent's weights or
//!    record.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::contracts::{
    Actor, ActorRole, Branch, Checkpoint, Derivation, Error, ErrorKind, ResumeLevel, Result, Run,
    RunState, SCHEMA_VERSION,
};
use crate::store::Store;

/// The schema the worker's state artifacts carry today.
pub const STATE_PROTOCOL: &str = "grove.worker.state/1";
/// Schema 2 records the trainable set alongside the optimizer moments, so
/// a resume can tell that the artifact describes *this* attempt's
/// parameters. Schema 1 predates that and is history-only.
pub const STATE_SCHEMA: u32 = 2;
/// The oldest schema still readable as history.
pub const LEGACY_STATE_SCHEMA: u32 = 1;

/// The self-description inside a worker state artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateManifest {
    pub schema: u32,
    pub protocol: String,
    pub run_id: String,
    #[serde(default)]
    pub attempt_id: String,
    pub step: u32,
    #[serde(default)]
    pub first_loss: Option<f64>,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub optimizer: serde_json::Value,
    #[serde(default)]
    pub rng: serde_json::Value,
}

impl StateManifest {
    /// The optimizer parameter keys this state carries, e.g. `h0.weight`.
    fn optimizer_keys(&self) -> Vec<String> {
        self.optimizer
            .get("adam")
            .and_then(|a| a.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn has(&self, field: &str) -> bool {
        match field {
            "params" => !self.params.is_null(),
            "optimizer" => !self.optimizer.is_null(),
            "rng" => !self.rng.is_null(),
            _ => true,
        }
    }
}

/// What a resume level needs the state to actually contain. A level
/// never silently degrades: asking for more than the artifact carries is
/// a refusal, not a partial resume.
pub fn required_fields(level: ResumeLevel) -> &'static [&'static str] {
    match level {
        // A fresh ledger starting from committed weights: parameters only.
        ResumeLevel::ModelInitialization => &["params"],
        // Continuing the same training: the optimizer moments and the RNG
        // stream, or the continuation is not the run it claims to be.
        ResumeLevel::LearningContinuation => &["params", "optimizer", "rng"],
        // Replay must additionally be able to reconstruct the exact
        // sampling sequence, which is the RNG stream plus moments.
        ResumeLevel::ControlledReplay => &["params", "optimizer", "rng"],
    }
}

/// Read and vet a worker state artifact against a declared resume level.
/// This is the compatibility gate: unknown schema, foreign run lineage,
/// or a level the artifact cannot satisfy means the host refuses instead
/// of resuming into something it cannot describe.
pub fn read_state_manifest(
    path: &Path,
    expected_run: &str,
    level: ResumeLevel,
) -> Result<StateManifest> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorKind::ArtifactUnavailable,
            format!("state artifact {} is unreadable: {e}", path.display()),
        )
    })?;
    let manifest = parse_state_manifest(&bytes, expected_run)?;
    require_level(&manifest, level)?;
    Ok(manifest)
}

/// Parse and lineage-check state bytes. Schema 1 is history-only: it can
/// be read, but never resumed, because it does not say which parameters
/// the optimizer moments belong to.
pub fn parse_state_manifest(bytes: &[u8], expected_run: &str) -> Result<StateManifest> {
    let manifest: StateManifest = serde_json::from_slice(bytes).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("state artifact does not parse as a grove state manifest: {e}"),
        )
    })?;
    if manifest.schema != STATE_SCHEMA || manifest.protocol != STATE_PROTOCOL {
        if manifest.schema == LEGACY_STATE_SCHEMA && manifest.protocol == STATE_PROTOCOL {
            return Ok(manifest);
        }
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "state artifact has unsupported schema {} ({}); supported is {} ({})",
                manifest.schema, manifest.protocol, STATE_SCHEMA, STATE_PROTOCOL
            ),
        ));
    }
    if manifest.run_id != expected_run {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "state artifact belongs to run {:?}, expected {:?}; state must not be spliced across runs",
                manifest.run_id, expected_run
            ),
        ));
    }
    Ok(manifest)
}

/// Refuse a resume the artifact cannot honestly serve.
pub fn require_level(manifest: &StateManifest, level: ResumeLevel) -> Result<()> {
    if manifest.schema < STATE_SCHEMA {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "state artifact schema {} predates the recorded trainable set and cannot serve {:?}; \
                 it stays readable as history, but re-initialize explicitly instead",
                manifest.schema, level
            ),
        ));
    }
    for field in required_fields(level) {
        if !manifest.has(field) {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!("{level:?} requires {field} in the state artifact, which is missing"),
            ));
        }
    }
    if required_fields(level).contains(&"optimizer") {
        let keys = manifest.optimizer_keys();
        if keys.is_empty() {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                "the state artifact records no optimizer parameters; a learning continuation \
                 without optimizer state is a new run wearing the old run's name",
            ));
        }
        if !keys.iter().any(|k| k.ends_with(".bias")) {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                "the state artifact has no bias in its optimizer parameters; \
                 the attempt that wrote it could not have trained biases",
            ));
        }
    }
    Ok(())
}

/// Commit a checkpoint: the state artifact becomes a durable, digest-named
/// object and only then does the checkpoint row appear. The manifest check
/// runs before the row exists, so an unparsable state never enters history.
///
/// The bytes that were validated are the bytes that get committed. The
/// file is read once: a second read would let a file replaced between
/// validation and commit slip an unvalidated artifact into history.
pub fn commit(
    store: &Store,
    actor: &Actor,
    run_id: &str,
    parent: Option<String>,
    level: ResumeLevel,
    state_path: &Path,
    budget_spent_steps: u32,
    now_ms: i64,
) -> Result<Checkpoint> {
    actor.require(ActorRole::Operator, "committing a checkpoint")?;
    // Read once, then validate *these* bytes.
    let bytes = std::fs::read(state_path).map_err(|e| {
        Error::new(
            ErrorKind::ArtifactUnavailable,
            format!("state artifact {} is unreadable: {e}", state_path.display()),
        )
    })?;
    let manifest = parse_state_manifest(&bytes, run_id)?;
    require_level(&manifest, level)?;
    // the checkpoint continues the run's model: lineage is the base
    // snapshot, which must be a committed manifest owned by this actor
    let run = store.get_run(run_id)?;
    // the artifact goes into the store under its digest — the same bytes
    let state_artifact = store.artifacts().put(&bytes)?;

    let checkpoint = Checkpoint {
        schema: SCHEMA_VERSION,
        id: format!("ckpt-{}-{}", run_id, manifest.step),
        owner: actor.id.clone(),
        run_id: run_id.to_string(),
        snapshot: run.base_snapshot,
        parent,
        resume_level: level,
        state_artifact,
        budget_spent_steps,
        created_at_ms: now_ms,
    };
    store.commit_checkpoint(actor, &checkpoint)?;
    Ok(checkpoint)
}

/// What a resume will create. The parent run is never mutated; the new
/// run inherits the budget ledger for learning-continuation.
#[derive(Debug)]
pub struct ResumePlan {
    pub run: Run,
    pub checkpoint_id: String,
    pub state_step: u32,
}

/// Plan (and record) a resume from a committed checkpoint.
///
/// * `ModelInitialization` — new ledger; the checkpoint's weights are a
///   starting point, no continuity is claimed.
/// * `LearningContinuation` — steps already spent are inherited, so the
///   budget line of the lineage is one continuous ledger.
/// * `ControlledReplay` — requires `deterministic_host: true`; without a
///   declared deterministic environment the host refuses to promise
///   replay.
pub fn resume_plan(
    store: &Store,
    actor: &Actor,
    checkpoint_id: &str,
    level: ResumeLevel,
    deterministic_host: bool,
    new_run_id: &str,
    steps_budget: u32,
) -> Result<ResumePlan> {
    actor.require(ActorRole::Operator, "resuming from a checkpoint")?;
    let checkpoint = store.get_checkpoint(checkpoint_id)?;
    // Losing the state object is not a reinitialization: a resume that
    // cannot restore the trained state must say so, not silently start over.
    let state_bytes = store.artifacts().get(&checkpoint.state_artifact)?;
    // A lineage whose data was retracted cannot be continued either.
    store.require_resumable(&checkpoint.snapshot)?;
    let manifest: StateManifest = serde_json::from_slice(&state_bytes).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("checkpointed state does not parse: {e}"),
        )
    })?;
    if manifest.schema != STATE_SCHEMA || manifest.protocol != STATE_PROTOCOL {
        if manifest.schema != LEGACY_STATE_SCHEMA {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!(
                    "checkpointed state has unsupported schema {} ({}); migrate explicitly",
                    manifest.schema, manifest.protocol
                ),
            ));
        }
    }
    // The requested level must be one this artifact can actually serve.
    require_level(&manifest, level)?;

    if level == ResumeLevel::ControlledReplay && !deterministic_host {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "controlled replay requires a declared deterministic host; \
             external responses are unknown, so replay is not promised",
        ));
    }

    let parent_run = store.get_run(&checkpoint.run_id)?;
    let steps_consumed = match level {
        // learning-continuation inherits the ledger: a resume never resets
        // what the lineage already spent
        ResumeLevel::LearningContinuation => checkpoint.budget_spent_steps,
        ResumeLevel::ModelInitialization | ResumeLevel::ControlledReplay => 0,
    };
    if steps_consumed > steps_budget {
        return Err(Error::new(
            ErrorKind::BudgetExhausted,
            format!(
                "checkpoint lineage spent {steps_consumed} steps, new budget is {steps_budget}"
            ),
        ));
    }

    let run = Run {
        schema: SCHEMA_VERSION,
        id: new_run_id.to_string(),
        task_id: parent_run.task_id.clone(),
        base_snapshot: checkpoint.snapshot,
        dataset_revision: parent_run.dataset_revision.clone(),
        recipe: parent_run.recipe.clone(),
        state: RunState::Queued,
        steps_consumed,
        steps_budget,
        resumed_from: Some(checkpoint.run_id.clone()),
    };
    store.put_run(actor, &run)?;
    Ok(ResumePlan {
        run,
        checkpoint_id: checkpoint_id.to_string(),
        state_step: manifest.step,
    })
}

/// Pause a run at a checkpoint boundary. The run becomes `paused` only
/// after its checkpoint is committed — a failed save leaves the run
/// exactly where it was, never in a claimed-safe state it cannot back.
pub fn pause(
    store: &Store,
    actor: &Actor,
    run_id: &str,
    state_path: &Path,
    now_ms: i64,
) -> Result<Checkpoint> {
    actor.require(ActorRole::Operator, "pausing a run")?;
    let run = store.get_run(run_id)?;
    if run.state.is_terminal() {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!("run {run_id} is terminal ({:?}); nothing to pause", run.state),
        ));
    }
    let checkpoint = commit(
        store,
        actor,
        run_id,
        None,
        ResumeLevel::LearningContinuation,
        state_path,
        run.steps_consumed,
        now_ms,
    )?;
    let mut paused = run.clone();
    paused.state = RunState::Paused;
    store.set_run_state(&paused)?;
    Ok(checkpoint)
}

/// Fork a new branch from a committed checkpoint. The parent checkpoint
/// and its run are untouched; the new branch starts pointing at the
/// parent's checkpoint and evolves independently.
pub fn fork_branch(
    store: &Store,
    actor: &Actor,
    parent_checkpoint_id: &str,
    branch_id: &str,
    policy: &str,
    budget_quota: u32,
) -> Result<Branch> {
    actor.require(ActorRole::Operator, "forking a branch")?;
    let parent = store.get_checkpoint(parent_checkpoint_id)?;
    // The fork records the derivation edge: it came from the parent's
    // state, as parameter training until a code change says otherwise.
    let branch = Branch {
        schema: SCHEMA_VERSION,
        id: branch_id.to_string(),
        owner: actor.id.clone(),
        head: Some(parent.id.clone()),
        head_version: 0,
        policy: policy.to_string(),
        budget_quota,
    };
    store.put_branch(actor, &branch)?;
    Ok(branch)
}

/// The derivation edge label for lineage bookkeeping (used by the branch
/// record's policy text until W07 gives edges their own table).
pub fn derivation_label(d: Derivation) -> &'static str {
    match d {
        Derivation::ParameterTraining => "parameter-training",
        Derivation::CodeRewrite => "code-rewrite",
        Derivation::WeightMigration => "weight-migration",
        Derivation::ModuleComposition => "module-composition",
        Derivation::Distillation => "distillation",
    }
}
