//! Deterministic mocks (ADR-016): scripted backends for tests, replay
//! mocks backed by recording files, recording pass-through hosts that
//! capture a real backend, and a synthetic embedder for offline
//! embedding-store development.
//!
//! Recording files are zio data literals — the same S-expressions a zio
//! REPL can `read-string` — one entry per line:
//!
//! ```text
//! ;; llm recording:       ["prompt" "response"]              ; per call
//! ;; embedding recording: [["t1" "t2"] (v1 ...) (v2 ...)]    ; per batch
//! ```
//!
//! Replay is keyed by FNV-1a hash of the prompt (NUL-joined texts for
//! embeddings) with full-string verification; a miss is a hard error —
//! the replay contract forbids touching the network.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::{EmbedHost, HostError, HostErrorKind, LlmHost, LlmOptions, fnv1a64};

// ── Scripted backends (for tests and recording) ─────────────────

/// Answers completion calls from a queue of responses in order, and
/// remembers the last prompt/options it saw.
pub struct ScriptedLlmHost {
    responses: RefCell<Vec<String>>,
    pub last_prompt: RefCell<String>,
    pub last_opts: RefCell<LlmOptions>,
}

impl ScriptedLlmHost {
    pub fn new<I: IntoIterator<Item = String>>(responses: I) -> Self {
        ScriptedLlmHost {
            responses: RefCell::new(responses.into_iter().collect()),
            last_prompt: RefCell::new(String::new()),
            last_opts: RefCell::new(LlmOptions::default()),
        }
    }
}

impl LlmHost for ScriptedLlmHost {
    fn complete(&self, prompt: &str, opts: &LlmOptions) -> Result<String, HostError> {
        *self.last_prompt.borrow_mut() = prompt.to_string();
        *self.last_opts.borrow_mut() = opts.clone();
        let mut queue = self.responses.borrow_mut();
        if queue.is_empty() {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                "script exhausted: no more scripted responses",
            ));
        }
        Ok(queue.remove(0))
    }
}

/// Answers embedding calls from a queue of batches in order.
pub struct ScriptedEmbedHost {
    batches: RefCell<Vec<Vec<Vec<f64>>>>,
    pub last_texts: RefCell<Vec<String>>,
}

impl ScriptedEmbedHost {
    pub fn new<I: IntoIterator<Item = Vec<Vec<f64>>>>(batches: I) -> Self {
        ScriptedEmbedHost {
            batches: RefCell::new(batches.into_iter().collect()),
            last_texts: RefCell::new(Vec::new()),
        }
    }
}

impl EmbedHost for ScriptedEmbedHost {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, HostError> {
        *self.last_texts.borrow_mut() = texts.to_vec();
        let mut queue = self.batches.borrow_mut();
        if queue.is_empty() {
            return Err(HostError::new(
                HostErrorKind::Protocol,
                "script exhausted: no more scripted batches",
            ));
        }
        Ok(queue.remove(0))
    }
}

/// A deterministic embedder that maps each text to a unit vector derived
/// from its hash. Identical texts embed identically; distinct texts embed
/// near-orthogonally — enough to exercise cosine stores offline, and
/// honest about being synthetic.
pub struct SyntheticEmbedHost {
    pub dim: usize,
}

impl SyntheticEmbedHost {
    pub fn new(dim: usize) -> Self {
        SyntheticEmbedHost { dim: dim.max(1) }
    }
}

impl EmbedHost for SyntheticEmbedHost {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, HostError> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut row = Vec::with_capacity(self.dim);
                for i in 0..self.dim {
                    let h = fnv1a64(format!("{text}\u{0}{i}").as_bytes());
                    row.push((h % 20_001) as f64 / 20_000.0 - 0.5);
                }
                let norm: f64 = row.iter().map(|x| x * x).sum::<f64>().sqrt();
                row.into_iter().map(|x| x / norm).collect()
            })
            .collect())
    }
}

// ── Replay mocks ────────────────────────────────────────────────

/// Replays recorded llm calls. Options are not part of the v1 replay key:
/// recordings are keyed by prompt content alone.
pub struct MockLlmHost {
    entries: HashMap<u64, (String, String)>,
    source: Option<String>,
}

impl MockLlmHost {
    pub fn from_recording_text(text: &str) -> Result<Self, HostError> {
        let doc = parse_recording(text)?;
        let mut entries = HashMap::new();
        for entry in doc {
            match entry {
                Datum::Vect(items) if items.len() == 2 => {
                    let prompt = as_str(&items[0], "llm recording prompt")?;
                    let response = as_str(&items[1], "llm recording response")?;
                    entries.insert(fnv1a64(prompt.as_bytes()), (prompt, response));
                }
                _ => {
                    return Err(HostError::new(
                        HostErrorKind::Protocol,
                        "llm recording entry must be [\"prompt\" \"response\"]",
                    ));
                }
            }
        }
        Ok(MockLlmHost { entries, source: None })
    }

    pub fn from_recording_file(path: impl AsRef<Path>) -> Result<Self, HostError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            HostError::new(HostErrorKind::Io, format!("cannot read {}: {e}", path.display()))
        })?;
        let mut mock = Self::from_recording_text(&text)?;
        mock.source = Some(path.display().to_string());
        Ok(mock)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn source_name(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

impl LlmHost for MockLlmHost {
    fn complete(&self, prompt: &str, _opts: &LlmOptions) -> Result<String, HostError> {
        let key = fnv1a64(prompt.as_bytes());
        match self.entries.get(&key) {
            Some((recorded_prompt, response)) if recorded_prompt == prompt => Ok(response.clone()),
            _ => Err(HostError::new(
                HostErrorKind::ReplayMiss,
                format!(
                    "no llm recording for prompt hash {key:016x} (fail-fast by contract; \
                     record the call or point at the right recording file)"
                ),
            )),
        }
    }
}

/// Replays recorded embedding batches.
pub struct MockEmbedHost {
    entries: HashMap<u64, (Vec<String>, Vec<Vec<f64>>)>,
    source: Option<String>,
}

impl MockEmbedHost {
    pub fn from_recording_text(text: &str) -> Result<Self, HostError> {
        let doc = parse_recording(text)?;
        let mut entries = HashMap::new();
        for entry in doc {
            match entry {
                Datum::Vect(items) if items.len() >= 2 => {
                    let texts = as_str_vec(&items[0], "embedding recording texts")?;
                    let vectors: Vec<Vec<f64>> = items[1..]
                        .iter()
                        .map(|d| as_f64_vec(d, "embedding recording vector"))
                        .collect::<Result<Vec<_>, _>>()?;
                    if vectors.len() != texts.len() {
                        return Err(HostError::new(
                            HostErrorKind::Protocol,
                            format!(
                                "embedding recording entry has {} vectors for {} texts",
                                vectors.len(),
                                texts.len()
                            ),
                        ));
                    }
                    let key = embed_key(&texts);
                    entries.insert(key, (texts, vectors));
                }
                _ => {
                    return Err(HostError::new(
                        HostErrorKind::Protocol,
                        "embedding recording entry must be [[\"t1\" ...] (v1 ...) ...]",
                    ));
                }
            }
        }
        Ok(MockEmbedHost { entries, source: None })
    }

    pub fn from_recording_file(path: impl AsRef<Path>) -> Result<Self, HostError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            HostError::new(HostErrorKind::Io, format!("cannot read {}: {e}", path.display()))
        })?;
        let mut mock = Self::from_recording_text(&text)?;
        mock.source = Some(path.display().to_string());
        Ok(mock)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn source_name(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

fn embed_key(texts: &[String]) -> u64 {
    fnv1a64(texts.join("\u{0}").as_bytes())
}

impl EmbedHost for MockEmbedHost {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, HostError> {
        let key = embed_key(texts);
        match self.entries.get(&key) {
            Some((recorded_texts, vectors)) if recorded_texts == texts => Ok(vectors.clone()),
            _ => Err(HostError::new(
                HostErrorKind::ReplayMiss,
                format!(
                    "no embedding recording for text-batch hash {key:016x} \
                     (fail-fast by contract)"
                ),
            )),
        }
    }
}

// ── Recording pass-through hosts ────────────────────────────────

/// Wraps a real backend, records every (prompt, response) pair, and saves
/// the capture as a zio data literal file for [`MockLlmHost`].
pub struct RecordingLlmHost {
    backend: Arc<dyn LlmHost>,
    calls: RefCell<Vec<(String, String)>>,
}

impl RecordingLlmHost {
    pub fn new(backend: Arc<dyn LlmHost>) -> Self {
        RecordingLlmHost { backend, calls: RefCell::new(Vec::new()) }
    }

    pub fn calls(&self) -> usize {
        self.calls.borrow().len()
    }

    /// Write the captured calls to `path`; returns the entry count.
    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<usize> {
        let calls = self.calls.borrow();
        std::fs::write(path, llm_recording_text(&calls))?;
        Ok(calls.len())
    }
}

impl LlmHost for RecordingLlmHost {
    fn complete(&self, prompt: &str, opts: &LlmOptions) -> Result<String, HostError> {
        let response = self.backend.complete(prompt, opts)?;
        self.calls.borrow_mut().push((prompt.to_string(), response.clone()));
        Ok(response)
    }
}

/// Wraps a real embedder and records every batch for [`MockEmbedHost`].
pub struct RecordingEmbedHost {
    backend: Arc<dyn EmbedHost>,
    batches: RefCell<Vec<(Vec<String>, Vec<Vec<f64>>)>>,
}

impl RecordingEmbedHost {
    pub fn new(backend: Arc<dyn EmbedHost>) -> Self {
        RecordingEmbedHost { backend, batches: RefCell::new(Vec::new()) }
    }

    pub fn batches(&self) -> usize {
        self.batches.borrow().len()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> std::io::Result<usize> {
        let batches = self.batches.borrow();
        std::fs::write(path, embed_recording_text(&batches))?;
        Ok(batches.len())
    }
}

impl EmbedHost for RecordingEmbedHost {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f64>>, HostError> {
        let vectors = self.backend.embed(texts)?;
        self.batches.borrow_mut().push((texts.to_vec(), vectors.clone()));
        Ok(vectors)
    }
}

/// Surface for integration tests: recording writers, so contract tests
/// exercise the exact byte format replay consumes.
pub mod test_support {
    pub use super::{embed_recording_text, llm_recording_text, parse_answer_lines};
}

/// Rust mirror of `lib/zio/proposer.zio` `llm--parse-answer` (对拍): trim
/// each line, drop empty / comment / markdown-fence lines, keep order.
/// The proposer contract test asserts the pure-Zio parser and this
/// reference agree on the same canned answers.
pub fn parse_answer_lines(answer: &str) -> Vec<String> {
    answer
        .split('\n')
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with(';')
                && !line.starts_with("```")
                && !line.ends_with("```")
        })
        .map(str::to_string)
        .collect()
}

// ── zio data literal format ─────────────────────────────────────

pub fn llm_recording_text(calls: &[(String, String)]) -> String {
    let mut out = String::from(";; zio-ai llm recording v1 — [\"prompt\" \"response\"] per call\n");
    for (prompt, response) in calls {
        out.push_str(&format!(
            "[\"{}\" \"{}\"]\n",
            zio_escape(prompt),
            zio_escape(response)
        ));
    }
    out
}

pub fn embed_recording_text(batches: &[(Vec<String>, Vec<Vec<f64>>)]) -> String {
    let mut out =
        String::from(";; zio-ai embedding recording v1 — [[\"t1\" ...] (v1 ...) ...] per batch\n");
    for (texts, vectors) in batches {
        let texts: Vec<String> = texts.iter().map(|t| format!("\"{}\"", zio_escape(t))).collect();
        let rows: Vec<String> = vectors
            .iter()
            .map(|row| format!("({})", row.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>().join(" ")))
            .collect();
        out.push_str(&format!("[[{}] {}]\n", texts.join(" "), rows.join(" ")));
    }
    out
}

/// Escape a string for zio string-literal syntax (the subset the reader
/// and the recorder agree on).
pub(crate) fn zio_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

// ── Recorder parser (zio data literal subset) ───────────────────

#[derive(Debug, Clone, PartialEq)]
enum Datum {
    Str(String),
    Num(f64),
    List(Vec<Datum>),
    Vect(Vec<Datum>),
}

fn parse_recording(text: &str) -> Result<Vec<Datum>, HostError> {
    let mut lexer = Lexer::new(text);
    let mut doc = Vec::new();
    while lexer.peek_token()?.is_some() {
        doc.push(parse_datum(&mut lexer)?);
    }
    Ok(doc)
}

fn parse_datum(lexer: &mut Lexer<'_>) -> Result<Datum, HostError> {
    match lexer.next_token()? {
        Some(Token::Str(s)) => Ok(Datum::Str(s)),
        Some(Token::Num(f)) => Ok(Datum::Num(f)),
        Some(Token::LParen) => parse_seq(lexer, Token::RParen).map(|v| Datum::List(v)),
        Some(Token::LBracket) => parse_seq(lexer, Token::RBracket).map(|v| Datum::Vect(v)),
        Some(other) => Err(unexpected(&other)),
        None => Err(HostError::new(HostErrorKind::Protocol, "recording ended mid-entry")),
    }
}

fn parse_seq(lexer: &mut Lexer<'_>, close: Token) -> Result<Vec<Datum>, HostError> {
    let mut items = Vec::new();
    loop {
        match lexer.peek_token()? {
            None => return Err(HostError::new(HostErrorKind::Protocol, "unclosed sequence in recording")),
            Some(tok) if tok == close => {
                lexer.next_token()?;
                return Ok(items);
            }
            Some(_) => items.push(parse_datum(lexer)?),
        }
    }
}

fn unexpected(token: &Token) -> HostError {
    HostError::new(
        HostErrorKind::Protocol,
        format!("unexpected {} in recording", token.describe()),
    )
}

fn as_str(datum: &Datum, what: &str) -> Result<String, HostError> {
    match datum {
        Datum::Str(s) => Ok(s.clone()),
        _ => Err(HostError::new(HostErrorKind::Protocol, format!("{what} must be a string"))),
    }
}

fn as_str_vec(datum: &Datum, what: &str) -> Result<Vec<String>, HostError> {
    match datum {
        Datum::Vect(items) | Datum::List(items) => {
            items.iter().map(|d| as_str(d, what)).collect()
        }
        _ => Err(HostError::new(HostErrorKind::Protocol, format!("{what} must be a vector"))),
    }
}

fn as_f64_vec(datum: &Datum, what: &str) -> Result<Vec<f64>, HostError> {
    match datum {
        Datum::Vect(items) | Datum::List(items) => items
            .iter()
            .map(|d| match d {
                Datum::Num(f) => Ok(*f),
                _ => Err(HostError::new(
                    HostErrorKind::Protocol,
                    format!("{what} must contain numbers"),
                )),
            })
            .collect(),
        _ => Err(HostError::new(HostErrorKind::Protocol, format!("{what} must be a sequence"))),
    }
}

#[derive(Debug, Clone)]
enum Token {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Str(String),
    Num(f64),
}

impl Token {
    fn describe(&self) -> &'static str {
        match self {
            Token::LParen => "(",
            Token::RParen => ")",
            Token::LBracket => "[",
            Token::RBracket => "]",
            Token::Str(_) => "string",
            Token::Num(_) => "number",
        }
    }
}

impl PartialEq for Token {
    fn eq(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer { src: src.as_bytes(), pos: 0 }
    }

    fn skip_trivia(&mut self) {
        loop {
            while self.pos < self.src.len() && self.src[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.pos + 1 < self.src.len()
                && self.src[self.pos] == b';'
                && self.src[self.pos + 1] == b';'
            {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    fn peek_token(&mut self) -> Result<Option<Token>, HostError> {
        let save = self.pos;
        let tok = self.next_token()?;
        self.pos = save;
        Ok(tok)
    }

    fn next_token(&mut self) -> Result<Option<Token>, HostError> {
        self.skip_trivia();
        if self.pos >= self.src.len() {
            return Ok(None);
        }
        let b = self.src[self.pos];
        self.pos += 1;
        let token = match b {
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b'[' => Token::LBracket,
            b']' => Token::RBracket,
            b'"' => return self.lex_string().map(Some),
            b if b == b'-' || b == b'+' || b.is_ascii_digit() || b == b'.' => {
                let start = self.pos - 1;
                while self.pos < self.src.len() {
                    let c = self.src[self.pos];
                    if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E'
                        || ((c == b'-' || c == b'+') && (self.src[self.pos - 1] == b'e' || self.src[self.pos - 1] == b'E'))
                    {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let text = std::str::from_utf8(&self.src[start..self.pos])
                    .map_err(|_| HostError::new(HostErrorKind::Protocol, "invalid UTF-8 in recording"))?;
                let value: f64 = text
                    .parse()
                    .map_err(|_| HostError::new(HostErrorKind::Protocol, format!("bad number {text:?}")))?;
                Token::Num(value)
            }
            other => {
                return Err(HostError::new(
                    HostErrorKind::Protocol,
                    format!("unexpected byte {other:#x} in recording"),
                ));
            }
        };
        Ok(Some(token))
    }

    fn lex_string(&mut self) -> Result<Token, HostError> {
        // The opening quote is already consumed; scan to the closing quote
        // in bytes, then decode the whole literal as UTF-8 so multi-byte
        // characters survive round-trips.
        let start = self.pos;
        loop {
            let b = *self.src.get(self.pos).ok_or_else(|| {
                HostError::new(HostErrorKind::Protocol, "unterminated string in recording")
            })?;
            match b {
                b'"' => {
                    let text = std::str::from_utf8(&self.src[start..self.pos])
                        .map_err(|_| HostError::new(HostErrorKind::Protocol, "invalid UTF-8 in recording string"))?;
                    self.pos += 1;
                    return Ok(Token::Str(unescape(text)?));
                }
                b'\\' => {
                    let esc = *self.src.get(self.pos + 1).ok_or_else(|| {
                        HostError::new(HostErrorKind::Protocol, "dangling escape in recording")
                    })?;
                    if !matches!(esc, b'"' | b'\\' | b'n' | b'r' | b't') {
                        return Err(HostError::new(
                            HostErrorKind::Protocol,
                            format!("unsupported escape \\{} in recording", esc as char),
                        ));
                    }
                    self.pos += 2;
                }
                _ => self.pos += 1,
            }
        }
    }
}

fn unescape(text: &str) -> Result<String, HostError> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            other => {
                return Err(HostError::new(
                    HostErrorKind::Protocol,
                    format!("unsupported escape \\{:?} in recording", other),
                ));
            }
        }
    }
    Ok(out)
}
