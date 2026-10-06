//! G01: the persistent execution-event log.
//!
//! Core's [`zio_core::observer`] says *what happened* while the program
//! ran. This module makes that durable and addressable:
//!
//! * **append-only.** A sequence number may be written once. Two claims
//!   about what happened at one point in a run is a contradiction, not
//!   an update, so a rewrite is a conflict rather than a silent
//!   overwrite.
//! * **ordered and cursor-readable.** A reader that has seen up to `N`
//!   gets exactly what came after — the whole reason a sequence number
//!   is assigned at the observation point rather than at read time.
//! * **bounded.** A detail field over the cap is refused, not truncated:
//!   a trace that stops recording without saying so is a trace that
//!   looks complete and is not.
//! * **evidence, not authority.** An event records that a call named
//!   `approve` happened. It is not an approval. Publication, evaluation
//!   and release stay behind the human boundary in the store's other
//!   tables; nothing here can reach them.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::contracts::{Actor, ActorRole, Error, ErrorKind, Result, SCHEMA_VERSION};
use crate::store::Store;

/// The largest `detail` a single event may carry. A trace that grows with
/// the data is a trace that leaks it.
pub const MAX_TRACE_BYTES: usize = 8 * 1024;

/// Where an event happened. Resolved against the same registry the
/// evaluator used, so a reader can show the source without the log
/// carrying the source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSource {
    pub source_id: usize,
    pub name: String,
    pub line: usize,
    pub col: usize,
}

/// One durable fact about an execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub schema: crate::contracts::SchemaVersion,
    /// Dense and monotonic within a run. Assigned at the observation
    /// point, so the order in the log is the order things happened.
    pub sequence: u64,
    pub run_id: String,
    pub attempt_id: String,
    pub kind: String,
    pub detail: String,
    /// Absent for something that has no place, such as a synthetic
    /// event. Never invented.
    #[serde(default)]
    pub source: Option<EventSourceStub>,
    /// When it was observed, not when it was written.
    pub at_ms: i64,
}

/// The serializable half of [`EventSource`].
///
/// Core's `Span` carries byte offsets and a line/column; what a log
/// needs is the resolved location, because the file is not stored beside
/// it. The two are converted at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSourceStub {
    pub source_id: usize,
    pub name: String,
    pub line: usize,
    pub col: usize,
}

impl From<EventSource> for EventSourceStub {
    fn from(s: EventSource) -> Self {
        EventSourceStub {
            source_id: s.source_id,
            name: s.name,
            line: s.line,
            col: s.col,
        }
    }
}

/// A handle onto one run's log. Naming the run is the only thing it can
/// do — there is no method here that approves, publishes, or evaluates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLog {
    pub run_id: String,
}

impl EventLog {
    pub fn for_run(run_id: &str) -> Self {
        EventLog {
            run_id: run_id.to_string(),
        }
    }
}

// ── the store table ───────────────────────────────────────────────

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS execution_events (
    run_id  TEXT NOT NULL,
    sequence INTEGER NOT NULL,
    attempt TEXT NOT NULL,
    kind    TEXT NOT NULL,
    detail  TEXT NOT NULL,
    source  TEXT,
    at_ms   INTEGER NOT NULL,
    schema  INTEGER NOT NULL,
    PRIMARY KEY (run_id, sequence)
);
CREATE INDEX IF NOT EXISTS execution_events_by_run
    ON execution_events (run_id, sequence);
"#;

pub(crate) fn install_schema(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

/// Append a batch of events, all or nothing.
///
/// A batch that contains one oversized or out-of-order event writes
/// nothing: a partial trace is a claim about coverage that the run did
/// not deliver.
pub fn append_events(
    store: &Store,
    actor: &Actor,
    events: &[ExecutionEvent],
) -> Result<()> {
    // Only a runner writes the log. A reader can read it and nothing
    // else, so the log cannot be back-filled by whoever is looking at it.
    actor.require(ActorRole::Operator, "appending execution events")?;
    if events.is_empty() {
        return Ok(());
    }
    let conn = store.connection();
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("begin: {e}")))?;

    for event in events {
        if event.detail.len() > MAX_TRACE_BYTES {
            return Err(Error::new(
                ErrorKind::ProtocolViolation,
                format!(
                    "execution event {}:{} detail is {} bytes, over the {MAX_TRACE_BYTES}-byte cap; \
                     the event is refused rather than truncated, because a trace that stops \
                     recording without saying so looks complete",
                    event.run_id, event.sequence, event.detail.len()
                ),
            ));
        }
        if event.schema != SCHEMA_VERSION {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!(
                    "execution event {}:{} has schema {}, store is {}",
                    event.run_id, event.sequence, event.schema, SCHEMA_VERSION
                ),
            ));
        }
        let source = event.source.as_ref().map(|s| {
            serde_json::to_string(s).unwrap_or_else(|_| "null".to_string())
        });
        // Prepared per insert and reset, rather than cached: a cached
        // statement is reset by the connection when it is returned, and
        // a statement reused across rows inside one transaction can be
        // left mid-step — which loses exactly the first row.
        let inserted = tx
            .prepare_cached(
                "INSERT INTO execution_events
                 (run_id, sequence, attempt, kind, detail, source, at_ms, schema)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .and_then(|mut stmt| {
                stmt.execute(rusqlite::params![
                    event.run_id,
                    event.sequence as i64,
                    event.attempt_id,
                    event.kind,
                    event.detail,
                    source,
                    event.at_ms,
                    event.schema as i64,
                ])
            });
        match inserted {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!(
                        "execution event {}:{} is already written; a sequence number may be \
                         claimed once — a second claim is a contradiction, not an update",
                        event.run_id, event.sequence
                    ),
                ));
            }
            Err(e) => {
                return Err(Error::new(
                    ErrorKind::BackendFailed,
                    format!(
                        "append execution event {}:{} (kind {:?}): {e}",
                        event.run_id, event.sequence, event.kind
                    ),
                ));
            }
        }
    }

    tx.commit()
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("commit: {e}")))?;
    Ok(())
}

/// Read a run's events, strictly after `after_sequence`.
///
/// `after_sequence` is exclusive, so a reader that has processed up to
/// and including `N` asks for `N` and gets the rest.
pub fn read_events(store: &Store, run_id: &str, after_sequence: u64) -> Result<Vec<ExecutionEvent>> {
    read_events_with(store, run_id, Some(after_sequence as i64), true)
}

/// Read a run's whole log, from the first event.
///
/// A cursor cannot express "before the first event" as a `u64`, and a
/// reader that has just opened a run needs exactly that. This is the
/// one place the cursor's arithmetic is allowed to be special-cased.
pub fn read_events_from_start(store: &Store, run_id: &str) -> Result<Vec<ExecutionEvent>> {
    read_events_with(store, run_id, None, false)
}

fn read_events_with(
    store: &Store,
    run_id: &str,
    after: Option<i64>,
    exclusive: bool,
) -> Result<Vec<ExecutionEvent>> {
    let sql = if exclusive {
        "SELECT run_id, sequence, attempt, kind, detail, source, at_ms, schema
         FROM execution_events
         WHERE run_id = ?1 AND sequence > ?2
         ORDER BY sequence"
    } else {
        "SELECT run_id, sequence, attempt, kind, detail, source, at_ms, schema
         FROM execution_events
         WHERE run_id = ?1
         ORDER BY sequence"
    };
    let conn = store.connection();
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("prepare read: {e}")))?;
    let run = run_id.to_string();
    let bound: Vec<Box<dyn rusqlite::ToSql>> = match after {
        Some(value) => vec![Box::new(run), Box::new(value)],
        None => vec![Box::new(run)],
    };
    let bound: Vec<&dyn rusqlite::ToSql> = bound.iter().map(|v| v.as_ref()).collect();
    let rows = stmt
        .query_map(bound.as_slice(), |row| {
            let source_text: Option<String> = row.get(5)?;
            Ok(ExecutionEvent {
                run_id: row.get(0)?,
                sequence: row.get::<_, i64>(1)? as u64,
                attempt_id: row.get(2)?,
                kind: row.get(3)?,
                detail: row.get(4)?,
                source: match source_text {
                    Some(text) if text != "null" => serde_json::from_str(&text).ok(),
                    _ => None,
                },
                at_ms: row.get(6)?,
                schema: row.get::<_, i64>(7)? as u32,
            })
        })
        .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("query: {e}")))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("read event: {e}"))
        })?);
    }
    Ok(out)
}

// ── the observer that feeds the log ───────────────────────────────

/// A [`zio_core::observer::Observer`] that collects events in memory so a
/// trusted host can stamp them with the run identity it owns and commit
/// them.
///
/// The split is deliberate: the evaluator knows *what happened* and
/// nothing about which run it was, and the host knows the run. Neither
/// can forge the other's half.
pub struct StoreObserver {
    events: Mutex<Vec<zio_core::observer::Event>>,
}

impl Default for StoreObserver {
    fn default() -> Self {
        StoreObserver {
            events: Mutex::new(Vec::new()),
        }
    }
}

impl StoreObserver {
    pub fn new() -> Self {
        StoreObserver::default()
    }

    /// Everything observed so far, without consuming the observer: the
    /// context still holds a reference to it.
    pub fn collected(&self) -> Vec<ExecutionEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .map(|e| ExecutionEvent {
                schema: SCHEMA_VERSION,
                sequence: e.sequence,
                run_id: String::new(),
                attempt_id: String::new(),
                kind: e.kind.as_str().to_string(),
                detail: e.detail,
                source: e.span.map(|span| EventSourceStub {
                    source_id: span.source_id.0,
                    name: String::new(),
                    line: span.line,
                    col: span.col,
                }),
                at_ms: 0,
            })
            .collect()
    }

    /// Take everything observed so far, consuming the observer.
    pub fn into_events(self) -> Vec<ExecutionEvent> {
        self.events
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .into_iter()
            .map(|e| ExecutionEvent {
                schema: SCHEMA_VERSION,
                sequence: e.sequence,
                run_id: String::new(),
                attempt_id: String::new(),
                kind: e.kind.as_str().to_string(),
                detail: e.detail,
                source: e
                    .span
                    .map(|span| EventSourceStub {
                        source_id: span.source_id.0,
                        name: String::new(),
                        line: span.line,
                        col: span.col,
                    }),
                at_ms: 0,
            })
            .collect()
    }
}

impl zio_core::observer::Observer for StoreObserver {
    fn on_event(&self, event: &zio_core::observer::Event) {
        // The lock is held only for the push: an observer must not be a
        // place where evaluation can block on I/O.
        let mut guard = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.push(event.clone());
    }
}

/// A ready-to-commit batch: events with the run identity a trusted host
/// owns already stamped on them.
pub struct RunEvents {
    run_id: String,
    attempt_id: String,
    events: Vec<ExecutionEvent>,
}

impl RunEvents {
    pub fn new(run_id: &str, attempt_id: &str, mut events: Vec<ExecutionEvent>) -> Self {
        let now = crate::api_time_ms();
        for (index, event) in events.iter_mut().enumerate() {
            event.run_id = run_id.to_string();
            event.attempt_id = attempt_id.to_string();
            event.sequence = index as u64;
            // A timestamp of 0 means "the host did not stamp it"; the
            // observation point is now, which is the honest floor.
            if event.at_ms == 0 {
                event.at_ms = now;
            }
        }
        RunEvents {
            run_id: run_id.to_string(),
            attempt_id: attempt_id.to_string(),
            events,
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append to the store. The identity is not the caller's to change.
    pub fn commit(&self, store: &Store, actor: &Actor) -> Result<()> {
        append_events(store, actor, &self.events)
    }
}

/// Convenience: the events of a run, as an `Arc`ed log handle.
pub fn log_for(store: &Store, run_id: &str) -> Result<Arc<EventLog>> {
    // Reading is how a log handle is validated: a handle for a run whose
    // events cannot be read is not a handle.
    let _ = read_events(store, run_id, 0)?;
    Ok(Arc::new(EventLog::for_run(run_id)))
}
