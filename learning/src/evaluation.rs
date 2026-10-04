//! W07: historical re-evaluation, hard gates and publication eligibility.
//!
//! Three rules shape this module:
//!
//! 1. **Scores live beside the model, never inside it.** The same
//!    snapshot can be evaluated under many protocols; the records are
//!    append-only and comparing across protocols is refused rather than
//!    silently blended.
//! 2. **Hard gates precede everything.** A candidate with a spectacular
//!    mean but a failed gate is not publishable — the gate is not an
//!    average, and no amount of quality buys it back.
//! 3. **Publication is a separate grant.** Eligibility (evaluation) and
//!    the act of pointing the deployment at a snapshot (an
//!    expected-version-checked switch) are different checks; a stale
//!    writer loses.

use serde::{Deserialize, Serialize};

use crate::contracts::{
    require_finite_metrics, Actor, ActorRole, ArtifactRef, Error, ErrorKind, EvaluationRecord,
    Result, SCHEMA_VERSION,
};
use crate::store::Store;

/// A frozen evaluation protocol: everything two runs must share for their
/// numbers to be comparable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvaluationProtocol {
    pub schema: u32,
    pub id: String,
    pub task_id: String,
    pub dataset_revision: String,
    /// Seeds actually run; a single lucky run promotes nothing.
    pub seeds: Vec<u64>,
    /// Hard gates as metric-threshold pairs. A candidate must satisfy
    /// every gate; gates are minimums.
    pub gates: Vec<(String, f64)>,
    /// A timed-out or crashed repeat is a failure recorded at worst-case
    /// quality — it is never dropped from the denominator.
    pub timeout_counts_as_failure: bool,
    pub device: String,
}

impl EvaluationProtocol {
    pub fn new(id: &str, task_id: &str, dataset_revision: &str) -> Self {
        Self {
            schema: SCHEMA_VERSION,
            id: id.to_string(),
            task_id: task_id.to_string(),
            dataset_revision: dataset_revision.to_string(),
            seeds: Vec::new(),
            gates: Vec::new(),
            timeout_counts_as_failure: true,
            device: "cpu".to_string(),
        }
    }
}

/// One repeat's outcome, as the protocol records it. A timeout or crash
/// is recorded as a failure with the protocol's worst-case quality, so
/// the denominator never silently shrinks.
#[derive(Debug, Clone, PartialEq)]
pub enum RepeatOutcome {
    Measured { metrics: Vec<(String, f64)> },
    TimedOut,
    Crashed(String),
}

/// Record one repeat. Scores are append-only: re-evaluating an old model
/// under the same protocol adds a record, never rewrites history.
pub fn record_evaluation(
    store: &Store,
    actor: &Actor,
    protocol: &EvaluationProtocol,
    snapshot: &ArtifactRef,
    repeat_index: u32,
    outcome: &RepeatOutcome,
    now_ms: i64,
) -> Result<EvaluationRecord> {
    actor.require(ActorRole::Operator, "recording an evaluation")?;
    let metrics = match outcome {
        RepeatOutcome::Measured { metrics } => {
            // a non-finite score would round-trip to null and corrupt every
            // average built on top of it
            require_finite_metrics(metrics)?;
            metrics.clone()
        }
        RepeatOutcome::TimedOut => {
            if !protocol.timeout_counts_as_failure {
                return Err(Error::invalid(
                    "protocol exempts timeouts, which would shrink the denominator",
                ));
            }
            protocol
                .gates
                .iter()
                .map(|(name, _)| (name.clone(), 0.0))
                .collect()
        }
        RepeatOutcome::Crashed(reason) => {
            let mut metrics = protocol
                .gates
                .iter()
                .map(|(name, _)| (name.clone(), 0.0))
                .collect::<Vec<_>>();
            metrics.push(("crash_reason".to_string(), 0.0));
            let _ = reason;
            metrics
        }
    };
    let record = EvaluationRecord {
        schema: SCHEMA_VERSION,
        id: format!(
            "eval-{}-{}-{repeat_index}",
            protocol.id,
            &snapshot.to_hex()[..12]
        ),
        snapshot: *snapshot,
        protocol_id: protocol.id.clone(),
        dataset_revision: protocol.dataset_revision.clone(),
        metrics,
        repeat_index,
        device: protocol.device.clone(),
        completed_at_ms: now_ms,
    };
    store.put_evaluation(actor, &record)?;
    Ok(record)
}

/// The per-snapshot comparison row: mean quality per metric over repeats,
/// plus the hard-gate verdict under one protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub snapshot: ArtifactRef,
    pub repeats: u32,
    pub mean: Vec<(String, f64)>,
    /// Every gate satisfied by the mean quality.
    pub meets_gates: bool,
    pub gate_failures: Vec<String>,
}

/// Compare snapshots under exactly one protocol. Records from other
/// protocols are excluded here and would corrupt any average — mixing is
/// refused upstream, not blended silently.
pub fn compare(
    store: &Store,
    protocol: &EvaluationProtocol,
    snapshots: &[ArtifactRef],
) -> Result<Vec<Comparison>> {
    let mut out = Vec::new();
    for snapshot in snapshots {
        let records = store.evaluations_for(snapshot)?;
        // a snapshot evaluated under another protocol must not leak in
        let mine: Vec<&EvaluationRecord> = records
            .iter()
            .filter(|r| r.protocol_id == protocol.id)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let mut mean: Vec<(String, f64)> = Vec::new();
        for (gate, _) in &protocol.gates {
            let sum: f64 = mine
                .iter()
                .map(|r| {
                    r.metrics
                        .iter()
                        .find(|(n, _)| n == gate)
                        .map(|(_, v)| *v)
                        .unwrap_or(0.0)
                })
                .sum();
            mean.push((gate.clone(), sum / mine.len() as f64));
        }
        let mut gate_failures = Vec::new();
        for (gate, threshold) in &protocol.gates {
            let value = mean
                .iter()
                .find(|(n, _)| n == gate)
                .map(|(_, v)| *v)
                .unwrap_or(0.0);
            if value < *threshold {
                gate_failures.push(format!("{gate} {value} < {threshold}"));
            }
        }
        out.push(Comparison {
            snapshot: *snapshot,
            repeats: mine.len() as u32,
            mean,
            meets_gates: gate_failures.is_empty(),
            gate_failures,
        });
    }
    Ok(out)
}

/// Quality/cost non-dominated selection: a candidate survives unless
/// another candidate is at least as good on quality AND at least as cheap.
/// The cheap-but-slightly-worse model stays on the menu — deployment cost
/// is a real axis, not noise.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub snapshot: ArtifactRef,
    /// Mean quality under the protocol (e.g. accuracy, higher is better).
    pub quality: f64,
    /// Full deployment cost per inference (lower is better).
    pub cost: f64,
    pub meets_gates: bool,
}

pub fn non_dominated(candidates: &[Candidate]) -> Vec<&Candidate> {
    // only deployable candidates take part in the domination check: a
    // gate-failed model is not an alternative anyone can choose, so its
    // numbers must not shadow a real option
    let eligible: Vec<&Candidate> = candidates.iter().filter(|c| c.meets_gates).collect();
    eligible
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            !eligible.iter().enumerate().any(|(j, other)| {
                !(i == &j) && other.quality >= c.quality && other.cost <= c.cost
            })
        })
        .map(|(_, c)| *c)
        .collect()
}

/// Publish the snapshot that earned it. Eligibility is checked here (the
/// comparison must exist, meet every gate, and be under THIS protocol);
/// the version-checked pointer flip stays in the store.
pub fn publish_candidate(
    store: &Store,
    actor: &Actor,
    protocol: &EvaluationProtocol,
    snapshot: &ArtifactRef,
    expected_version: Option<u32>,
) -> Result<u32> {
    actor.require(ActorRole::Publisher, "publishing a model")?;
    // A snapshot whose training data was retracted is not a weaker
    // candidate, it is not a candidate: the gate check below would happily
    // pass it, and deployment is the last chance to notice.
    store.require_deployable(snapshot)?;
    let comparison = compare(store, protocol, std::slice::from_ref(snapshot))?;
    let row = comparison.first().ok_or_else(|| {
        Error::new(
            ErrorKind::IncompatibleState,
            "no evaluation under this protocol: a model cannot be published on \
             numbers from a different comparison",
        )
    })?;
    if !row.meets_gates {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!("hard gates not met: {:?}", row.gate_failures),
        ));
    }
    store.publish(actor, snapshot, expected_version)
}
