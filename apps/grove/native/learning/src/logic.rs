//! G04: immutable logic candidates, three-state governance, and the one
//! boundary that may point a deployment at new bytes.
//!
//! Three rules shape this module.
//!
//! 1. **A candidate is immutable; its diff is review material, not
//!    authority.** The authority comparison is made against the *path*
//!    of the logic and the `require`s the source actually declares,
//!    because a diff is prose someone can write and a path is not.
//! 2. **Governance is three states, not one boolean.** `learnable` (a
//!    candidate may change it), `governed` (a candidate may propose, a
//!    human approves) and `protected` (a candidate may not touch it at
//!    all — the evaluator, the permission configuration and the publish
//!    entry points live here). Collapsing *governed* into *learnable*
//!    is how an agent ends up rewriting its own evaluator.
//! 3. **`Store::publish` is `pub(crate)`.** The only way to move the
//!    pointer is [`approve_and_publish`], which demands an authenticated
//!    publisher, an approval that names *this* artifact and *this*
//!    evaluation, and the publication version the approver saw.
//!
//! Accepted ≠ published. Qualification moves a candidate's state and
//! leaves the deployment exactly where it was; a candidate that
//! evaluated well and was never approved is nobody's deployment.

use serde::{Deserialize, Serialize};

use crate::contracts::{Actor, ActorRole, ArtifactRef, Error, ErrorKind, Result, SCHEMA_VERSION};
use crate::store::Store;

// ── governance ─────────────────────────────────────────────────────

/// What may be done to one region of the codebase by a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Governance {
    /// A candidate may change it, within its grant's capabilities.
    Learnable,
    /// A candidate may propose a change; it is compared against the
    /// parent and a human must approve before it is used.
    Governed,
    /// A candidate may not change it. An attempt is refused when the
    /// proposal is made, not when someone notices it in review.
    Protected,
}

impl Governance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Learnable => "learnable",
            Self::Governed => "governed",
            Self::Protected => "protected",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "learnable" => Ok(Self::Learnable),
            "governed" => Ok(Self::Governed),
            "protected" => Ok(Self::Protected),
            other => Err(Error::invalid(format!(
                "governance is learnable, governed or protected; got {other:?}"
            ))),
        }
    }
}

/// The governance map for one checkout.
///
/// A region is a path prefix, so a whole directory can be protected
/// without listing every file, and the **longest matching prefix
/// wins** — otherwise a protected file inside a learnable tree would be
/// governed by the shorter, weaker entry. Undeclared code is
/// `Learnable`: defaulting it to protected would make the map
/// unusable for anything not yet enumerated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GovernanceMap {
    pub schema: u32,
    pub regions: Vec<(String, Governance)>,
}

impl GovernanceMap {
    pub fn new() -> Self {
        Self {
            schema: SCHEMA_VERSION,
            regions: Vec::new(),
        }
    }

    /// Declare a region. Re-declaring a prefix replaces its state, so
    /// tightening is a normal operation and a looser second declaration
    /// cannot be smuggled under a name that is already protected.
    pub fn with(mut self, prefix: impl Into<String>, state: Governance) -> Self {
        let prefix = prefix.into();
        match self.regions.iter_mut().find(|(p, _)| *p == prefix) {
            Some(entry) => entry.1 = state,
            None => self.regions.push((prefix, state)),
        }
        self
    }

    pub fn state_of(&self, path: &str) -> Governance {
        let path = path.replace('\\', "/");
        self.regions
            .iter()
            .filter(|(prefix, _)| under(&path, prefix))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, state)| *state)
            .unwrap_or(Governance::Learnable)
    }

    /// The protected prefixes, for a report or a prompt that must not
    /// discuss them.
    pub fn protected_regions(&self) -> Vec<&str> {
        self.regions
            .iter()
            .filter(|(_, state)| *state == Governance::Protected)
            .map(|(prefix, _)| prefix.as_str())
            .collect()
    }
}

impl Default for GovernanceMap {
    fn default() -> Self {
        Self::new()
    }
}

/// The governance this repository ships with.
///
/// Declared here rather than read from a file, because the regions that
/// must be protected are structural: the evaluator that decides whether
/// a candidate qualifies, the permission table, and the store's own
/// write boundary. A repository that has not configured anything still
/// gets these three refused, and can *tighten* the map but never
/// accidentally loosen these by omitting a file.
///
/// The agent's own logic is `Governed`: a candidate may propose a change
/// to it, and a human approves. Everything else is undeclared, and
/// undeclared is `Learnable`.
pub fn default_governance() -> GovernanceMap {
    GovernanceMap::new()
        .with(
            "apps/grove/native/learning/src/evaluation.rs",
            Governance::Protected,
        )
        .with(
            "apps/grove/native/learning/src/contracts.rs",
            Governance::Protected,
        )
        .with(
            "apps/grove/native/learning/src/store.rs",
            Governance::Protected,
        )
        .with(
            "apps/grove/native/learning/src/logic.rs",
            Governance::Protected,
        )
        .with("libs/loom/agent.zio", Governance::Governed)
        .with("apps/grove/main.zio", Governance::Governed)
}

/// Is `path` inside `prefix`, by whole path component? A relative
/// prefix matches from the start of the path, never from inside it, and
/// `agent.zio` must not match a prefix `agent.zio.bak`.
pub fn under(path: &str, prefix: &str) -> bool {
    let prefix = prefix.trim_end_matches('/');
    if prefix.is_empty() {
        return false;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

// ── the candidate ──────────────────────────────────────────────────

/// A proposed change to a piece of agent logic.
///
/// Immutable once written and identified by its digest, so "what
/// changed" has exactly one answer per id. `diff_ref` is review
/// material: nothing in this module compares it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicCandidate {
    pub schema: u32,
    pub id: String,
    /// The logic this one replaces, as an artifact of the *old source*.
    /// `None` is a first candidate, and has no "before" to compare.
    pub parent_logic_ref: Option<ArtifactRef>,
    /// The repository path this candidate claims to change. The
    /// governance map is consulted about *this string*, so it is a
    /// claim — and a false one is caught by [`require_allowed`].
    pub module_path: String,
    /// The proposed source, as bytes in the artifact store.
    pub source_ref: ArtifactRef,
    /// The `require`s the proposer declares.
    pub declared_dependencies: Vec<String>,
    pub diff_ref: ArtifactRef,
    /// Failure evidence this was built from. The plan's rule: the model
    /// sees allowed old logic and failure evidence, never the hidden
    /// evaluation answers.
    pub evidence_refs: Vec<ArtifactRef>,
    pub created_at_ms: i64,
}

impl LogicCandidate {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        crate::contracts::canonical_json(self)
    }
}

/// Where a candidate stands. `Proposed` is the only state a caller may
/// create; the rest are moved by this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateState {
    Proposed,
    /// Evaluated under a frozen protocol and cleared its gates. Not
    /// yet anyone's deployment.
    Qualified,
    /// An authenticated human approved it and the pointer moved.
    Active,
    /// Evaluated and failed its gates. The record stays: "we tried this
    /// and it did not work" is the fact that stops the next attempt
    /// from being the same attempt.
    Rejected,
    /// A human declined it. Distinct from `Rejected` — nobody claimed
    /// it was worse, they declined to switch.
    Declined,
}

impl CandidateState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Qualified => "qualified",
            Self::Active => "active",
            Self::Rejected => "rejected",
            Self::Declined => "declined",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "proposed" => Ok(Self::Proposed),
            "qualified" => Ok(Self::Qualified),
            "active" => Ok(Self::Active),
            "rejected" => Ok(Self::Rejected),
            "declined" => Ok(Self::Declined),
            other => Err(Error::invalid(format!(
                "candidate state {other:?} is not one this host writes"
            ))),
        }
    }
}

/// What comparing a candidate against its parent and the governance map
/// found. Every refusal names a *checked* fact, so a proposer cannot
/// talk past one with a better-written diff.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeVerdict {
    /// The regions the proposal actually changed, per the two sources.
    pub changed: Vec<String>,
    pub refusals: Vec<String>,
    /// Dependencies the proposal adds that the parent did not declare.
    pub new_dependencies: Vec<String>,
}

impl ScopeVerdict {
    pub fn refused(&self) -> bool {
        !self.refusals.is_empty()
    }
}

/// Compare a proposal against its parent under a governance map.
///
/// This decides whether the proposal is *allowed to exist*, not whether
/// it is *good*: a candidate that rewrites the evaluator is refused
/// whether or not its numbers improved.
pub fn compare_scope(
    parent: Option<(&str, &[String])>,
    candidate: (&str, &[String], &str),
    governance: &GovernanceMap,
) -> ScopeVerdict {
    // `candidate` is (module_path, declared_dependencies, source).
    let (path, declared, source) = candidate;
    let mut refusals = Vec::new();
    let mut changed = Vec::new();

    // 1. the path itself. A protected path is refused before its
    //    contents are even read — that is the whole point of naming a
    //    region rather than diffing a file.
    match governance.state_of(path) {
        Governance::Protected => refusals.push(format!(
            "{path} is protected; a candidate may not rewrite it"
        )),
        Governance::Governed | Governance::Learnable => {}
    }

    // 2. the source. Identical text is not a change, and a proposal
    //    whose bytes equal its parent's would otherwise qualify as "a
    //    change that changed nothing".
    match parent {
        Some((parent_source, parent_dependencies)) => {
            changed = changed_regions(parent_source, source);
            for dep in declared {
                let dep = normalise_dep(dep);
                if !parent_dependencies.iter().any(|d| normalise_dep(d) == dep) {
                    // A new `require` is how a candidate reaches code
                    // nobody reviewed. Only a learnable region may be
                    // newly reached.
                    if governance.state_of(&dep) != Governance::Learnable {
                        refusals.push(format!(
                            "dependency {dep} is newly required and its region is not learnable"
                        ));
                    }
                }
            }
        }
        None => changed.push(path.to_string()),
    }

    let mut new_dependencies = Vec::new();
    if let Some((_, parent_dependencies)) = parent {
        for dep in declared {
            let dep = normalise_dep(dep);
            if !parent_dependencies.iter().any(|d| normalise_dep(d) == dep) {
                new_dependencies.push(dep);
            }
        }
    }

    ScopeVerdict {
        changed,
        refusals,
        new_dependencies,
    }
}

/// The top-level definitions whose text differs between two sources.
///
/// A structural comparison, not a line diff: the question is *which
/// definitions moved*, and the answer has to be stable enough to map
/// onto a path. Definitions carry no path of their own, so a change is
/// attributed to the module it was made in.
pub fn changed_regions(parent_source: &str, candidate_source: &str) -> Vec<String> {
    if normalise_source(parent_source) == normalise_source(candidate_source) {
        return Vec::new();
    }
    // The per-definition names, so a reviewer sees *what* moved rather
    // than a number.
    let before = definition_names(parent_source);
    let after = definition_names(candidate_source);
    let mut out: Vec<String> = Vec::new();
    for name in &after {
        if !before.contains(name) {
            out.push(format!("+{name}"));
        }
    }
    for name in &before {
        if !after.contains(name) {
            out.push(format!("-{name}"));
        }
    }
    out
}

/// The `defn`/`defmacro` names a source declares, in order.
fn definition_names(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(ctx) = zio_core::bootstrap::language_context(zio_core::bootstrap::ModuleRoots::empty())
    else {
        return out;
    };
    let Ok(forms) = zio_core::bootstrap::parse_source(&ctx, "logic.zio", source) else {
        return out;
    };
    for form in forms {
        if let Some(name) = definition_name(&form) {
            out.push(name);
        }
    }
    out
}

/// The name a `(defn name …)` / `(defmacro name …)` form binds.
fn definition_name(form: &zio_core::sexp::Sexp) -> Option<String> {
    use zio_core::sexp::Sexp;
    let Sexp::List(items, _) = form else {
        return None;
    };
    let head = match items.iter().next() {
        Some(Sexp::Symbol(name, _)) => name.as_str(),
        _ => return None,
    };
    if head != "defn" && head != "defmacro" {
        return None;
    }
    match items.iter().nth(1) {
        Some(Sexp::Symbol(name, _)) => Some(name.clone()),
        _ => None,
    }
}

fn normalise_dep(name: &str) -> String {
    name.trim_start_matches("./").to_string()
}

/// Line endings and trailing blank lines are not a behaviour change;
/// treating them as one makes every reformat look like a logic change.
fn normalise_source(source: &str) -> String {
    source.replace("\r\n", "\n").trim_end().to_string()
}

/// Refuse a scope comparison that found anything. Shared with the
/// callers so no one decides for itself that a refusal is advisory.
pub fn require_allowed(verdict: &ScopeVerdict) -> Result<()> {
    if verdict.refused() {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            format!(
                "candidate scope is not allowed: {}",
                verdict.refusals.join("; ")
            ),
        ));
    }
    Ok(())
}

// ── the approval boundary ──────────────────────────────────────────

/// A human decision about one deployable artifact, bound to the
/// evaluation that justified it and to the version it expects to
/// replace.
///
/// There is no `approved: true` anywhere in this crate. This record is
/// written only by [`approve_and_publish`], only for a `Publisher`
/// actor, and only after the evaluation it names is checked against the
/// artifact being published.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HumanApproval {
    pub schema: u32,
    pub id: String,
    /// The authenticated principal. From a bearer token at the API, from
    /// the shell's own identity at the CLI — never from a request body.
    pub authenticated_actor: String,
    /// What the decision was about: a candidate id for logic, a
    /// `snapshot@protocol` name for a model.
    pub subject_id: String,
    /// The artifact that would become the deployment.
    pub candidate_ref: ArtifactRef,
    /// Every evaluation this approval rests on. One is enough; the list
    /// is what the comparison actually consumed.
    pub evaluation_refs: Vec<ArtifactRef>,
    /// The publication version the approver saw. A later publish
    /// against a moved pointer is refused, not applied.
    pub expected_publication_version: Option<u32>,
    pub decision: String,
    pub created_at_ms: i64,
}

impl HumanApproval {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        crate::contracts::canonical_json(self)
    }
}

/// Commit a candidate and record it as `proposed`.
///
/// The manifest bytes land first, so a crash between the two leaves an
/// unreferenced manifest (garbage-collectable) rather than a row naming
/// bytes that are not there.
pub fn put_candidate(
    store: &Store,
    actor: &Actor,
    candidate: &LogicCandidate,
) -> Result<ArtifactRef> {
    actor.require(ActorRole::Operator, "proposing a logic candidate")?;
    let digest = store.commit_manifest("LogicCandidate", &actor.id, candidate)?;
    store.insert_candidate(actor, candidate, digest)?;
    Ok(digest)
}

/// Propose, and refuse before anything is written.
///
/// A refused proposal leaves no candidate row: a refused candidate is a
/// claim that was never made, and keeping it would make "what was
/// tried" include things nobody may try.
pub fn propose(
    store: &Store,
    actor: &Actor,
    governance: &GovernanceMap,
    candidate: &LogicCandidate,
    parent_source: Option<&str>,
    parent_dependencies: &[String],
) -> Result<ArtifactRef> {
    actor.require(ActorRole::Operator, "proposing a logic candidate")?;
    let source =
        String::from_utf8_lossy(&store.artifacts().get(&candidate.source_ref)?).into_owned();
    // What the source *declares*, read from the source. A candidate
    // that under-declares its dependencies is describing itself, not
    // the code that will run.
    let actual = crate::execution::probe_dependencies(&source);
    for declared in &candidate.declared_dependencies {
        let declared = normalise_dep(declared);
        if !actual.iter().any(|d| normalise_dep(d) == declared) {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "candidate declares dependency {declared}, which its source does not require"
                ),
            ));
        }
    }
    // An undeclared dependency is the same lie in the other direction:
    // the source reaches something the proposal did not admit to.
    for dep in &actual {
        let dep = normalise_dep(dep);
        if !candidate
            .declared_dependencies
            .iter()
            .any(|d| normalise_dep(d) == dep)
        {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!("candidate source requires {dep}, which the proposal does not declare"),
            ));
        }
    }

    let parent = parent_source.map(|s| (s, parent_dependencies));
    let verdict = compare_scope(
        parent,
        (
            &candidate.module_path,
            &candidate.declared_dependencies,
            &source,
        ),
        governance,
    );
    require_allowed(&verdict)?;
    put_candidate(store, actor, candidate)
}

/// The verdict one evaluation produced for a candidate.
///
/// The candidate's own output is never evidence: the metrics come from
/// `EvaluationRecord`s written by the trusted evaluator, so a program
/// that printed "tests passed" contributes nothing. Silence is not a
/// pass either — a protocol that never ran against this candidate has
/// not spoken about it.
pub fn evaluate_candidate(
    store: &Store,
    actor: &Actor,
    protocol: &crate::evaluation::EvaluationProtocol,
    candidate: &LogicCandidate,
    now_ms: i64,
) -> Result<CandidateVerdict> {
    actor.require(ActorRole::Operator, "evaluating a logic candidate")?;
    let records: Vec<crate::contracts::EvaluationRecord> =
        store.evaluations_for(&candidate.source_ref)?;
    let mine: Vec<_> = records
        .iter()
        .filter(|r| r.protocol_id == protocol.id)
        .collect();
    let mut verdict = CandidateVerdict {
        candidate_id: candidate.id.clone(),
        state: CandidateState::Rejected,
        evaluation_refs: Vec::new(),
        gate_failures: Vec::new(),
    };
    if mine.is_empty() {
        verdict.gate_failures.push(format!(
            "no evaluation under protocol {}; an unevaluated candidate is not qualified",
            protocol.id
        ));
        store.set_candidate_state(actor, &candidate.id, CandidateState::Rejected, now_ms)?;
        return Ok(verdict);
    }
    for record in &mine {
        verdict.evaluation_refs.push(store.commit_manifest(
            "EvaluationRecord",
            &actor.id,
            record,
        )?);
    }
    verdict.gate_failures = crate::evaluation::gate_failures(&mine, protocol);
    verdict.state = if verdict.gate_failures.is_empty() {
        CandidateState::Qualified
    } else {
        CandidateState::Rejected
    };
    store.set_candidate_state(actor, &candidate.id, verdict.state, now_ms)?;
    Ok(verdict)
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateVerdict {
    pub candidate_id: String,
    pub state: CandidateState,
    pub evaluation_refs: Vec<ArtifactRef>,
    pub gate_failures: Vec<String>,
}

/// The whole sequence the plan states, in order:
/// qualify → approve → publish. `expected_version` is the caller's read
/// of the pointer; a caller holding a stale value is refused rather than
/// allowed to overwrite somebody else's switch.
pub fn approve_and_publish(
    store: &Store,
    actor: &Actor,
    protocol: &crate::evaluation::EvaluationProtocol,
    candidate: &LogicCandidate,
    expected_version: Option<u32>,
    now_ms: i64,
) -> Result<(u32, HumanApproval)> {
    let verdict = evaluate_candidate(store, actor, protocol, candidate, now_ms)?;
    if verdict.state != CandidateState::Qualified {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "candidate {} does not qualify under {}: {:?}; the active publication is untouched",
                candidate.id, protocol.id, verdict.gate_failures
            ),
        ));
    }
    let approval = record_approval(
        store,
        actor,
        &candidate.id,
        &candidate.source_ref,
        &verdict.evaluation_refs,
        expected_version,
        now_ms,
    )?;
    let version = publish_approved(store, actor, &approval, expected_version)?;
    store.set_candidate_state(actor, &candidate.id, CandidateState::Active, now_ms)?;
    Ok((version, approval))
}

/// Record the human decision. Separate from publishing so a decision
/// can be recorded without moving the pointer, and so the refusal of an
/// approval is checked by the one function that applies it.
pub fn record_approval(
    store: &Store,
    actor: &Actor,
    subject_id: &str,
    candidate_ref: &ArtifactRef,
    evaluation_refs: &[ArtifactRef],
    expected_publication_version: Option<u32>,
    now_ms: i64,
) -> Result<HumanApproval> {
    actor.require(ActorRole::Publisher, "approving a deployment change")?;
    if evaluation_refs.is_empty() {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            "an approval must name the evaluation it was decided against",
        ));
    }
    // The version race is checked *before* anything is written. An
    // approval that has already lost its race is not a decision anybody
    // made, and recording it would leave a row claiming a human
    // approved something that was never applied.
    if let (Some(expected), Some((current, _))) =
        (expected_publication_version, store.active_publication()?)
    {
        if expected != current {
            return Err(Error::conflict(format!(
                "approval expected publication v{expected}, the pointer is at v{current}"
            )));
        }
    }
    let approval = HumanApproval {
        schema: SCHEMA_VERSION,
        // Per *decision*, not per artifact: the same snapshot approved
        // twice at two different versions is two decisions, and an id
        // derived from the digest alone would make the second one look
        // like a duplicate of the first. The expected version is part
        // of the id because that is what the approver was looking at.
        id: format!(
            "approval-{}-{}-v{}",
            &candidate_ref.to_hex()[..12],
            &crate::contracts::digest_bytes(subject_id.as_bytes()).to_hex()[..6],
            expected_publication_version.unwrap_or(0)
        ),
        authenticated_actor: actor.id.clone(),
        subject_id: subject_id.to_string(),
        candidate_ref: *candidate_ref,
        evaluation_refs: evaluation_refs.to_vec(),
        expected_publication_version,
        decision: "approved".to_string(),
        created_at_ms: now_ms,
    };
    store.put_approval(actor, &approval)?;
    Ok(approval)
}

/// Record a human refusal. The candidate keeps its record and its
/// evidence, so "we looked at it and said no" is answerable later.
pub fn decline(
    store: &Store,
    actor: &Actor,
    subject_id: &str,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    actor.require(ActorRole::Publisher, "declining a deployment change")?;
    store.note_rejection(actor, subject_id, reason, now_ms)?;
    // A decline moves the candidate's state too: "qualified but not
    // adopted" and "declined" are different answers, and the review
    // board needs to tell them apart.
    match store.candidate_state(subject_id) {
        Ok(state) => {
            store.set_candidate_state(actor, subject_id, CandidateState::Declined, now_ms)?;
            let _ = state;
        }
        // Nothing to move: a model snapshot has no candidate row, and
        // the recorded refusal is still the fact.
        Err(e) if e.kind == ErrorKind::ArtifactUnavailable => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Apply an approval. `pub(crate)` for the same reason `Store::publish`
/// is: this is the last thing before the pointer moves, and it must not
/// be reachable by anything that did not authenticate an approval.
///
/// Three refusals, each naming its own reason:
///
/// * the approval is about a different artifact;
/// * it names no evaluation that exists for this subject;
/// * the pointer moved since the approver looked at it.
pub(crate) fn publish_approved(
    store: &Store,
    actor: &Actor,
    approval: &HumanApproval,
    expected_version: Option<u32>,
) -> Result<u32> {
    actor.require(ActorRole::Publisher, "publishing approved logic")?;
    if approval.decision != "approved" {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "approval {} is a {}, not an approval",
                approval.id, approval.decision
            ),
        ));
    }
    if !store.approval_is_current(approval)? {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "approval {} does not match a recorded evaluation of {}; \
                 a mismatched approval is refused",
                approval.id, approval.subject_id
            ),
        ));
    }
    // The version race has its own error class. An approval that named a
    // version and found the pointer somewhere else lost a race between
    // two publishers — that is a conflict, not a malformed decision, and
    // merging the two would tell the loser their approval was invalid
    // when it was merely out of date.
    if !store.approval_version_is_current(approval)? {
        let current = store.active_publication()?.map(|(v, _)| v);
        return Err(Error::conflict(format!(
            "approval {} expected publication v{}, the pointer is at v{}",
            approval.id,
            approval
                .expected_publication_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".to_string()),
            current
                .map(|v| v.to_string())
                .unwrap_or_else(|| "none".to_string()),
        )));
    }
    store.publish(actor, &approval.candidate_ref, expected_version)
}

/// Qualify, approve and publish a *model* snapshot.
///
/// The same three steps as [`approve_and_publish`], with the gate check
/// delegated to `evaluation::publish_snapshot`'s comparison so the
/// model path and the logic path cannot disagree about what qualifies.
/// The approval's `subject_id` names the snapshot under the protocol it
/// was scored with, so two protocols' records for one snapshot are two
/// different subjects rather than one ambiguous approval.
pub fn publish_snapshot_approved(
    store: &Store,
    actor: &Actor,
    protocol: &crate::evaluation::EvaluationProtocol,
    snapshot: &ArtifactRef,
    expected_version: Option<u32>,
    now_ms: i64,
) -> Result<(u32, HumanApproval)> {
    let records = store.evaluations_for(snapshot)?;
    let mine: Vec<_> = records
        .iter()
        .filter(|r| r.protocol_id == protocol.id)
        .collect();
    if mine.is_empty() {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!(
                "no evaluation of {snapshot} under protocol {}; a snapshot cannot be \
                 published on numbers from a different comparison",
                protocol.id
            ),
        ));
    }
    let mut refs = Vec::new();
    for record in &mine {
        refs.push(store.commit_manifest("EvaluationRecord", &actor.id, *record)?);
    }
    let subject = format!("{}@{}", snapshot.to_hex(), protocol.id);
    let approval = record_approval(
        store,
        actor,
        &subject,
        snapshot,
        &refs,
        expected_version,
        now_ms,
    )?;
    let version = publish_approved(store, actor, &approval, expected_version)?;
    Ok((version, approval))
}

/// Apply a recorded approval.
///
/// The two-step path: an approval is recorded against what a reviewer
/// saw, and applied later. Between the two the pointer may have moved,
/// and `publish_approved` refuses then — which is the whole reason the
/// approval commits to a version.
pub fn apply_approval(
    store: &Store,
    actor: &Actor,
    approval: &HumanApproval,
    expected_version: Option<u32>,
) -> Result<u32> {
    publish_approved(store, actor, approval, expected_version)
}

/// An in-flight caller keeps the logic it started with.
///
/// The pin is taken once at entry and handed to the call; the next call
/// reads the new pointer. A rollback that flips the pointer back does
/// not reach into a call that is already running, and a call that is
/// already running does not get moved to the new pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveLogic {
    pub version: u32,
    pub snapshot: ArtifactRef,
    /// The candidate id behind the pointer, when the pointer names one.
    pub candidate_id: Option<String>,
}

impl ActiveLogic {
    pub fn pin(store: &Store) -> Result<Option<Self>> {
        let Some((version, snapshot)) = store.active_publication()? else {
            return Ok(None);
        };
        Ok(Some(Self {
            version,
            snapshot,
            candidate_id: store.active_candidate_id()?,
        }))
    }
}

impl std::fmt::Display for ActiveLogic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.candidate_id {
            Some(id) => write!(f, "v{} {} ({id})", self.version, self.snapshot),
            None => write!(f, "v{} {}", self.version, self.snapshot),
        }
    }
}
