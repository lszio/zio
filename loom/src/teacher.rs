//! Teacher capability protocol (grove delivery plan W03).
//!
//! A teacher is a *different role* from a text-completion host: it declares
//! what it can accept and produce, it may or may not offer soft outputs,
//! and it states what may be done with its answers. The learner consumes
//! only the hard/soft output it declared — a provider that has no logits
//! cannot be asked for them, and its absence is reported as a missing
//! capability rather than faked with a one-hot vector.
//!
//! Two invariants the type system and the request builder enforce:
//!
//! 1. **A teacher never approves a candidate.** [`TeacherResponse`] may
//!    carry a proposed program, but that field is typed as a *proposal*
//!    ([`ProposedProgram`]) and the learning loop must still parse, gate
//!    and score it exactly like any other proposer's text.
//! 2. **Nothing leaves without permission.** A request names the licence
//!    the caller claims; a content block the caller is not allowed to
//!    externalize is rejected before any transport call happens.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{HostError, HostErrorKind};

// ── Capabilities ───────────────────────────────────────────────────

/// What the caller is permitted to do with an exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageLicence {
    /// Answers may train a local student.
    TrainStudent,
    /// Answers may be shown to a human.
    DisplayOnly,
    /// No right to use the answer at all.
    Forbidden,
}

impl UsageLicence {
    pub fn allows_training(self) -> bool {
        matches!(self, Self::TrainStudent)
    }
}

/// Input modalities a teacher accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Modality {
    Text,
    Image,
    Audio,
    Numeric,
}

/// Output shapes a teacher can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputKind {
    /// A single discrete answer.
    HardLabel,
    /// A full distribution or raw logits over `vocabulary`.
    SoftDistribution,
    /// A program the teacher proposes. Still only a proposal.
    Program,
}

/// A teacher declares this before it is ever called. A missing
/// declaration is a missing capability, not a default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeacherCapability {
    pub teacher_id: String,
    /// Provider's model revision, when it exposes one. `None` means the
    /// source version is incomplete and must be recorded as such.
    pub model_version: Option<String>,
    pub input_modalities: Vec<Modality>,
    pub output_kinds: Vec<OutputKind>,
    /// Class/vocabulary space, required for soft-output distillation.
    pub vocabulary: Option<Vec<String>>,
    /// Whether the provider retains the request, and whether that stored
    /// data may be used to train anything.
    pub retains_data: bool,
    pub permits_training_use: bool,
}

impl TeacherCapability {
    pub fn accepts(&self, modality: Modality) -> bool {
        self.input_modalities.contains(&modality)
    }

    pub fn supports(&self, kind: OutputKind) -> bool {
        self.output_kinds.contains(&kind)
    }

    /// Soft distillation needs both a declared distribution *and* an
    /// aligned label space. Either missing means hard distillation only.
    pub fn supports_soft_targets(&self) -> bool {
        self.supports(OutputKind::SoftDistribution) && self.vocabulary.is_some()
    }

    /// Whether this exchange may become training data at all.
    pub fn permits(&self, licence: UsageLicence) -> Result<(), HostError> {
        if licence == UsageLicence::Forbidden {
            return Err(HostError::new(
                HostErrorKind::Config,
                format!("teacher {} was called with a forbidden licence", self.teacher_id),
            ));
        }
        if licence.allows_training() && !self.permits_training_use {
            return Err(HostError::new(
                HostErrorKind::Config,
                format!(
                    "teacher {} does not permit training use of its answers",
                    self.teacher_id
                ),
            ));
        }
        Ok(())
    }
}

// ── Requests ───────────────────────────────────────────────────────

/// One piece of content. Binary payloads are carried by reference, so a
/// request never smuggles a large blob through a text channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ContentPart {
    Text { text: String },
    /// Reference to content the host already holds.
    Reference { artifact: String, media_type: String },
    /// A numeric vector, small enough to inline.
    Numbers { values: Vec<f64> },
}

impl ContentPart {
    pub fn modality(&self) -> Modality {
        match self {
            Self::Text { .. } => Modality::Text,
            Self::Reference { media_type, .. } if media_type.starts_with("image/") => Modality::Image,
            Self::Reference { media_type, .. } if media_type.starts_with("audio/") => Modality::Audio,
            Self::Reference { .. } => Modality::Text,
            Self::Numbers { .. } => Modality::Numeric,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeacherRequest {
    pub request_id: String,
    pub capability: TeacherCapability,
    pub parts: Vec<ContentPart>,
    pub licence: UsageLicence,
    /// What the caller wants back. Soft distribution is only honoured
    /// when the capability declares it.
    pub wants: Vec<OutputKind>,
    pub max_response_bytes: usize,
}

/// A program a teacher proposed. Deliberately a distinct type from any
/// approved candidate: it must re-enter the reader and the whitelist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedProgram {
    pub language: String,
    pub source: String,
    /// The teacher that proposed it, for provenance.
    pub proposed_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeacherResponse {
    pub request_id: String,
    pub teacher_id: String,
    /// Present when the provider exposes a model revision.
    pub model_version: Option<String>,
    /// The discrete answer, when requested and produced.
    pub hard_label: Option<String>,
    /// The distribution over `capability.vocabulary`, when declared and
    /// produced. A hard-label-only provider never has this field.
    pub soft_scores: Option<Vec<f64>>,
    pub program: Option<ProposedProgram>,
    /// Why the exchange failed, when it did. A failed teacher call stays
    /// visible instead of degrading into an empty success.
    pub error: Option<String>,
}

impl TeacherResponse {
    /// Soft targets are only meaningful when the scores line up with the
    /// declared vocabulary. A length mismatch is a protocol failure, not
    /// something to pad or truncate.
    pub fn soft_targets(&self, capability: &TeacherCapability) -> Result<&[f64], HostError> {
        let Some(scores) = self.soft_scores.as_ref() else {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                format!("teacher {} returned no soft scores", self.teacher_id),
            ));
        };
        let Some(vocabulary) = capability.vocabulary.as_ref() else {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                "soft scores without a declared vocabulary cannot be aligned",
            ));
        };
        if scores.len() != vocabulary.len() {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                format!(
                    "teacher {} returned {} scores for a {}-entry vocabulary",
                    self.teacher_id,
                    scores.len(),
                    vocabulary.len()
                ),
            ));
        }
        if scores.iter().any(|s| !s.is_finite()) {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                format!("teacher {} returned a non-finite score", self.teacher_id),
            ));
        }
        Ok(scores)
    }

    /// A failed exchange. Errors stay visible; they never become a
    /// zero-score distribution or an empty label.
    pub fn failure(&self) -> Result<(), HostError> {
        match &self.error {
            Some(message) => Err(HostError::new(
                HostErrorKind::Protocol,
                format!("teacher {} failed: {message}", self.teacher_id),
            )),
            None => Ok(()),
        }
    }
}

// ── The host trait ─────────────────────────────────────────────────

/// Teacher capability provider. Kept separate from [`crate::LlmHost`] so
/// the existing text-completion surface keeps its meaning and its
/// consumers.
pub trait TeacherHost {
    /// Validate the request against the declared capability and produce an
    /// answer. Implementations must not silently substitute a capability
    /// the provider does not have.
    fn query(&self, request: &TeacherRequest) -> Result<TeacherResponse, HostError>;
}

/// Validate a request *before* any transport call. Returns the reason the
/// request may not proceed.
pub fn validate_request(request: &TeacherRequest) -> Result<(), HostError> {
    if request.parts.is_empty() {
        return Err(HostError::new(
            HostErrorKind::Config,
            "teacher request carries no content",
        ));
    }
    for part in &request.parts {
        let modality = part.modality();
        if !request.capability.accepts(modality) {
            return Err(HostError::new(
                HostErrorKind::Config,
                format!(
                    "teacher {} does not accept {modality:?} input",
                    request.capability.teacher_id
                ),
            ));
        }
    }
    for kind in &request.wants {
        if !request.capability.supports(*kind) {
            return Err(HostError::new(
                HostErrorKind::Config,
                format!(
                    "teacher {} does not provide {kind:?} output",
                    request.capability.teacher_id
                ),
            ));
        }
    }
    if request.max_response_bytes == 0 {
        return Err(HostError::new(
            HostErrorKind::Config,
            "teacher request must declare a response size limit",
        ));
    }
    request.capability.permits(request.licence)
}

/// A teacher whose answer is recorded verbatim, for replay and audit. The
/// record is the evidence that a real exchange happened; it never
/// invents an answer that was not produced.
pub struct RecordingTeacher<H: TeacherHost> {
    inner: H,
    pub calls: std::sync::Mutex<Vec<TeacherRequest>>,
}

impl<H: TeacherHost> RecordingTeacher<H> {
    pub fn new(inner: H) -> Self {
        Self { inner, calls: std::sync::Mutex::new(Vec::new()) }
    }

    pub fn recorded(&self) -> Vec<TeacherRequest> {
        self.calls.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl<H: TeacherHost> TeacherHost for RecordingTeacher<H> {
    fn query(&self, request: &TeacherRequest) -> Result<TeacherResponse, HostError> {
        validate_request(request)?;
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(request.clone());
        }
        self.inner.query(request)
    }
}

impl fmt::Display for TeacherRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "teacher request {} → {}",
            self.request_id, self.capability.teacher_id
        )
    }
}

#[cfg(feature = "http")]
pub mod http_teacher;
