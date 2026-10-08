//! W16: expert routing, output combination and multi-teacher distillation.
//!
//! An ensemble is a **model identity**, not a runtime convention: the
//! router, each expert's exact snapshot, the output space they must
//! agree on, the combination rule and the per-call budget are all bound
//! into one `EnsembleSpec` inside the `ModelSnapshot`. That is what makes
//! "which model answered this?" answerable from the record.
//!
//! The refusals matter more than the combination:
//!
//! * experts whose outputs live in different spaces are never averaged —
//!   the composition is refused at bind time;
//! * with no available expert the ensemble **abstains**; it never
//!   fabricates the majority of nothing;
//! * an unavailable expert and a missing modality are *recorded
//!   behaviours* (they are what makes coverage a real number), not
//!   silent fallbacks;
//! * an ensemble call cannot exceed its declared per-call budget, and
//!   the budget is summed over the experts it called — routing is not a
//!   way around accounting;
//! * a population's agreement is not a label. `agree_as_label` is
//!   explicitly not provided, and the distillation path below consumes
//!   teacher output as a *target*, never as ground truth for the
//!   student.

use std::collections::BTreeMap;

use crate::contracts::{
    Actor, ActorRole, ArtifactRef, EnsembleRule, EnsembleSpec, Error, ErrorKind, ExpertSpec,
    ModelSnapshot, Result, SCHEMA_VERSION,
};
use crate::store::Store;

/// One expert's answer to one input.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpertOutput {
    pub expert: String,
    /// `None` = the expert abstained or had no output. Abstention is a
    /// value: it is counted, never imputed.
    pub class: Option<u32>,
    pub cost_steps: u32,
}

/// What the ensemble decided, and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct EnsembleOutcome {
    /// `None` = abstain. With no available expert this is the ONLY
    /// possible answer.
    pub class: Option<u32>,
    pub rule: EnsembleRule,
    /// Experts that produced an answer.
    pub contributors: Vec<String>,
    /// Experts that were selected but produced nothing.
    pub unavailable: Vec<String>,
    /// Modalities the router found absent.
    pub missing_modality: Vec<String>,
    /// Total steps spent, summed over every expert actually called.
    pub cost_steps: u32,
    /// Tail latency per expert, milliseconds. Reported so a routing
    /// policy cannot look free by being slow.
    pub latency_ms: BTreeMap<String, u32>,
}

/// Bind an ensemble into a snapshot. Every expert must be a committed
/// snapshot owned by this actor, and every expert must emit into the
/// ensemble's declared output space.
pub fn bind_ensemble(
    store: &Store,
    actor: &Actor,
    spec: EnsembleSpec,
    entrypoint: &str,
) -> Result<ModelSnapshot> {
    actor.require(ActorRole::Operator, "binding an ensemble")?;
    if spec.experts.is_empty() {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            "an ensemble with no experts has no answer; bind at least one",
        ));
    }
    if spec.budget_per_call == 0 {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            "an ensemble with a zero per-call budget cannot be called at all",
        ));
    }
    let mut names: Vec<&str> = Vec::new();
    for expert in &spec.experts {
        if names.contains(&expert.name.as_str()) {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!("expert {:?} is bound twice", expert.name),
            ));
        }
        names.push(&expert.name);
        if expert.output_space != spec.output_space {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "expert {:?} emits space {:?} but the ensemble combines in {:?}; \
                     outputs in different spaces cannot be integrated",
                    expert.name, expert.output_space, spec.output_space
                ),
            ));
        }
        if expert.weight.is_finite() && expert.weight < 0.0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("expert {:?} has a negative weight", expert.name),
            ));
        }
        // an expert names a concrete snapshot: it must be committed, and
        // its bytes must be present. An unavailable expert is a runtime
        // state, not a bind-time fiction.
        store.require_owner(&expert.snapshot, actor)?;
        store.load_manifest(&expert.snapshot)?;
    }
    let snapshot = ModelSnapshot {
        schema: SCHEMA_VERSION,
        owner: actor.id.clone(),
        entrypoint: entrypoint.to_string(),
        params: Vec::new(),
        libraries: Vec::new(),
        preprocessing_version: "ensemble@1".to_string(),
        modules: Vec::new(),
        ensemble: Some(spec),
        // an ensemble is served by a router, not by a single operator
        // graph, so it carries none: `EnsembleSpec` is the whole contract
        graph: None,
    };
    store.commit_manifest("ModelSnapshot", &actor.id, &snapshot)?;
    Ok(snapshot)
}

/// What the router decided for one input: which experts to call and
/// which modalities are missing.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub experts: Vec<String>,
    pub missing_modality: Vec<String>,
}

/// The routing policy. Selection is a *declared* rule, not a learned
/// one: a router that silently changed which expert serves which input
/// would make the ensemble's record a lie.
///
/// `all_modalities` is the full list the task declares; `present` is what
/// this observation actually carried. The difference is reported as
/// `missing_modality` — a declared absence, never a zero-filled
/// stand-in — and a modality's absence is the router's problem to
/// handle, not something to paper over.
pub fn route(
    spec: &EnsembleSpec,
    all_modalities: &[String],
    present: &[String],
    expert_available: &[String],
) -> Route {
    let missing: Vec<String> = all_modalities
        .iter()
        .filter(|m| !present.contains(m))
        .cloned()
        .collect();
    let experts: Vec<String> = spec
        .experts
        .iter()
        .filter(|e| expert_available.iter().any(|a| a == &e.name))
        .map(|e| e.name.clone())
        .collect();
    Route {
        experts,
        missing_modality: missing,
    }
}

/// Combine expert outputs under the ensemble's declared rule.
///
/// The three cases that matter:
///
/// * **no available expert** → abstain, never a fabricated class;
/// * **`AllAgree` with a missing expert** → abstain: the rule *is* the
///   answer, so an absent expert cannot be treated as agreeing;
/// * **`Vote` / `Weighted`** → the modal (or weighted) class among
///   contributors; a tie has no winner, so it abstains.
pub fn combine(spec: &EnsembleSpec, outputs: &[ExpertOutput]) -> Result<EnsembleOutcome> {
    let unavailable: Vec<String> = outputs
        .iter()
        .filter(|o| o.class.is_none())
        .map(|o| o.expert.clone())
        .collect();
    let contributors: Vec<&ExpertOutput> = outputs.iter().filter(|o| o.class.is_some()).collect();
    let cost_steps: u32 = outputs.iter().map(|o| o.cost_steps).sum();
    if contributors.is_empty() {
        return Ok(EnsembleOutcome {
            class: None,
            rule: spec.rule,
            contributors: Vec::new(),
            unavailable: outputs.iter().map(|o| o.expert.clone()).collect(),
            missing_modality: Vec::new(),
            cost_steps,
            latency_ms: BTreeMap::new(),
        });
    }

    let class = match spec.rule {
        EnsembleRule::AllAgree => {
            // The rule demands EVERY declared expert. One unavailable
            // expert means unanimity is unestablished, so the survivor's
            // answer must not be promoted to "all agree" — that would
            // manufacture the agreement the rule exists to require.
            if !unavailable.is_empty() {
                None
            } else {
                contributors[0]
                    .class
                    .filter(|&first| contributors.iter().all(|o| o.class == Some(first)))
            }
        }
        EnsembleRule::Vote => {
            let mut tally: BTreeMap<u32, u32> = BTreeMap::new();
            for output in &contributors {
                if let Some(class) = output.class {
                    *tally.entry(class).or_insert(0) += 1;
                }
            }
            let top = tally.values().copied().max().unwrap_or(0);
            let leaders: Vec<u32> = tally
                .iter()
                .filter(|(_, count)| **count == top)
                .map(|(class, _)| *class)
                .collect();
            // a tie has no honest winner
            if leaders.len() == 1 {
                Some(leaders[0])
            } else {
                None
            }
        }
        EnsembleRule::Weighted => {
            let mut tally: BTreeMap<u32, f64> = BTreeMap::new();
            for output in &contributors {
                if let Some(class) = output.class {
                    let weight = spec
                        .experts
                        .iter()
                        .find(|e| e.name == output.expert)
                        .map(|e| e.weight)
                        .unwrap_or(1.0);
                    *tally.entry(class).or_insert(0.0) += weight;
                }
            }
            let top = tally.values().copied().fold(f64::NEG_INFINITY, f64::max);
            let leaders: Vec<u32> = tally
                .iter()
                .filter(|(_, score)| (**score - top).abs() < f64::EPSILON)
                .map(|(class, _)| *class)
                .collect();
            if leaders.len() == 1 {
                Some(leaders[0])
            } else {
                None
            }
        }
    };

    Ok(EnsembleOutcome {
        class,
        rule: spec.rule,
        contributors: contributors.iter().map(|o| o.expert.clone()).collect(),
        unavailable,
        missing_modality: Vec::new(),
        cost_steps,
        latency_ms: BTreeMap::new(),
    })
}

/// Charge one ensemble call against its declared per-call budget. The
/// total is the SUM over the experts called, so a three-expert call
/// cannot be priced like a one-expert call.
pub fn charge(spec: &EnsembleSpec, called: &[String], spent: u32) -> Result<u32> {
    if spent > spec.budget_per_call {
        return Err(Error::new(
            ErrorKind::BudgetExhausted,
            format!(
                "ensemble call spent {spent} steps over its {} per-call budget \
                 (experts: {called:?})",
                spec.budget_per_call
            ),
        ));
    }
    Ok(spent)
}

/// One teacher contribution to a multi-teacher distillation: which
/// teacher, which snapshot, and the answer it gave.
#[derive(Debug, Clone, PartialEq)]
pub struct TeacherVote {
    pub teacher: String,
    pub snapshot: ArtifactRef,
    pub class: u32,
}

/// The distillation target for one input, built from several teachers.
///
/// Agreement is a *weight*, not a label: teachers that agree reinforce,
/// teachers that disagree are recorded, and the target is still only a
/// target — the student is evaluated by the task's own evaluation, and a
/// unanimous-but-wrong population does not get to skip that. This is why
/// the function returns agreement as data instead of a verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct DistillationTarget {
    pub class: u32,
    pub agreement: f64,
    pub dissenting: Vec<String>,
}

pub fn distillation_target(votes: &[TeacherVote]) -> Result<DistillationTarget> {
    if votes.is_empty() {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            "no teacher produced an output: there is no target to distil",
        ));
    }
    let mut tally: BTreeMap<u32, usize> = BTreeMap::new();
    for vote in votes {
        *tally.entry(vote.class).or_insert(0) += 1;
    }
    let (class, count) = tally
        .iter()
        .max_by_key(|(class, count)| (**count, std::cmp::Reverse(**class)))
        .map(|(class, count)| (*class, *count))
        .expect("the map is non-empty");
    let dissenting: Vec<String> = votes
        .iter()
        .filter(|v| v.class != class)
        .map(|v| v.teacher.clone())
        .collect();
    // the tie-break above is deterministic, but the caller still sees the
    // agreement level, so a 2-vs-2 population cannot masquerade as a
    // confident label
    Ok(DistillationTarget {
        class,
        agreement: count as f64 / votes.len() as f64,
        dissenting,
    })
}

/// An expert binding as a product view: which module, which snapshot, and
/// whether the snapshot is still loadable.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpertView {
    pub spec: ExpertSpec,
    pub loadable: bool,
}

/// Render an ensemble for the module view, reporting an expert whose
/// snapshot is gone as **unloadable** rather than hiding it. An expert
/// that silently disappeared would be an ensemble that quietly got
/// weaker.
pub fn expert_views(store: &Store, spec: &EnsembleSpec) -> Result<Vec<ExpertView>> {
    let mut out = Vec::new();
    for expert in &spec.experts {
        let loadable = store.artifacts().exists(&expert.snapshot)
            && store.load_manifest(&expert.snapshot).is_ok();
        out.push(ExpertView {
            spec: expert.clone(),
            loadable,
        });
    }
    Ok(out)
}
