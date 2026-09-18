//! OpenAI-compatible HTTP implementation of the host protocols.
//!
//! Opt-in via the `http` feature; the default build of zio-ai has no
//! network dependency at all. The client is small, blocking, and honest
//! about it (ADR-012/014): a completion call blocks the eval loop for at
//! most the configured timeout, reads at most `max_response_bytes`, and
//! maps every failure to a typed [`HostError`].

use std::io::Read;
use std::time::Duration;

use crate::{EmbedHost, HostError, HostErrorKind, LlmHost, LlmOptions};

/// OpenAI-compatible chat/embedding host.
pub struct HttpAiHost {
    agent: ureq::Agent,
    base_url: String,
    api_key: Option<String>,
    model: String,
    embed_model: String,
    max_response_bytes: usize,
}

pub struct HttpAiHostBuilder {
    base_url: String,
    api_key: Option<String>,
    model: String,
    embed_model: String,
    timeout: Duration,
    max_response_bytes: usize,
}

impl Default for HttpAiHostBuilder {
    fn default() -> Self {
        HttpAiHostBuilder {
            base_url: "https://api.openai.com/v1".into(),
            api_key: None,
            model: "gpt-4o-mini".into(),
            embed_model: "text-embedding-3-small".into(),
            timeout: Duration::from_secs(30),
            max_response_bytes: 1024 * 1024,
        }
    }
}

impl HttpAiHostBuilder {
    /// Root of an OpenAI-compatible API, e.g. `https://api.openai.com/v1`.
    pub fn base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn embed_model(mut self, embed_model: impl Into<String>) -> Self {
        self.embed_model = embed_model.into();
        self
    }

    /// Connect/read/write timeout for every call.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Hard cap on response body size (ProcessHost discipline).
    pub fn max_response_bytes(mut self, max: usize) -> Self {
        self.max_response_bytes = max;
        self
    }

    pub fn build(self) -> HttpAiHost {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(self.timeout)
            .timeout_read(self.timeout)
            .timeout_write(self.timeout)
            .build();
        HttpAiHost {
            agent,
            base_url: self.base_url,
            api_key: self.api_key,
            model: self.model,
            embed_model: self.embed_model,
            max_response_bytes: self.max_response_bytes,
        }
    }
}

impl HttpAiHost {
    pub fn builder() -> HttpAiHostBuilder {
        HttpAiHostBuilder::default()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }

    fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, HostError> {
        let mut request = self.agent.post(&self.url(path));
        if let Some(key) = &self.api_key {
            request = request.set("Authorization", &format!("Bearer {key}"));
        }
        let response = request
            .send_json(body)
            .map_err(|e| map_ureq_error(path, e))?;
        let status = response.status();
        if status >= 400 {
            let body = limited_read(response.into_reader(), self.max_response_bytes, path)?;
            return Err(HostError::new(
                HostErrorKind::Http,
                format!("{path} answered {}: {}", status, String::from_utf8_lossy(&body)),
            ));
        }
        let bytes = limited_read(response.into_reader(), self.max_response_bytes, path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| HostError::new(HostErrorKind::Protocol, format!("{path}: invalid JSON: {e}")))
    }
}

fn limited_read(
    reader: impl std::io::Read,
    cap: usize,
    path: &str,
) -> Result<Vec<u8>, HostError> {
    let mut limited = reader.take(cap as u64 + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| HostError::new(HostErrorKind::Transport, format!("{path}: read failed: {e}")))?;
    if bytes.len() > cap {
        return Err(HostError::new(
            HostErrorKind::Protocol,
            format!("{path}: response exceeds the {}-byte cap", cap),
        ));
    }
    Ok(bytes)
}

fn map_ureq_error(path: &str, error: ureq::Error) -> HostError {
    match error {
        ureq::Error::Status(code, response) => {
            // Bound the error body too — a hostile endpoint gets no
            // unbounded read out of us.
            let mut limited = response.into_reader().take(4096);
            let mut body = Vec::new();
            let _ = limited.read_to_end(&mut body);
            HostError::new(
                HostErrorKind::Http,
                format!("{path} answered {code}: {}", String::from_utf8_lossy(&body)),
            )
        }
        ureq::Error::Transport(transport) => {
            // ureq 2.x normalizes socket timeouts to ErrorKind::Io with a
            // "timed out" message (WouldBlock is normalized upstream).
            let kind = if transport.kind() == ureq::ErrorKind::Io
                && transport.to_string().to_ascii_lowercase().contains("timed out")
            {
                HostErrorKind::Timeout
            } else {
                HostErrorKind::Transport
            };
            HostError::new(kind, format!("{path}: {transport}"))
        }
    }
}

impl LlmHost for HttpAiHost {
    fn complete(&self, prompt: &str, opts: &LlmOptions) -> Result<String, HostError> {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
        });
        if let Some(temperature) = opts.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        if let Some(max_tokens) = opts.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if !opts.stop.is_empty() {
            body["stop"] = serde_json::json!(opts.stop);
        }
        let response = self.post_json("chat/completions", body)?;
        response["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| {
                HostError::new(
                    HostErrorKind::Protocol,
                    "chat/completions: no choices[0].message.content in response",
                )
            })
    }
}

impl EmbedHost for HttpAiHost {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, HostError> {
        let body = serde_json::json!({ "model": self.embed_model, "input": texts });
        let response = self.post_json("embeddings", body)?;
        let data = response["data"]
            .as_array()
            .ok_or_else(|| {
                HostError::new(HostErrorKind::Protocol, "embeddings: no data array in response")
            })?;
        if data.len() != texts.len() {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                format!("embeddings: {} rows for {} texts", data.len(), texts.len()),
            ));
        }
        data.iter()
            .enumerate()
            .map(|(i, row)| {
                row["embedding"]
                    .as_array()
                    .ok_or_else(|| {
                        HostError::new(
                            HostErrorKind::Protocol,
                            format!("embeddings: row {i} has no embedding array"),
                        )
                    })?
                    .iter()
                    .map(|v| {
                        v.as_f64().ok_or_else(|| {
                            HostError::new(
                                HostErrorKind::Protocol,
                                format!("embeddings: row {i} has a non-number component"),
                            )
                        })
                    })
                    .collect()
            })
            .collect()
    }
}
