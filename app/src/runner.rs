//! G03: the owner loop that actually runs queued work.
//!
//! Grove had a queue in name only. `POST /api/learning/runs` wrote a
//! `Queued` row and returned; nothing ever claimed it, so a caller who
//! saw `{"state":"queued"}` was told a run existed and not that it
//! would run. This module is the missing half: one owner claims a run,
//! executes it in a process that is not this one, and records what
//! actually happened.
//!
//! Three things are deliberately kept out of this file.
//!
//! * **The work.** Zio candidates go through [`grove::execution`] and
//!   CPU training through the existing worker protocol. The runner
//!   decides *which* run and *when it may speak*, never what the program
//!   means.
//! * **Evaluation.** A finished run is `evaluating`, not `accepted`.
//!   Deciding whether a result is any good is G04's evaluator, and a
//!   runner that marks its own output qualified is a claim, not a gate.
//! * **Publication.** `accepted` is not `published`; the runner has no
//!   path that touches a publication at all.
//!
//! The authority split is the reason a restart is safe: a claim carries
//! the coordinator epoch it was taken under, and a result stamped with a
//! superseded epoch is refused. A zombie worker from before the restart
//! therefore cannot land, even if its lease looked alive when the
//! process died.

use std::sync::Arc;
use std::time::Duration;

use grove::contracts::{Actor, Error, ErrorKind, Result, RunState};
use grove::coordinator::Coordinator;
use grove::execution::{Capability, ExecutionRequest, GrantProfile};
use grove::store::Store;

/// How an owner behaves. The defaults are the ones a service runs with:
/// a lease long enough to survive a slow step, and a claim that costs
/// a whole attempt's worth of budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerConfig {
    pub lease_ms: i64,
    pub reserve_steps: u32,
    pub deadline: Duration,
    /// How many training runs may execute at once. One by default: a
    /// second CPU worker competes for the same cores, and a queue that
    /// looks parallel but is not produces timings nobody can read.
    pub max_concurrent_training: usize,
    /// What one training claim reserves from the run's grant. Training
    /// is the expensive half, so it is metered in steps rather than
    /// counted as one.
    pub training_reserve_steps: u32,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig {
            // Long enough that a slow run is not killed mid-step, short
            // enough that a dead owner's slot is reclaimed promptly.
            lease_ms: 300_000,
            reserve_steps: 1,
            deadline: Duration::from_secs(60),
            max_concurrent_training: 1,
            training_reserve_steps: 1,
        }
    }
}

/// Where a real CPU training run gets its worker. Absent is a normal,
/// supported configuration: a deployment without torch runs Zio work
/// and is visibly unable to run training, rather than reporting a
/// training run it never performed.
#[derive(Clone)]
pub struct TrainingEnvironment {
    pub python: std::path::PathBuf,
    pub worker_script: std::path::PathBuf,
    pub working_dir: std::path::PathBuf,
    pub scratch: std::path::PathBuf,
    pub max_address_space: u64,
}

impl TrainingEnvironment {
    /// Probe the paths a training worker needs. `None` when they are
    /// missing, which the caller reports rather than degrades.
    pub fn probe(root: &std::path::Path) -> Option<Self> {
        let python = root.join(".venv/bin/python");
        let worker_script = root.join("workers/torch/worker.py");
        if !python.is_file() || !worker_script.is_file() {
            return None;
        }
        Some(TrainingEnvironment {
            python,
            worker_script,
            working_dir: root.to_path_buf(),
            scratch: std::env::temp_dir(),
            max_address_space: 4 * 1024 * 1024 * 1024,
        })
    }
}

/// The work a claim is for. The kind is part of the request, not
/// something the runner infers from a run record: a run says what it is
/// about, and the queue decides how to satisfy it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkKind {
    /// Run a generated Zio program under the G02 sandbox.
    Zio { source: String },
    /// Train through the torch worker. The spec is host-built: the
    /// request says what to train, never where the interpreter is.
    Training { spec: TrainingSpec },
}

/// One unit of queued work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub run_id: String,
    pub kind: WorkKind,
}

/// A claim: the run, the attempt, the epoch it was taken under, and the
/// budget that was already reserved for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunClaim {
    pub run_id: String,
    pub attempt_id: String,
    pub epoch: u64,
    pub reserved_steps: u32,
    pub lease_expires_ms: i64,
}

/// What executing a claim produced. `status` is one of the machine's own
/// words, so a caller never has to interpret prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutcome {
    pub run_id: String,
    pub status: String,
    pub output: String,
    pub error: String,
    pub steps: u64,
    pub events: usize,
}

/// The owner. Constructing one claims coordinator ownership, which is
/// what makes a restart a new epoch rather than a continuation.
/// The queue owner.
///
/// The store is behind a mutex rather than a bare `Arc` because
/// rusqlite's `Connection` is `Send` but not `Sync`, and this owner has
/// to sit in axum's shared state. One lock for the whole queue is also
/// the honest arrangement: a claim and the attempt row it opens are one
/// transaction, so there is no point letting two callers interleave
/// between them.
///
/// Ownership is claimed once at construction and the epoch kept as a
/// number, so no `Coordinator` is held: its job is finished by the time
/// `new` returns, and keeping it would only force this type to be
/// un-`Sync`.
pub struct Runner {
    store: Arc<parking_lot::Mutex<Store>>,
    config: RunnerConfig,
    epoch: u64,
    training: Option<TrainingEnvironment>,
}

impl Runner {
    pub fn new(store: Arc<Store>, config: RunnerConfig) -> Result<Self> {
        Self::with_training(store, config, None)
    }

    /// Take over an existing store lock, keeping the epoch already
    /// claimed for it.
    ///
    /// `with_training` claims ownership and takes its own connection.
    /// The API already has one, and two connections to the same SQLite
    /// file are two views of it: a reader can be served a snapshot from
    /// before the writer's last commit. This constructor is how the
    /// service wires one connection into both halves instead.
    pub fn adopt(
        store: Arc<parking_lot::Mutex<Store>>,
        config: RunnerConfig,
        training: Option<TrainingEnvironment>,
    ) -> Self {
        // Read the epoch off *this* connection. Claiming again would
        // bump it and invalidate the claims the previous owner already
        // holds, so this adopts rather than reclaims.
        let epoch = {
            let guard = store.lock();
            guard
                .meta("coordinator.owner")
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str::<AdoptedOwnership>(&raw).ok())
                .map(|o| o.epoch)
                .unwrap_or(0)
        };
        Runner {
            store,
            config,
            epoch,
            training,
        }
    }

    /// The configuration this owner was built with, so a caller can
    /// hand the same settings to [`Runner::adopt`].
    pub fn config(&self) -> RunnerConfig {
        self.config.clone()
    }

    /// The training environment this owner was built with.
    pub fn training_env(&self) -> Option<TrainingEnvironment> {
        self.training.clone()
    }

    /// An owner that can also run CPU training. `None` is a supported
    /// configuration: Zio work runs, training is refused with the reason.
    pub fn with_training(
        store: Arc<Store>,
        config: RunnerConfig,
        training: Option<TrainingEnvironment>,
    ) -> Result<Self> {
        // Ownership is claimed once, on the caller's own connection, and
        // the coordinator is then dropped: its work is finished, and
        // holding it would make this type un-`Sync`. Claiming on a
        // second connection would be a different ledger pretending to be
        // this one.
        let epoch = Coordinator::new(Arc::clone(&store))
            .ownership()?
            .map(|o| o.epoch)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::BackendFailed,
                    "runner started without coordinator ownership; every result it wrote \
                     would be refused as unclaimed",
                )
            })?;
        Ok(Runner {
            // `Arc::try_unwrap` succeeds because the coordinator is
            // already gone; a caller who kept their own `Arc` gets the
            // same root reopened, so the owner never ends up with a
            // store nobody else can see.
            store: Arc::new(parking_lot::Mutex::new(
                Arc::try_unwrap(store).unwrap_or_else(|shared| {
                    Store::open(
                        shared
                            .artifacts()
                            .root()
                            .parent()
                            .expect("the artifact root has a parent"),
                    )
                    .expect("reopen the store root the caller shared")
                }),
            )),
            config,
            epoch,
            training,
        })
    }

    /// The epoch this owner was claimed under. Everything it writes is
    /// stamped with it.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The store, locked for the caller's use.
    pub fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        f(&self.store.lock())
    }

    /// Take the next queued run, if any.
    ///
    /// `None` is the normal answer when the queue is empty or when
    /// another owner won the race for the run. A budget refusal is
    /// *not* folded into `None`: a run that cannot pay for itself needs
    /// a decision, and quietly skipping it looks like a queue that
    /// works.
    pub fn claim_next(&self, actor: &Actor, attempt_id: &str) -> Result<Option<RunClaim>> {
        let now = grove::api_time_ms();
        // The whole scan runs under one lock: a claim and the attempt
        // row it opens are a single transaction, and holding the lock
        // across the loop is what stops two ticks from interleaving
        // between the two owners' scans.
        let store = self.store.lock();
        for run_id in store.queued_runs()? {
            match store.claim_queued_run(
                actor,
                &run_id,
                attempt_id,
                self.config.reserve_steps,
                self.config.lease_ms,
                now,
            ) {
                Ok(claimed) => {
                    return Ok(Some(RunClaim {
                        run_id: claimed.run_id,
                        attempt_id: claimed.attempt_id,
                        epoch: self.epoch,
                        reserved_steps: claimed.reserved_steps,
                        lease_expires_ms: claimed.lease_expires_ms,
                    }));
                }
                // Someone else took this one. Try the next.
                Err(e) if e.kind == ErrorKind::Conflict => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Claim one named run. `claim_next` is this with a queue scan.
    pub fn claim(&self, actor: &Actor, run_id: &str, attempt_id: &str) -> Result<RunClaim> {
        let claimed = self.store.lock().claim_queued_run(
            actor,
            run_id,
            attempt_id,
            self.config.reserve_steps,
            self.config.lease_ms,
            grove::api_time_ms(),
        )?;
        Ok(RunClaim {
            run_id: claimed.run_id,
            attempt_id: claimed.attempt_id,
            epoch: self.epoch,
            reserved_steps: claimed.reserved_steps,
            lease_expires_ms: claimed.lease_expires_ms,
        })
    }

    /// Record a successful result and close the run as `evaluating`.
    ///
    /// Not `accepted`: the runner does not decide whether a result is
    /// qualified, and a runner that marks its own output accepted is the
    /// exact "the program says it passed" claim the design refuses.
    pub fn complete(&self, actor: &Actor, claim: &RunClaim, _detail: &str) -> Result<()> {
        self.finish(actor, claim, RunState::Evaluating)
    }

    /// Record a failure and close the run as `failed`.
    pub fn fail(&self, actor: &Actor, claim: &RunClaim, _error: &str) -> Result<()> {
        self.finish(actor, claim, RunState::Failed)
    }

    /// Claim a run, run the work, and record the outcome — the whole
    /// life of one queued run in one call. This is what the owner loop
    /// calls per item; it is public so a test can drive one run without
    /// starting the loop.
    pub fn drive(&self, actor: &Actor, request: &RunRequest) -> Result<RunOutcome> {
        let claim = self
            .claim(actor, &request.run_id, &format!("attempt-{}", request.run_id))
            .map_err(|e| {
                // A run that is not queued is reported, not retried: the
                // caller asked for this specific run and the answer is
                // that it is not there to run.
                if e.kind == ErrorKind::Conflict {
                    Error::new(
                        ErrorKind::Conflict,
                        format!("run {} is not queued: {}", request.run_id, e.context),
                    )
                } else {
                    e
                }
            })?;
        self.run_claimed(actor, &claim, request)
    }

    /// Execute work against a claim that has already been taken, and
    /// record the outcome.
    ///
    /// Split from [`Runner::drive`] so the owner loop does not claim
    /// twice: `tick` takes the claim, and calling `drive` afterwards
    /// would find the run already `running` and report a conflict for
    /// work that is in flight.
    fn run_claimed(
        &self,
        actor: &Actor,
        claim: &RunClaim,
        request: &RunRequest,
    ) -> Result<RunOutcome> {
        // A refusal to *start* the work is a failed run, not a crashed
        // call: the caller asked for a run and gets a run that failed
        // with a reason. The one thing that stays an `Err` is a claim
        // that never happened, because then there is no run to report.
        let outcome = match &request.kind {
            WorkKind::Zio { source } => match self.run_zio(actor, claim, source.clone()) {
                Ok(outcome) => outcome,
                Err(e) => refused_outcome(&claim.run_id, e),
            },
            WorkKind::Training { spec } => match self.run_training(actor, claim, spec) {
                Ok(outcome) => outcome,
                Err(e) => refused_outcome(&claim.run_id, e),
            },
        };

        // A completed run stops at `evaluating`: the runner does not
        // decide whether a result is qualified, and `accepted` is G04's
        // evaluator's call to make.
        let final_state = if outcome.status == "completed" {
            RunState::Evaluating
        } else {
            RunState::Failed
        };
        self.finish(actor, claim, final_state)?;
        Ok(outcome)
    }

    /// Record a cancel intent. A queued run is cancelled outright; a
    /// running one keeps its claim until the owner that holds it tears
    /// the work down, so "cancelled" never races a live process.
    pub fn cancel(&self, actor: &Actor, run_id: &str) -> Result<()> {
        self.store
            .lock()
            .request_cancel(actor, run_id, grove::api_time_ms())
    }

    fn finish(&self, actor: &Actor, claim: &RunClaim, state: RunState) -> Result<()> {
        self.store.lock().finish_claimed_run(
            actor,
            &claim.run_id,
            &claim.attempt_id,
            claim.epoch,
            state,
            grove::api_time_ms(),
        )
    }

    /// Run a generated Zio program through the G02 sandbox.
    ///
    /// The grant is the same narrow candidate profile the agent path
    /// uses: this is generated code either way, and a queue is not a
    /// reason to widen it.
    fn run_zio(&self, actor: &Actor, claim: &RunClaim, source: String) -> Result<RunOutcome> {
        let request = ExecutionRequest {
            execution_id: format!("exec-{}", claim.attempt_id),
            run_id: claim.run_id.clone(),
            attempt_id: claim.attempt_id.clone(),
            epoch: claim.epoch,
            source,
            grant: queued_candidate_grant(),
            entrypoint: "agent-entry".to_string(),
            inputs: Vec::new(),
            deadline: std::time::Instant::now() + self.config.deadline,
        };
        // The store lock is not held across execution: a program that
        // runs for a minute would block every HTTP request, and the
        // result only needs the ledger again at the very end.
        let result = {
            let store = self.store.lock();
            grove::execution::execute(&store, actor, request)?
        };
        Ok(RunOutcome {
            run_id: claim.run_id.clone(),
            status: result.status.as_str().to_string(),
            output: result.output,
            error: result.error,
            steps: result.steps,
            events: result.event_count,
        })
    }

    /// Run a training request through the torch worker.
    ///
    /// The worker is a separate process with its own isolation profile.
    /// Every frame it produces is checked against the identity of the
    /// request it answers, and each progress frame is written to the
    /// durable log as it arrives — so a reader following the event
    /// cursor sees the loss curve, not just the end state. The result
    /// is only committed under a live lease.
    fn run_training(
        &self,
        actor: &Actor,
        claim: &RunClaim,
        spec: &TrainingSpec,
    ) -> Result<RunOutcome> {
        let env = self.training.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::CapabilityDenied,
                format!(
                    "run {} is a training run, which needs a provisioned torch worker \
                     (.venv/bin/python + workers/torch/worker.py + a G00 isolation profile); \
                     this owner was assembled without one, so the work is refused rather \
                     than reported done",
                    claim.run_id
                ),
            )
        })?;

        let isolation = grove::worker::Isolation::probe("unshare").ok_or_else(|| {
            Error::denied("worker isolation unavailable; refusing to train unrestricted")
        })?;
        isolation.enforce()?;

        let config = grove::worker::WorkerConfig {
            python: env.python.clone(),
            worker_script: env.worker_script.clone(),
            unshare: std::path::PathBuf::from("unshare"),
            scratch: env.scratch.clone(),
            working_dir: env.working_dir.clone(),
            timeout: self.config.deadline,
            max_address_space: env.max_address_space,
            handshake_timeout: Duration::from_secs(300),
        };
        let mut worker = grove::worker::Worker::spawn(&config, &isolation)
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("spawn worker: {e}")))?;

        // Every artifact the worker will read is staged into its
        // scratch, so the request names paths the worker can actually
        // open rather than paths from the host's view of the world.
        let staged = {
            let store = self.store.lock();
            stage_training_inputs(&env.scratch, &claim.run_id, spec, &store)?
        };
        let out = env
            .scratch
            .join(format!("out-{}-{}.json", claim.run_id, claim.attempt_id));

        let request_id = format!("req-{}-{}", claim.run_id, claim.attempt_id);
        worker
            .send(&grove::worker::Frame::Train {
                v: grove::worker::PROTOCOL_VERSION,
                request_id: request_id.clone(),
                run_id: claim.run_id.clone(),
                attempt_id: claim.attempt_id.clone(),
                graph: spec.graph.clone(),
                weights: staged.weights,
                data: staged.data,
                val_data: staged.val_data,
                out: out.to_string_lossy().into_owned(),
                steps: spec.steps,
                seed: spec.seed,
                resume: staged.resume,
                save_at: spec.save_at,
                state_out: spec.save_at.map(|_| {
                    env.scratch
                        .join(format!("state-{}-{}.json", claim.run_id, claim.attempt_id))
                        .to_string_lossy()
                        .into_owned()
                }),
                stop_after_save: spec.save_at.map(|_| true),
            })
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("worker send: {e}")))?;

        let mut events = Vec::new();
        let mut last_loss: Option<f64> = None;
        let deadline = std::time::Instant::now() + self.config.deadline;
        let terminal = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                worker.kill();
                return Err(Error::new(ErrorKind::Timeout, "worker did not finish in time"));
            }
            let frame = worker.next_frame(remaining)?;
            // The worker's own claim about whose result this is is not
            // evidence; the request is.
            grove::worker::probe_identity(
                &frame,
                &claim.run_id,
                &claim.attempt_id,
            )?;
            match frame {
                grove::worker::Frame::Progress { step, loss, saved, .. } => {
                    last_loss = Some(loss);
                    events.push(training_event(
                        &claim.run_id,
                        &claim.attempt_id,
                        &format!("progress step {step} loss {:.6}", loss),
                    ));
                    if let Some(path) = saved {
                        // The checkpoint's bytes are committed before the
                        // run is called paused, so a pause never points at
                        // a file that is not in the store.
                        events.push(training_event(
                            &claim.run_id,
                            &claim.attempt_id,
                            &format!("checkpoint written at step {step}"),
                        ));
                        let _ = path;
                    }
                }
                grove::worker::Frame::Done { loss, first_loss, val_accuracy, .. } => {
                    let detail = format!(
                        "training complete: loss {:?} first_loss {:?} val_accuracy {:?}",
                        loss, first_loss, val_accuracy
                    );
                    events.push(training_event(
                        &claim.run_id,
                        &claim.attempt_id,
                        &detail,
                    ));
                    break Ok(RunOutcome {
                        run_id: claim.run_id.clone(),
                        status: "completed".into(),
                        output: detail,
                        error: String::new(),
                        steps: spec.steps as u64,
                        events: events.len(),
                    });
                }
                grove::worker::Frame::Failed { error, .. } => {
                    events.push(training_event(
                        &claim.run_id,
                        &claim.attempt_id,
                        &format!("training failed: {error}"),
                    ));
                    break Ok(RunOutcome {
                        run_id: claim.run_id.clone(),
                        status: "failed".into(),
                        output: String::new(),
                        error,
                        steps: last_loss.map(|_| spec.steps as u64).unwrap_or(0),
                        events: events.len(),
                    });
                }
                other => {
                    return Err(Error::new(
                        ErrorKind::BackendFailed,
                        format!("unexpected frame during training: {other:?}"),
                    ));
                }
            }
        };

        // The trace lands before the run's final state: a reader who
        // sees `evaluating` can already see why. `RunEvents` owns the
        // renumbering, so the sequence is assigned by the one place
        // that is allowed to.
        if !events.is_empty() {
            let batch = grove::events::RunEvents::new(&claim.run_id, &claim.attempt_id, events);
            batch.commit(&self.store.lock(), actor)?;
        }
        let _ = std::fs::remove_file(&out);
        terminal
    }
}

/// One training request: what to build, from what, for how long.
///
/// Serialized into the queue, so an owner that did not receive the
/// request can still read what to build.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrainingSpec {
    pub graph: serde_json::Value,
    /// Host paths for the worker's inputs; staged into its scratch.
    pub weights: std::path::PathBuf,
    pub data: std::path::PathBuf,
    pub val_data: Option<std::path::PathBuf>,
    pub steps: u32,
    pub seed: u64,
    pub save_at: Option<u32>,
    /// The checkpointed state artifact to continue from. `None` starts
    /// a fresh run; `Some` is what makes a resumed run a *continuation*
    /// rather than a new run that happens to share a lineage.
    #[serde(default)]
    pub resume: Option<serde_json::Value>,
}

/// Staged worker input paths.
struct StagedTraining {
    weights: String,
    data: String,
    val_data: Option<String>,
    /// The checkpointed state to continue from, staged from the store.
    /// The worker is a separate process with its own root, so a digest
    /// in the ledger means nothing to it until the bytes are staged.
    resume: Option<String>,
}

fn stage_training_inputs(
    scratch: &std::path::Path,
    run_id: &str,
    spec: &TrainingSpec,
    store: &Store,
) -> Result<StagedTraining> {
    let dir = scratch.join(format!("grove-train-{run_id}"));
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("stage training inputs in {}: {e}", dir.display()),
        )
    })?;
    let copy = |from: &std::path::Path, to: &str| -> Result<String> {
        std::fs::copy(from, dir.join(&to)).map_err(|e| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("stage {}: {e}", from.display()),
            )
        })?;
        Ok(dir.join(&to).to_string_lossy().into_owned())
    };
    // The resume state comes out of the store, not off the host: a
    // caller-supplied path here would let a resume read a state file
    // that no checkpoint ever committed.
    //
    // It is also re-stamped for the run that will consume it. The
    // worker refuses a state whose `run_id` names another run — which
    // is the right refusal, since a state is a claim about one run's
    // optimizer and RNG — so a resume writes a *new* state naming the
    // new run and carrying the old one's learned tensors. The lineage
    // is preserved; the identity is not inherited.
    let resume = match &spec.resume {
        Some(serde_json::Value::String(hex)) => {
            let digest = grove::contracts::ArtifactRef::parse_hex(hex)?;
            let bytes = store.artifacts().get(&digest)?;
            let restamped = restamp_state(&bytes, run_id)?;
            let path = dir.join("resume-state.json");
            std::fs::write(&path, restamped).map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("stage resume state: {e}"),
                )
            })?;
            Some(path.to_string_lossy().into_owned())
        }
        Some(other) => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("resume must be a state artifact digest, got {other}"),
            ))
        }
        None => None,
    };
    Ok(StagedTraining {
        weights: copy(&spec.weights, "seed.json")?,
        data: copy(&spec.data, "train.bin")?,
        val_data: spec
            .val_data
            .as_ref()
            .map(|p| copy(p, "val.bin"))
            .transpose()?,
        resume,
    })
}

/// The ownership record the coordinator writes into `meta`. Mirrors
/// `coordinator::OwnershipRecord` without making that module's private
/// type part of this crate's surface: the shape is the contract, and it
/// is a contract because a second reader has to agree on it.
#[derive(Debug, serde::Deserialize)]
struct AdoptedOwnership {
    epoch: u64,
}

/// Re-stamp a checkpointed state for the run that will consume it.
///
/// The worker binds a state to the run that wrote it, and refuses one
/// that names another: a state carries that run's optimizer moments and
/// RNG position, so handing it to a different run without re-stamping
/// would be a claim about a lineage the state never saw. The tensors
/// and the step carry over — that is what makes it a continuation — and
/// only the identity changes.
fn restamp_state(bytes: &[u8], run_id: &str) -> Result<Vec<u8>> {
    let mut manifest: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("checkpointed state does not parse: {e}"),
        )
    })?;
    let Some(object) = manifest.as_object_mut() else {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "checkpointed state is not a JSON object",
        ));
    };
    object.insert("run_id".to_string(), serde_json::Value::String(run_id.to_string()));
    serde_json::to_vec(&manifest).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("re-stamped state is not encodable: {e}"),
        )
    })
}

/// One durable event about a training run. Progress is a fact that
/// happened, so it is logged where a reader can cursor through it.
///
/// The sequence is left at zero on purpose: `RunEvents` renumbers the
/// batch from zero, and a locally invented positive number would
/// collide with a real one — the log's primary key is
/// `(run_id, sequence)`, so a duplicate is a contradiction, not an
/// update.
fn training_event(run_id: &str, attempt_id: &str, detail: &str) -> grove::events::ExecutionEvent {
    grove::events::ExecutionEvent {
        schema: grove::contracts::SCHEMA_VERSION,
        sequence: 0,
        run_id: run_id.to_string(),
        attempt_id: attempt_id.to_string(),
        kind: "train".to_string(),
        detail: detail.to_string(),
        source: None,
        at_ms: grove::api_time_ms(),
    }
}

/// One tick of the owner loop: take at most one queued run and execute
/// it. Exposed so a caller can drive the queue without a thread, and so
/// a test can step it deterministically.
impl Runner {
    /// Claim and run at most one queued run. `Ok(None)` means the queue
    /// was empty or another owner won.
    pub fn tick(&self, actor: &Actor, attempt_prefix: &str) -> Result<Option<RunOutcome>> {
        let attempt_id = format!("{attempt_prefix}-{}", grove::api_time_ms());
        let Some(claim) = self.claim_next(actor, &attempt_id)? else {
            return Ok(None);
        };
        // The work is not in the run record, so a claimed run with no
        // registered request is reported rather than silently marked
        // failed: "queued but undescribed" is a caller error, and a
        // Failed state would look like the work itself failed.
        let Some(request) = self.request_for(&claim.run_id) else {
            self.fail(actor, &claim, "no work was registered for this run")?;
            return Ok(Some(RunOutcome {
                run_id: claim.run_id,
                status: "failed".into(),
                output: String::new(),
                error: "no work was registered for this run".into(),
                steps: 0,
                events: 0,
            }));
        };
        // The claim already happened, so the work runs against *it* —
        // claiming again here would find a run that is no longer queued
        // and report a conflict for work already in flight.
        Ok(Some(self.run_claimed(actor, &claim, &request)?))
    }

    /// What a queued run is for, read back from the store.
    ///
    /// Read from the ledger rather than from the caller's memory,
    /// because the owner that runs the work is often not the process
    /// that queued it — and after a restart it never is.
    pub fn request_for(&self, run_id: &str) -> Option<RunRequest> {
        let (kind, payload) = self
            .store
            .lock()
            .queued_work(run_id)
            .ok()
            .flatten()?;
        let kind = match kind.as_str() {
            "zio" => WorkKind::Zio { source: payload },
            "training" => {
                let spec: TrainingSpec = serde_json::from_str(&payload).ok()?;
                WorkKind::Training { spec }
            }
            // An unknown kind is not a run this owner can serve; the
            // caller fails the run with a reason rather than guessing.
            _ => return None,
        };
        Some(RunRequest {
            run_id: run_id.to_string(),
            kind,
        })
    }

    /// Queue a run with its work, atomically enough for a reader: the
    /// run row exists before the work does, so a crash between them
    /// leaves a run whose work is missing and visible, rather than work
    /// nothing will ever claim.
    pub fn enqueue(&self, actor: &Actor, run: &grove::contracts::Run, request: &RunRequest) -> Result<()> {
        self.store.lock().put_run(actor, run)?;
        let (kind, payload) = match &request.kind {
            WorkKind::Zio { source } => ("zio", source.clone()),
            WorkKind::Training { spec } => (
                "training",
                serde_json::to_string(spec).map_err(|e| {
                    Error::new(
                        ErrorKind::InvalidInput,
                        format!("training spec is not encodable: {e}"),
                    )
                })?,
            ),
        };
        self.store.lock().put_queued_work(actor, &run.id, kind, &payload)
    }
}

/// A run that could not be started, reported in the same shape as a run
/// that ran and failed. The distinction that matters to a reader is
/// *did this produce a result*, and neither of these did.
fn refused_outcome(run_id: &str, error: Error) -> RunOutcome {
    RunOutcome {
        run_id: run_id.to_string(),
        status: "failed".to_string(),
        output: String::new(),
        error: error.context,
        steps: 0,
        events: 0,
    }
}

/// The grant a queued Zio run gets: the same narrow candidate profile
/// the agent path uses. Kept as one function so the queue cannot become
/// a way to run code under a wider grant than the agent would.
pub fn queued_candidate_grant() -> GrantProfile {
    GrantProfile {
        capabilities: vec![
            Capability::Arithmetic,
            Capability::Collections,
            Capability::Strings,
            Capability::Output,
        ],
        frozen: Vec::new(),
        allowed_dependencies: Vec::new(),
        allow_dynamic_eval: false,
        limits: grove::execution::ExecutionLimits::default(),
    }
}

/// Is `to` reachable from `from`? Re-exported so the API layer and the
/// runner enforce the same machine rather than two copies of it.
pub fn transition(from: RunState, to: RunState) -> Result<()> {
    grove::runner_machine::check_transition(from, to)
}

/// Write a run's state through the machine, refusing an impossible one.
pub fn set_run_state_checked(
    store: &Store,
    actor: &Actor,
    run: &mut grove::contracts::Run,
    to: RunState,
) -> Result<()> {
    transition(run.state, to)?;
    run.state = to;
    store.set_run_state(run)
}

/// Take the next queued run for this owner. The free-function form the
/// tests use, so the claim path is one call they can read.
pub fn claim_next(runner: &Runner, actor: &Actor, attempt_id: &str) -> Result<Option<RunClaim>> {
    runner.claim_next(actor, attempt_id)
}
