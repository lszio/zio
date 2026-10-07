//! HTTP teacher adapter (opt-in, `http` feature).
//!
//! Reuses the transport discipline already proven in [`crate::http`]:
//! blocking calls, a connect/read/write timeout, and a hard cap on the
//! response body. A teacher that streams more than the declared budget is
//! an error, not a truncated success.

use serde::{Deserialize, Serialize};

use crate::teacher::{ContentPart, TeacherHost, TeacherRequest, TeacherResponse, validate_request};
use crate::{HostError, HostErrorKind};

/// Wire shape of a teacher exchange. Only the fields the capability
/// declares are sent: a provider without soft-output support never sees a
/// `want_soft` flag, so it cannot be pressured into inventing one.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireRequest {
    request_id: String,
    teacher_id: String,
    model: Option<String>,
    parts: Vec<ContentPart>,
    want_hard: bool,
    want_soft: bool,
    want_program: bool,
    licence: String,
}

#[derive(Debug, Clone, Deserialize)]
struct WireResponse {
    request_id: String,
    teacher_id: String,
    model_version: Option<String>,
    hard_label: Option<String>,
    soft_scores: Option<Vec<f64>>,
    program: Option<WireProgram>,
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WireProgram {
    language: String,
    source: String,
}

/// OpenAI-compatible-style teacher endpoint.
pub struct HttpTeacherHost {
    agent: ureq::Agent,
    base_url: String,
    api_key: Option<String>,
    path: String,
    max_response_bytes: usize,
}

impl HttpTeacherHost {
    pub fn builder() -> HttpTeacherHostBuilder {
        HttpTeacherHostBuilder::default()
    }

    fn post(&self, body: &WireRequest) -> Result<WireResponse, HostError> {
        let mut request = self.agent.post(&format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            self.path
        ));
        if let Some(key) = &self.api_key {
            request = request.set("Authorization", &format!("Bearer {key}"));
        }
        let response = request
            .send_json(body)
            .map_err(|e| crate::http::map_ureq_error(&self.path, e))?;
        let status = response.status();
        if status >= 400 {
            let mut limited = response.into_reader().take(4096);
            let mut body_bytes = Vec::new();
            use std::io::Read as _;
            let _ = limited.read_to_end(&mut body_bytes);
            return Err(HostError::new(
                HostErrorKind::Http,
                format!(
                    "{} answered {}: {}",
                    self.path,
                    status,
                    String::from_utf8_lossy(&body_bytes)
                ),
            ));
        }
        let bytes =
            crate::http::limited_read(response.into_reader(), self.max_response_bytes, &self.path)?;
        serde_json::from_slice(&bytes).map_err(|e| {
            HostError::new(
                HostErrorKind::Protocol,
                format!("{}: invalid JSON: {e}", self.path),
            )
        })
    }
}

pub struct HttpTeacherHostBuilder {
    base_url: String,
    api_key: Option<String>,
    path: String,
    timeout: std::time::Duration,
    max_response_bytes: usize,
}

impl Default for HttpTeacherHostBuilder {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8088/v1".into(),
            api_key: None,
            path: "teacher".into(),
            timeout: std::time::Duration::from_secs(30),
            max_response_bytes: 1024 * 1024,
        }
    }
}

impl HttpTeacherHostBuilder {
    /// Base URL, e.g. `http://127.0.0.1:8088/v1`.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    pub fn timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn max_response_bytes(mut self, max: usize) -> Self {
        self.max_response_bytes = max;
        self
    }

    pub fn build(self) -> HttpTeacherHost {
        HttpTeacherHost {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(self.timeout)
                .timeout_read(self.timeout)
                .timeout_write(self.timeout)
                .build(),
            base_url: self.base_url,
            api_key: self.api_key,
            path: self.path,
            max_response_bytes: self.max_response_bytes,
        }
    }
}

impl TeacherHost for HttpTeacherHost {
    fn query(&self, request: &TeacherRequest) -> Result<TeacherResponse, HostError> {
        validate_request(request)?;
        let wire = WireRequest {
            request_id: request.request_id.clone(),
            teacher_id: request.capability.teacher_id.clone(),
            model: request.capability.model_version.clone(),
            parts: request.parts.clone(),
            want_hard: request
                .wants
                .contains(&crate::teacher::OutputKind::HardLabel),
            want_soft: request.capability.supports_soft_targets()
                && request
                    .wants
                    .contains(&crate::teacher::OutputKind::SoftDistribution),
            want_program: request.wants.contains(&crate::teacher::OutputKind::Program),
            licence: serde_json::to_value(request.licence)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "display-only".to_string()),
        };
        let response = self.post(&wire)?;
        if response.request_id != request.request_id {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                format!(
                    "teacher answered request {} for request {}",
                    response.request_id, request.request_id
                ),
            ));
        }
        let teacher_id = response.teacher_id.clone();
        Ok(TeacherResponse {
            request_id: response.request_id,
            program: response.program.map(|p| crate::teacher::ProposedProgram {
                language: p.language,
                source: p.source,
                proposed_by: teacher_id.clone(),
            }),
            teacher_id,
            model_version: response.model_version,
            hard_label: response.hard_label,
            soft_scores: response.soft_scores,
            error: response.error,
        })
    }
}
