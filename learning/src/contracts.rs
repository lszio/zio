//! Versioned record types and the single error vocabulary for grove.
//!
//! Every persisted record carries a [`SchemaVersion`]; a store opened on an
//! unknown version refuses to execute rather than guessing (plan W01:
//! "unknown schema is not executed"). Reference boundaries are explicit: a
//! record names the digests or ids it depends on, and the store validates
//! them in the same transaction that commits the record.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

/// The schema version stamped on every stored record.
pub type SchemaVersion = u32;

/// Version currently emitted for every record type in this crate.
pub const SCHEMA_VERSION: SchemaVersion = 1;

/// Stable error classes. Callers branch on the class, never on message text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    InvalidInput,
    CapabilityDenied,
    Conflict,
    IncompatibleState,
    BudgetExhausted,
    ArtifactUnavailable,
    Timeout,
    Cancelled,
    BackendFailed,
    /// A declared contract was violated by data that does not belong in
    /// the path: incompatible semantic spaces, a mismatched vocabulary, a
    /// masked action that reached a loss. Distinct from
    /// `incompatible-state` because the *caller's request* is malformed,
    /// not the store's history.
    ProtocolViolation,
}

impl ErrorKind {
    /// Machine-readable prefix, also the Zio-visible error prefix.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid-input",
            Self::CapabilityDenied => "capability-denied",
            Self::Conflict => "conflict",
            Self::IncompatibleState => "incompatible-state",
            Self::BudgetExhausted => "budget-exhausted",
            Self::ArtifactUnavailable => "artifact-unavailable",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::BackendFailed => "backend-failed",
            Self::ProtocolViolation => "protocol-violation",
        }
    }
}

/// A grove error: a stable [`ErrorKind`] plus minimal context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub context: String,
}

impl Error {
    pub fn new(kind: ErrorKind, context: impl Into<String>) -> Self {
        Self { kind, context: context.into() }
    }

    pub fn invalid(context: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidInput, context)
    }

    pub fn denied(context: impl Into<String>) -> Self {
        Self::new(ErrorKind::CapabilityDenied, context)
    }

    pub fn conflict(context: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, context)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.context)
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

// ── Identity and permission context ────────────────────────────────

/// Roles are distinct capabilities, not tiers of the same one: annotating a
/// label, starting a run and publishing a model are three separate grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActorRole {
    Reader,
    Annotator,
    Operator,
    Publisher,
}

impl ActorRole {
    /// Containment strength. A role is a *capability*, not a score, so
    /// there is deliberately no `Ord`: the only thing the number is for
    /// is "does this grant include that one", and for merging a
    /// permission closure across modules (a composition may require
    /// MORE than its parts, never less).
    pub fn rank(self) -> u8 {
        match self {
            Self::Reader => 0,
            Self::Annotator => 1,
            Self::Operator => 2,
            Self::Publisher => 3,
        }
    }
}

/// The authenticated caller: an id plus the single role it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub id: String,
    pub role: ActorRole,
}

impl Actor {
    pub fn new(id: impl Into<String>, role: ActorRole) -> Self {
        Self { id: id.into(), role }
    }

    /// Role containment: an operator may also annotate, a publisher may also
    /// operate. A reader may do nothing else.
    pub fn may(&self, required: ActorRole) -> bool {
        self.role.rank() >= required.rank()
    }

    pub fn require(&self, required: ActorRole, what: &str) -> Result<()> {
        if self.may(required) {
            Ok(())
        } else {
            Err(Error::denied(format!(
                "{} requires role {:?}, actor {:?} holds {:?}",
                what, required, self.id, self.role
            )))
        }
    }
}

/// Reference to one immutable artifact. Content identity *is* the digest, so
/// a corrupted body fails to load rather than silently describing something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub digest: [u8; 32],
}

impl ArtifactRef {
    pub fn to_hex(&self) -> String {
        self.digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn parse_hex(hex: &str) -> Result<Self> {
        if hex.len() != 64 {
            return Err(Error::invalid(format!(
                "artifact digest must be 64 hex chars, got {}",
                hex.len()
            )));
        }
        let mut digest = [0u8; 32];
        for (i, slot) in digest.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                .map_err(|_| Error::invalid(format!("artifact digest is not hex: {hex}")))?;
        }
        Ok(Self { digest })
    }
}

impl fmt::Display for ArtifactRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// sha256 over a manifest's canonical JSON bytes. Deterministic: the caller
/// serializes with sorted keys, so the digest is stable across processes.
pub fn digest_bytes(bytes: &[u8]) -> ArtifactRef {
    let mut hasher = Sha256::new();
    hasher.update(b"grove-artifact-v1\0");
    hasher.update(bytes);
    let out = hasher.finalize();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&out);
    ArtifactRef { digest }
}

/// Reject manifests that cannot be encoded deterministically. `serde_json`
/// silently turns a non-finite float into `null`, so encoding one would give
/// a frozen manifest a hash that silently describes a different value; the
/// metric-carrying records are therefore checked structurally before they
/// are allowed to be written.
pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value)
        .map_err(|e| Error::invalid(format!("manifest is not encodable: {e}")))
}

/// Metrics are the only float-bearing field in a stored record. A non-finite
/// value there would round-trip to `null` and make the frozen record
/// ambiguous, so it is refused at the point of use.
pub fn require_finite_metrics(metrics: &[(String, f64)]) -> Result<()> {
    for (name, value) in metrics {
        if !value.is_finite() {
            return Err(Error::invalid(format!(
                "metric {name} is {value}; stored records must carry finite numbers"
            )));
        }
    }
    Ok(())
}

// ── Records ────────────────────────────────────────────────────────

/// A module-level parameter: shape, dtype and the artifact that carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamRef {
    pub module: String,
    pub shape: Vec<u32>,
    pub dtype: String,
    pub artifact: ArtifactRef,
}

/// One module of a modular snapshot (W13).
///
/// A module binds the *semantic* space it accepts and the one it emits —
/// two values of equal width in different spaces are not
/// interchangeable, and a composition that joins them is a
/// `protocol-violation`, not a lucky guess. `layers` names the values
/// this module owns, so a local replacement can move one module without
/// touching its neighbours.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleSpec {
    pub name: String,
    /// The semantic space this module's input lives in.
    pub input_space: String,
    /// The semantic space this module's output lives in.
    pub output_space: String,
    /// Graph value names this module owns (its trainable parameters).
    #[serde(default)]
    pub layers: Vec<String>,
    /// Modules whose output feeds this one.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Modules sharing this name are ONE evolution unit: their parameters
    /// move together and can never be restored as two conflicting
    /// versions.
    #[serde(default)]
    pub shared_group: Option<String>,
    /// A frozen module is context for its neighbours: it may be
    /// referenced by a composition but never trained by one.
    #[serde(default)]
    pub frozen: bool,
    /// The permission closure: composing modules must not widen this.
    pub requires: ActorRole,
}

/// How expert outputs become one answer (W16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnsembleRule {
    /// Majority vote. With no available expert the result is an
    /// abstention, never a fabricated answer.
    Vote,
    /// Weighted by declared expert weights.
    Weighted,
    /// Refuse unless every declared expert produced an output in the
    /// same space; a missing expert fails the request.
    AllAgree,
}

/// One expert bound to a concrete snapshot version and the space its
/// output lives in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpertSpec {
    pub name: String,
    pub module: String,
    pub snapshot: ArtifactRef,
    /// The output space of THIS expert. Incompatible spaces are refused
    /// at composition time, not averaged together at run time.
    pub output_space: String,
    #[serde(default)]
    pub weight: f64,
}

/// Router, expert versions, combination rule and their dependencies, bound
/// into the snapshot so the ensemble is one reviewable identity rather
/// than a runtime convention.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnsembleSpec {
    /// The space the router's input features live in.
    pub router_input_space: String,
    /// The space all experts must agree on.
    pub output_space: String,
    pub rule: EnsembleRule,
    pub experts: Vec<ExpertSpec>,
    /// Hard ceiling on steps one ensemble call may consume, summed over
    /// its experts. Routing around this is not a routing decision.
    pub budget_per_call: u32,
}

/// Which principal produced this snapshot; cross-actor artifact
/// references are rejected by the store unless ownership matches.
/// Exact versions of the capability libraries the program needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSnapshot {
    pub schema: SchemaVersion,
    pub owner: String,
    pub entrypoint: String,
    pub params: Vec<ParamRef>,
    pub libraries: Vec<(String, String)>,
    pub preprocessing_version: String,
    /// Module decomposition. Empty = a monolithic model, which is what
    /// every W00–W10 record is; the field exists so composition has a
    /// place to live without a second record type.
    #[serde(default)]
    pub modules: Vec<ModuleSpec>,
    /// Router + experts, when this snapshot is an ensemble.
    #[serde(default)]
    pub ensemble: Option<EnsembleSpec>,
    /// The operator graph this snapshot executes, as the worker protocol
    /// describes it. A snapshot that cannot say what it computes is not
    /// deployable: the product would have to take the graph from the
    /// caller, and a caller-supplied graph against committed weights is
    /// how a prediction ends up describing a model nobody trained.
    #[serde(default)]
    pub graph: Option<serde_json::Value>,
}

impl ModelSnapshot {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }

    /// A monolithic snapshot: one entrypoint, no modules, no router.
    /// Every pre-W13 record is exactly this.
    pub fn new(
        owner: impl Into<String>,
        entrypoint: impl Into<String>,
        params: Vec<ParamRef>,
        preprocessing_version: impl Into<String>,
    ) -> Self {
        Self::with_graph(
            owner,
            entrypoint,
            params,
            preprocessing_version,
            None,
        )
    }

    /// The same, plus the graph the snapshot executes. A deployable
    /// snapshot always has one — see [`ModelSnapshot::graph`].
    pub fn with_graph(
        owner: impl Into<String>,
        entrypoint: impl Into<String>,
        params: Vec<ParamRef>,
        preprocessing_version: impl Into<String>,
        graph: Option<serde_json::Value>,
    ) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            owner: owner.into(),
            entrypoint: entrypoint.into(),
            params,
            libraries: Vec::new(),
            preprocessing_version: preprocessing_version.into(),
            modules: Vec::new(),
            ensemble: None,
            graph,
        }
    }
}


/// A typed content block. Bytes live in the artifact store; the record
/// carries the reference and the declared media type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub media_type: String,
    pub artifact: ArtifactRef,
}

/// Which modalities are genuinely present. A missing modality is a
/// declared fact, never a zero-filled stand-in.
/// Whether this observation's data may be used for training.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub schema: SchemaVersion,
    pub id: String,
    pub owner: String,
    pub task_id: String,
    pub session_id: String,
    pub source: String,
    pub occurred_at_ms: i64,
    pub blocks: Vec<ContentBlock>,
    pub modality_mask: Vec<bool>,
    pub training_permitted: bool,
}

impl Observation {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// The exact snapshot that produced this output. Feedback always binds
/// to this id, never to "the current head".
/// Set when the input could not be decided; abstain is a real output.
/// A prediction bound to the exact snapshot that produced it.
/// Feedback references this record, never "the current head".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prediction {
    pub schema: SchemaVersion,
    pub id: String,
    pub owner: String,
    pub observation_id: String,
    pub snapshot_digest: ArtifactRef,
    pub snapshot_schema: SchemaVersion,
    pub output: String,
    pub abstained: bool,
    pub created_at_ms: i64,
}

impl Prediction {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// The kinds the first slice of the design needs. The envelope is open, the
/// vocabulary is versioned with the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalKind {
    /// Teacher hard-output pseudo-label.
    TeacherLabel,
    /// Human correction of a specific prediction field.
    HumanCorrection,
    HumanPreference,
    Demonstration,
    /// A delayed environment result bound to the original action.
    EnvironmentResult,
    /// Supersedes an earlier signal for the same target.
    Revision,
    /// Withdraws a signal; affects later views, not frozen ones.
    Retraction,
}

impl SignalKind {
    /// Retractions and revisions point at another signal; the rest do not.
    pub fn targets_other_signal(self) -> bool {
        matches!(self, Self::Revision | Self::Retraction)
    }
}

/// Idempotency key, unique per producer. A replayed receipt is a
/// duplicate, never a second counted sample.
/// The output field this signal is about; a correction applies here and
/// nowhere else.
/// Licence under which this signal may be consumed.
/// Set for Revision / Retraction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LearningSignal {
    pub schema: SchemaVersion,
    pub id: String,
    pub idempotency_key: String,
    pub producer: String,
    pub kind: SignalKind,
    pub task_id: String,
    pub observation_id: Option<String>,
    pub prediction_id: Option<String>,
    pub target_field: Option<String>,
    pub content: String,
    pub usage_permitted: bool,
    pub occurred_at_ms: i64,
    pub received_at_ms: i64,
    pub revises: Option<String>,
}

impl LearningSignal {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// A frozen member set. Once written, its membership cannot change: later
/// signals produce a new revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetRevision {
    pub schema: SchemaVersion,
    pub id: String,
    pub task_id: String,
    pub policy_version: String,
    pub signal_ids: Vec<String>,
    pub observation_ids: Vec<String>,
    pub split: String,
    pub frozen_at_ms: i64,
}

impl DatasetRevision {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// Signal kinds this recipe is allowed to consume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    pub schema: SchemaVersion,
    pub id: String,
    pub task_id: String,
    pub consumes: Vec<SignalKind>,
    pub trainable_modules: Vec<String>,
    pub budget_steps: u32,
}

impl Recipe {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunState {
    Queued,
    Running,
    Evaluating,
    Accepted,
    Rejected,
    Paused,
    Failed,
    Cancelled,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Rejected | Self::Failed | Self::Cancelled
        )
    }
}

/// Set when the run continues a previous one; the parent keeps its own
/// record untouched.
/// One learning run: fixed contract, dataset revision and recipe.
/// A resume creates a *new* run and leaves this record untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub schema: SchemaVersion,
    pub id: String,
    pub task_id: String,
    pub base_snapshot: ArtifactRef,
    pub dataset_revision: String,
    pub recipe: String,
    pub state: RunState,
    pub steps_consumed: u32,
    pub steps_budget: u32,
    pub resumed_from: Option<String>,
}

impl Run {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// What a checkpoint can actually restore. Model-initialization is *not*
/// learning-continuation and never pretends to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResumeLevel {
    ModelInitialization,
    LearningContinuation,
    ControlledReplay,
}

/// An immutable learning state plus the manifest that commits it.
/// Resources already spent. A resume inherits this ledger rather than
/// resetting the budget.
/// An immutable learning state, restorable at a declared level.
/// Model-initialization is not learning-continuation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: SchemaVersion,
    pub id: String,
    pub owner: String,
    pub run_id: String,
    pub snapshot: ArtifactRef,
    pub parent: Option<String>,
    pub resume_level: ResumeLevel,
    pub state_artifact: ArtifactRef,
    pub budget_spent_steps: u32,
    pub created_at_ms: i64,
}

impl Checkpoint {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// A lineage DAG node: an immutable checkpoint id plus the edge that explains
/// how it was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Derivation {
    ParameterTraining,
    CodeRewrite,
    WeightMigration,
    ModuleComposition,
    Distillation,
}

/// Optimistic-concurrency counter; head updates compare against it.
/// A research direction with a comparable, versioned head pointer.
/// The branch is not the model: history stays in the checkpoints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    pub schema: SchemaVersion,
    pub id: String,
    pub owner: String,
    pub head: Option<String>,
    pub head_version: u32,
    pub policy: String,
    pub budget_quota: u32,
}

impl Branch {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Population {
    pub schema: SchemaVersion,
    pub id: String,
    pub owner: String,
    pub members: Vec<String>,
    pub total_budget_steps: u32,
    pub spent_steps: u32,
    pub policy: String,
}

impl Population {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}


/// Scores live beside a snapshot, never inside it, so the same
/// model can be re-evaluated under another protocol unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationRecord {
    pub schema: SchemaVersion,
    pub id: String,
    pub snapshot: ArtifactRef,
    pub protocol_id: String,
    pub dataset_revision: String,
    pub metrics: Vec<(String, f64)>,
    pub repeat_index: u32,
    pub device: String,
    pub completed_at_ms: i64,
}

impl EvaluationRecord {
    /// The canonical bytes this record is identified and frozen by.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_json(self)
    }
}

