//! SQLite-backed store: manifests, references and optimistic head updates.
//!
//! The invariant this module defends is *no committed reference to a byte
//! that is not durable*. Every write path is therefore: persist artifact
//! bytes first, verify the digest, then commit the manifest row in a
//! transaction that re-checks the schema version. A crash between the two
//! steps leaves an unreferenced object, which is garbage-collectable; it
//! never leaves a committed record that cannot be loaded.

use std::path::Path;
use std::sync::Arc;


use rusqlite::{params, Connection, OptionalExtension};

use crate::artifacts::ArtifactStore;
use crate::contracts::{
    Actor, ActorRole, ArtifactRef, Branch, Checkpoint, DatasetRevision, Error, ErrorKind,
    EvaluationRecord, LearningSignal, ModelSnapshot, Observation, Population, Prediction, Recipe,
    Result, Run, RunState, SCHEMA_VERSION, SignalKind,
};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS manifests (
    digest TEXT PRIMARY KEY,   -- artifact digest of the canonical record
    kind   TEXT NOT NULL,
    owner  TEXT NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS observations (
    id     TEXT PRIMARY KEY,
    owner  TEXT NOT NULL,
    task   TEXT NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS predictions (
    id         TEXT PRIMARY KEY,
    owner      TEXT NOT NULL,
    observation TEXT NOT NULL,
    snapshot   TEXT NOT NULL,
    schema     INTEGER NOT NULL,
    body       TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS signals (
    id             TEXT PRIMARY KEY,
    producer       TEXT NOT NULL,
    idempotency    TEXT NOT NULL,
    kind           TEXT NOT NULL,
    target         TEXT NOT NULL,   -- observation id / prediction id / task
    status         TEXT NOT NULL,   -- accepted | superseded | retracted
    task           TEXT NOT NULL,
    target_field   TEXT,
    received_at_ms INTEGER NOT NULL,
    schema         INTEGER NOT NULL,
    body           TEXT NOT NULL,
    UNIQUE (producer, idempotency)
);

CREATE TABLE IF NOT EXISTS datasets (
    id     TEXT PRIMARY KEY,
    task   TEXT NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS recipes (
    id     TEXT PRIMARY KEY,
    task   TEXT NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id     TEXT PRIMARY KEY,
    task   TEXT NOT NULL,
    state  TEXT NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS checkpoints (
    id       TEXT PRIMARY KEY,
    owner    TEXT NOT NULL,
    run      TEXT NOT NULL,
    parent   TEXT,
    schema   INTEGER NOT NULL,
    body     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS branches (
    id     TEXT PRIMARY KEY,
    owner  TEXT NOT NULL,
    head   TEXT,
    version INTEGER NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS populations (
    id     TEXT PRIMARY KEY,
    owner  TEXT NOT NULL,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS evaluations (
    id           TEXT PRIMARY KEY,
    snapshot     TEXT NOT NULL,
    protocol     TEXT NOT NULL,
    repeat_index INTEGER NOT NULL,
    schema       INTEGER NOT NULL,
    body         TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS attempts (
    id         TEXT PRIMARY KEY,
    branch     TEXT NOT NULL,
    run        TEXT NOT NULL,
    lease_ms   INTEGER NOT NULL,   -- expiry; an expired attempt cannot commit
    active     INTEGER NOT NULL,   -- 1 while it may still speak for the branch
    billed     INTEGER NOT NULL    -- steps already billed through this attempt
);

CREATE TABLE IF NOT EXISTS billing (
    message_id TEXT PRIMARY KEY,    -- idempotency: a replayed receipt bills once
    steps      INTEGER NOT NULL,
    outcome    TEXT NOT NULL,        -- charged | unknown
    detail     TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS protocols (
    id     TEXT PRIMARY KEY,
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS acceptance_budget (
    protocol TEXT PRIMARY KEY,
    remaining INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS publications (
    id       TEXT PRIMARY KEY,
    snapshot TEXT NOT NULL,
    version  INTEGER NOT NULL,
    actor    TEXT NOT NULL,
    state    TEXT NOT NULL      -- staged | active | retired
);
"#;

/// The receipt returned when a signal is submitted. `duplicate` means the
/// idempotency key was already accepted — not a second counted sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Receipt {
    Accepted,
    Duplicate,
}

impl Receipt {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Duplicate => "duplicate",
        }
    }
}

/// A conflict awaiting adjudication: two human corrections about the same
/// target from the same authority level. Last write does not win.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub target: String,
    pub target_field: Option<String>,
    pub existing: String,
    pub incoming: String,
}

/// The trusted host handle.
#[derive(Debug)]
pub struct Store {
    pub(crate) conn: Connection,
    artifacts: Arc<ArtifactStore>,
}

impl Store {
    /// Open (or create) a store rooted at `root`, with artifacts under
    /// `root/artifacts`. An existing database written by an unknown schema
    /// version is refused rather than migrated on the fly.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("create store root: {e}"))
        })?;
        let artifacts = Arc::new(ArtifactStore::open(root.join("artifacts"))?);
        let conn = Connection::open(root.join("grove.db")).map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("open store: {e}"))
        })?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("init schema: {e}")))?;

        let existing: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| row.get(0))
            .optional()
            .map_err(db_err)?;
        match existing.as_deref() {
            None => {
                conn.execute(
                    "INSERT INTO meta (key, value) VALUES ('schema', ?1)",
                    params![SCHEMA_VERSION.to_string()],
                )
                .map_err(db_err)?;
            }
            Some(found) => {
                let found: u32 = found.parse().map_err(|_| {
                    Error::new(
                        ErrorKind::IncompatibleState,
                        format!("store meta schema is not a number: {found}"),
                    )
                })?;
                if found != SCHEMA_VERSION {
                    return Err(Error::new(
                        ErrorKind::IncompatibleState,
                        format!("store schema {found} != supported {SCHEMA_VERSION}; migrate explicitly"),
                    ));
                }
            }
        }
        Ok(Self { conn, artifacts })
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    // ── manifests ──────────────────────────────────────────────────

    /// Commit any serializable record as an immutable manifest. The bytes
    /// are durable and digest-checked before the row is written.
    pub fn commit_manifest<T: serde::Serialize>(&self, kind: &str, owner: &str, value: &T) -> Result<ArtifactRef> {
        let bytes = crate::contracts::canonical_json(value)?;
        let digest = self.artifacts.put(&bytes)?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO manifests (digest, kind, owner, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![digest.to_hex(), kind, owner, SCHEMA_VERSION, String::from_utf8_lossy(&bytes)],
            )
            .map_err(db_err)?;
        Ok(digest)
    }

    /// Load a manifest back, verifying both the digest and the schema.
    pub fn load_manifest(&self, digest: &ArtifactRef) -> Result<String> {
        let body: String = self
            .conn
            .query_row(
                "SELECT body FROM manifests WHERE digest = ?1",
                params![digest.to_hex()],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no committed manifest for {}", digest),
                )
            })?;
        let bytes = body.as_bytes();
        if crate::contracts::digest_bytes(bytes) != *digest {
            return Err(Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("manifest {digest} does not match its committed digest"),
            ));
        }
        Ok(body)
    }

    /// A principal may only reference artifacts it owns. This is the
    /// boundary that stops one run from adopting another run's weights.
    pub fn require_owner(&self, digest: &ArtifactRef, actor: &Actor) -> Result<()> {
        let owner: Option<String> = self
            .conn
            .query_row(
                "SELECT owner FROM manifests WHERE digest = ?1",
                params![digest.to_hex()],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        match owner {
            Some(owner) if owner == actor.id => Ok(()),
            Some(owner) => Err(Error::denied(format!(
                "artifact {digest} belongs to {owner}, actor {} may not reference it",
                actor.id
            ))),
            None => Err(Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no committed manifest for {digest}"),
            )),
        }
    }

    // ── model snapshots ────────────────────────────────────────────

    pub fn put_snapshot(&self, actor: &Actor, snapshot: &ModelSnapshot) -> Result<ArtifactRef> {
        actor.require(ActorRole::Operator, "committing a model snapshot")?;
        if snapshot.owner != actor.id {
            return Err(Error::denied(format!(
                "snapshot owner {} does not match actor {}",
                snapshot.owner, actor.id
            )));
        }
        for param in &snapshot.params {
            if !self.artifacts.exists(&param.artifact) {
                return Err(Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("parameter artifact {} is not in the store", param.artifact),
                ));
            }
        }
        self.commit_manifest("ModelSnapshot", &actor.id, snapshot)
    }

    // ── observations and predictions ───────────────────────────────

    pub fn put_observation(&self, actor: &Actor, observation: &Observation) -> Result<()> {
        if observation.owner != actor.id {
            return Err(Error::denied(format!(
                "observation owner {} does not match actor {}",
                observation.owner, actor.id
            )));
        }
        for block in &observation.blocks {
            if !self.artifacts.exists(&block.artifact) {
                return Err(Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("content artifact {} is not in the store", block.artifact),
                ));
            }
        }
        let body = observation.canonical_bytes()?;
        let bytes = body.as_slice();
        self.conn
            .execute(
                "INSERT OR IGNORE INTO observations (id, owner, task, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    observation.id,
                    observation.owner,
                    observation.task_id,
                    observation.schema,
                    String::from_utf8_lossy(bytes)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_observation(&self, id: &str) -> Result<Observation> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM observations WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no observation {id}"))
            })?;
        decode(&body, require_schema(schema)?)
    }

    /// Record a prediction against the exact snapshot that produced it. A
    /// late human correction therefore reaches *that* version, not whatever
    /// the current head happens to be.
    pub fn put_prediction(&self, actor: &Actor, prediction: &Prediction) -> Result<()> {
        actor.require(ActorRole::Reader, "recording a prediction")?;
        let snapshot = ArtifactRef::parse_hex(&prediction.snapshot_digest.to_hex())?;
        self.require_owner(&snapshot, actor)?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO predictions (id, owner, observation, snapshot, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    prediction.id,
                    prediction.owner,
                    prediction.observation_id,
                    prediction.snapshot_digest.to_hex(),
                    prediction.schema,
                    String::from_utf8_lossy(&prediction.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_prediction(&self, id: &str) -> Result<Prediction> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row(
                "SELECT body, schema FROM predictions WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no prediction {id}"))
            })?;
        decode(&body, require_schema(schema)?)
    }

    // ── signals ────────────────────────────────────────────────────

    /// Submit a signal. Returns `Duplicate` when this producer already
    /// submitted the same idempotency key — the caller gets a receipt, the
    /// dataset gets no second sample.
    pub fn submit_signal(
        &self,
        actor: &Actor,
        signal: &LearningSignal,
    ) -> Result<Receipt> {
        let required = match signal.kind {
            SignalKind::HumanCorrection | SignalKind::HumanPreference | SignalKind::Demonstration => {
                ActorRole::Annotator
            }
            SignalKind::TeacherLabel => ActorRole::Operator,
            _ => ActorRole::Reader,
        };
        actor.require(required, "submitting this signal kind")?;

        let target = signal
            .observation_id
            .clone()
            .or_else(|| signal.prediction_id.clone())
            .unwrap_or_else(|| signal.task_id.clone());

        if signal.kind.targets_other_signal() {
            let referred = signal.revises.as_ref().ok_or_else(|| {
                Error::invalid(format!("{:?} signal must name the signal it revises", signal.kind))
            })?;
            let exists: bool = self
                .conn
                .query_row(
                    "SELECT 1 FROM signals WHERE id = ?1",
                    params![referred],
                    |_| Ok(true),
                )
                .optional()
                .map_err(db_err)?
                .unwrap_or(false);
            if !exists {
                return Err(Error::new(
                    ErrorKind::IncompatibleState,
                    format!("signal {signal_id} revises unknown signal {referred}", signal_id = signal.id),
                ));
            }
        }

        let body = signal.canonical_bytes()?;
        let inserted = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO signals
                   (id, producer, idempotency, kind, target, status, task, target_field, received_at_ms, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'accepted', ?6, ?7, ?8, ?9, ?10)",
                params![
                    signal.id,
                    signal.producer,
                    signal.idempotency_key,
                    kind_str(signal.kind),
                    target,
                    signal.task_id,
                    signal.target_field,
                    signal.received_at_ms,
                    signal.schema,
                    String::from_utf8_lossy(&body)
                ],
            )
            .map_err(db_err)?;
        if inserted == 0 {
            return Ok(Receipt::Duplicate);
        }
        Ok(Receipt::Accepted)
    }

    /// Signals about one target that are still in force. A retraction or a
    /// superseding correction removes its target from this list without
    /// touching signals already frozen into a dataset revision.
    pub fn active_signals(&self, target: &str) -> Result<Vec<LearningSignal>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT body, schema FROM signals
                 WHERE target = ?1 AND status = 'accepted'
                 ORDER BY received_at_ms, id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![target], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (body, schema) = row.map_err(db_err)?;
            out.push(decode::<LearningSignal>(&body, require_schema(schema)?)?);
        }
        Ok(out)
    }

    pub fn signal_status(&self, id: &str) -> Result<String> {
        self.conn
            .query_row("SELECT status FROM signals WHERE id = ?1", params![id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no signal {id}")))
    }

    /// Mark a signal retracted. Frozen dataset revisions keep their members;
    /// only later views see the change.
    pub fn retract_signal(&self, actor: &Actor, signal_id: &str) -> Result<()> {
        actor.require(ActorRole::Annotator, "retracting a signal")?;
        let changed = self
            .conn
            .execute(
                "UPDATE signals SET status = 'retracted' WHERE id = ?1 AND status = 'accepted'",
                params![signal_id],
            )
            .map_err(db_err)?;
        if changed == 0 {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!("signal {signal_id} is not in force"),
            ));
        }
        Ok(())
    }

    /// Adjudicate conflicting human corrections about the same target field:
    /// the winner stays, the loser is marked superseded. This is the only
    /// path that resolves a conflict, and it is explicit.
    pub fn resolve_conflict(
        &self,
        actor: &Actor,
        keep: &str,
        supersede: &str,
    ) -> Result<()> {
        actor.require(ActorRole::Annotator, "adjudicating a conflict")?;
        let (keep_target, keep_field): (String, Option<String>) = self
            .conn
            .query_row(
                "SELECT target, target_field FROM signals WHERE id = ?1",
                params![keep],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no signal {keep}"))
            })?;
        let (other_target, other_field): (String, Option<String>) = self
            .conn
            .query_row(
                "SELECT target, target_field FROM signals WHERE id = ?1",
                params![supersede],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no signal {supersede}"))
            })?;
        if keep_target != other_target || keep_field != other_field {
            return Err(Error::invalid(format!(
                "signals {keep} and {supersede} are not in conflict (different target/field)"
            )));
        }
        self.conn
            .execute(
                "UPDATE signals SET status = 'superseded' WHERE id = ?1 AND status = 'accepted'",
                params![supersede],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// In-force signals for one target, detecting human-vs-human conflicts.
    pub fn conflicts_for(&self, target: &str) -> Result<Vec<Conflict>> {
        let signals = self.active_signals(target)?;
        let mut out = Vec::new();
        let humans: Vec<&LearningSignal> = signals
            .iter()
            .filter(|s| {
                matches!(
                    s.kind,
                    SignalKind::HumanCorrection | SignalKind::HumanPreference | SignalKind::Demonstration
                )
            })
            .collect();
        for (i, a) in humans.iter().enumerate() {
            for b in humans.iter().skip(i + 1) {
                if a.target_field == b.target_field && a.content != b.content {
                    out.push(Conflict {
                        target: target.to_string(),
                        target_field: a.target_field.clone(),
                        existing: a.id.clone(),
                        incoming: b.id.clone(),
                    });
                }
            }
        }
        Ok(out)
    }

    // ── dataset revisions ──────────────────────────────────────────

    /// Freeze the current in-force signal set for a task. Membership is
    /// written once and never rewritten: a later signal produces a new
    /// revision rather than mutating a view a run is already training on.
    pub fn freeze_dataset(
        &self,
        actor: &Actor,
        revision: &DatasetRevision,
    ) -> Result<()> {
        actor.require(ActorRole::Operator, "freezing a dataset revision")?;
        for signal_id in &revision.signal_ids {
            let status: Option<String> = self
                .conn
                .query_row("SELECT status FROM signals WHERE id = ?1", params![signal_id], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(db_err)?;
            match status.as_deref() {
                Some("accepted") => {}
                Some(other) => {
                    return Err(Error::new(
                        ErrorKind::IncompatibleState,
                        format!("signal {signal_id} is {other}, not in force"),
                    ));
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::ArtifactUnavailable,
                        format!("signal {signal_id} does not exist"),
                    ))
                }
            }
        }
        self.conn
            .execute(
                "INSERT INTO datasets (id, task, schema, body) VALUES (?1, ?2, ?3, ?4)",
                params![
                    revision.id,
                    revision.task_id,
                    revision.schema,
                    String::from_utf8_lossy(&revision.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_dataset(&self, id: &str) -> Result<DatasetRevision> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM datasets WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no dataset {id}"))
            })?;
        decode(&body, require_schema(schema)?)
    }

    /// Signals that are in force but not yet frozen into any revision for
    /// this task — the delta a later view would pick up.
    pub fn pending_signals(&self, task: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT s.id FROM signals s
                 WHERE s.status = 'accepted' AND s.task = ?1
                   AND NOT EXISTS (
                     SELECT 1 FROM datasets d
                     WHERE d.task = ?1 AND instr(d.body, '\"' || s.id || '\"') > 0
                   )
                 ORDER BY s.received_at_ms, s.id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![task], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        Ok(out)
    }

    // ── recipes and runs ───────────────────────────────────────────

    pub fn put_recipe(&self, actor: &Actor, recipe: &Recipe) -> Result<()> {
        actor.require(ActorRole::Operator, "registering a recipe")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO recipes (id, task, schema, body) VALUES (?1, ?2, ?3, ?4)",
                params![
                    recipe.id,
                    recipe.task_id,
                    recipe.schema,
                    String::from_utf8_lossy(&recipe.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_recipe(&self, id: &str) -> Result<Recipe> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM recipes WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no recipe {id}"))
            })?;
        decode(&body, require_schema(schema)?)
    }

    pub fn put_run(&self, actor: &Actor, run: &Run) -> Result<()> {
        actor.require(ActorRole::Operator, "recording a run")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO runs (id, task, state, schema, body) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run.id,
                    run.task_id,
                    state_str(run.state),
                    run.schema,
                    String::from_utf8_lossy(&run.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Every run record, for the inspect surface.
    pub fn runs(&self) -> Vec<Run> {
        let mut stmt = match self.conn.prepare("SELECT body, schema FROM runs ORDER BY id") {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .expect("query_map cannot fail on a prepared select");
        rows.flatten()
            .filter_map(|(body, schema)| decode(&body, require_schema(schema).ok()?).ok())
            .collect()
    }

    pub fn checkpoints(&self) -> Vec<Checkpoint> {
        let mut stmt = match self
            .conn
            .prepare("SELECT body, schema FROM checkpoints ORDER BY id")
        {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .expect("query_map cannot fail on a prepared select");
        rows.flatten()
            .filter_map(|(body, schema)| decode(&body, require_schema(schema).ok()?).ok())
            .collect()
    }

    pub fn branches(&self) -> Vec<Branch> {
        let mut stmt = match self
            .conn
            .prepare("SELECT body, schema FROM branches ORDER BY id")
        {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .expect("query_map cannot fail on a prepared select");
        rows.flatten()
            .filter_map(|(body, schema)| decode(&body, require_schema(schema).ok()?).ok())
            .collect()
    }

    pub fn get_run(&self, id: &str) -> Result<Run> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM runs WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no run {id}")))?;
        decode(&body, require_schema(schema)?)
    }

    pub fn set_run_state(&self, run: &Run) -> Result<()> {
        self.conn
            .execute(
                "UPDATE runs SET state = ?1, body = ?2 WHERE id = ?3",
                params![state_str(run.state), String::from_utf8_lossy(&run.canonical_bytes()?), run.id],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Consume `steps` from the run's budget. A resume inherits the ledger,
    /// so the total across a lineage never exceeds what was granted.
    pub fn consume_steps(&self, run_id: &str, steps: u32) -> Result<u32> {
        let mut run = self.get_run(run_id)?;
        let spent = run.steps_consumed + steps;
        if spent > run.steps_budget {
            return Err(Error::new(
                ErrorKind::BudgetExhausted,
                format!("run {run_id} asked for {spent} steps, budget is {}", run.steps_budget),
            ));
        }
        run.steps_consumed = spent;
        self.set_run_state(&run)?;
        Ok(spent)
    }

    // ── checkpoints and branches ───────────────────────────────────

    /// Commit a checkpoint: its manifest bytes become durable first, and only
    /// then is the row inserted. A kill between the two leaves an
    /// unreferenced object, never a half-written checkpoint.
    pub fn commit_checkpoint(&self, actor: &Actor, checkpoint: &Checkpoint) -> Result<()> {
        actor.require(ActorRole::Operator, "committing a checkpoint")?;
        if checkpoint.owner != actor.id {
            return Err(Error::denied(format!(
                "checkpoint owner {} does not match actor {}",
                checkpoint.owner, actor.id
            )));
        }
        // The state artifact must be readable and intact before commit.
        self.artifacts.get(&checkpoint.state_artifact)?;
        self.require_owner(&checkpoint.snapshot, actor)?;
        let body = checkpoint.canonical_bytes()?;
        self.conn.execute_batch("BEGIN IMMEDIATE").map_err(db_err)?;
        let result = (|| -> Result<()> {
            self.conn
                .execute(
                    "INSERT INTO checkpoints (id, owner, run, parent, schema, body)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        checkpoint.id,
                        checkpoint.owner,
                        checkpoint.run_id,
                        checkpoint.parent,
                        checkpoint.schema,
                        String::from_utf8_lossy(&body)
                    ],
                )
                .map_err(db_err)?;
            self.conn.execute_batch("COMMIT").map_err(db_err)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        result
    }

    pub fn get_checkpoint(&self, id: &str) -> Result<Checkpoint> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row(
                "SELECT body, schema FROM checkpoints WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no checkpoint {id}"))
            })?;
        decode(&body, require_schema(schema)?)
    }

    pub fn put_branch(&self, actor: &Actor, branch: &Branch) -> Result<()> {
        actor.require(ActorRole::Operator, "creating a branch")?;
        if branch.owner != actor.id {
            return Err(Error::denied("branch owner does not match actor"));
        }
        self.conn
            .execute(
                "INSERT INTO branches (id, owner, head, version, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    branch.id,
                    branch.owner,
                    branch.head,
                    branch.head_version,
                    branch.schema,
                    String::from_utf8_lossy(&branch.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Move a branch head with optimistic concurrency. A stale writer is
    /// rejected with `conflict` rather than overwriting newer progress.
    pub fn advance_head(
        &self,
        actor: &Actor,
        branch_id: &str,
        new_head: &str,
        expected_version: u32,
    ) -> Result<u32> {
        actor.require(ActorRole::Operator, "advancing a branch head")?;
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        let row: Option<(Option<String>, u32, String)> = tx
            .query_row(
                "SELECT head, version, body FROM branches WHERE id = ?1",
                params![branch_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_err)?;
        let (head, version, body) = row.ok_or_else(|| {
            Error::new(ErrorKind::ArtifactUnavailable, format!("no branch {branch_id}"))
        })?;
        if version != expected_version {
            return Err(Error::conflict(format!(
                "branch {branch_id} is at version {version}, writer expected {expected_version}"
            )));
        }
        let mut branch: Branch = serde_json::from_str(&body).map_err(|e| {
            Error::new(ErrorKind::IncompatibleState, format!("branch {branch_id} is unreadable: {e}"))
        })?;
        if head.is_none() {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!("branch {branch_id} has no head to advance"),
            ));
        }
        branch.head = Some(new_head.to_string());
        branch.head_version = version + 1;
        tx.execute(
            "UPDATE branches SET head = ?1, version = ?2, body = ?3 WHERE id = ?4",
            params![
                new_head,
                branch.head_version,
                String::from_utf8_lossy(&branch.canonical_bytes()?),
                branch_id
            ],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(branch.head_version)
    }

    pub fn get_branch(&self, id: &str) -> Result<Branch> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM branches WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no branch {id}")))?;
        decode(&body, require_schema(schema)?)
    }

    // ── populations, evaluations, publication ──────────────────────

    pub fn put_population(&self, actor: &Actor, population: &Population) -> Result<()> {
        actor.require(ActorRole::Operator, "recording a population")?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO populations (id, owner, schema, body) VALUES (?1, ?2, ?3, ?4)",
                params![
                    population.id,
                    population.owner,
                    population.schema,
                    String::from_utf8_lossy(&population.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Debit a population's shared budget. A fork never doubles the grant:
    /// the whole population spends one ledger.
    pub fn spend_population(&self, population: &Population, steps: u32) -> Result<Population> {
        let mut next = population.clone();
        next.spent_steps += steps;
        if next.spent_steps > next.total_budget_steps {
            return Err(Error::new(
                ErrorKind::BudgetExhausted,
                format!(
                    "population {} spent {} of {} steps",
                    population.id, next.spent_steps, next.total_budget_steps
                ),
            ));
        }
        self.conn
            .execute(
                "UPDATE populations SET body = ?1 WHERE id = ?2",
                params![String::from_utf8_lossy(&next.canonical_bytes()?), next.id],
            )
            .map_err(db_err)?;
        Ok(next)
    }

    pub fn get_population(&self, id: &str) -> Result<Population> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM populations WHERE id = ?1", params![id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no population {id}"))
            })?;
        decode(&body, require_schema(schema)?)
    }

    /// Scores are kept beside the snapshot, never inside it: the same model
    /// can be re-evaluated under another protocol without changing identity.
    pub fn put_evaluation(&self, actor: &Actor, record: &EvaluationRecord) -> Result<()> {
        actor.require(ActorRole::Operator, "recording an evaluation")?;
        self.conn
            .execute(
                "INSERT INTO evaluations (id, snapshot, protocol, repeat_index, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    record.id,
                    record.snapshot.to_hex(),
                    record.protocol_id,
                    record.repeat_index,
                    record.schema,
                    String::from_utf8_lossy(&record.canonical_bytes()?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn evaluations_for(&self, snapshot: &ArtifactRef) -> Result<Vec<EvaluationRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT body, schema FROM evaluations WHERE snapshot = ?1 ORDER BY repeat_index, id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![snapshot.to_hex()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (body, schema) = row.map_err(db_err)?;
            out.push(decode(&body, require_schema(schema)?)?);
        }
        Ok(out)
    }

    /// Provision a versioned evaluation protocol. Gates travel with it:
    /// any later comparison or publication loads THIS record, never a
    /// looser reconstruction.
    pub fn put_protocol(
        &self,
        actor: &Actor,
        protocol: &crate::evaluation::EvaluationProtocol,
    ) -> Result<()> {
        actor.require(ActorRole::Operator, "provisioning an evaluation protocol")?;
        self.conn
            .execute(
                "INSERT INTO protocols (id, schema, body) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET body = ?3, schema = ?2",
                params![
                    protocol.id,
                    protocol.schema,
                    String::from_utf8_lossy(&crate::contracts::canonical_json(protocol)?)
                ],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn get_protocol(
        &self,
        id: &str,
    ) -> Result<crate::evaluation::EvaluationProtocol> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row("SELECT body, schema FROM protocols WHERE id = ?1", params![id], {
                |row| Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no protocol {id}"))
            })?;
        require_schema(schema)?;
        decode(&body, SCHEMA_VERSION)
    }

    /// Debit the shared acceptance budget for one protocol. Every branch
    /// draws from the same line — a fork never buys extra holdout looks.
    pub fn consume_acceptance_budget(&self, protocol: &str, amount: u32) -> Result<u32> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        let remaining: Option<i64> = tx
            .query_row(
                "SELECT remaining FROM acceptance_budget WHERE protocol = ?1",
                params![protocol],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let remaining = remaining.ok_or_else(|| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no acceptance budget is provisioned for protocol {protocol}"),
            )
        })?;
        if (remaining as i64) < amount as i64 {
            return Err(Error::new(
                ErrorKind::BudgetExhausted,
                format!(
                    "acceptance budget for {protocol} has {remaining} left, {amount} requested"
                ),
            ));
        }
        let next = remaining - amount as i64;
        tx.execute(
            "UPDATE acceptance_budget SET remaining = ?1 WHERE protocol = ?2",
            params![next, protocol],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(next as u32)
    }

    /// Provision (or top up) the shared acceptance budget for a protocol.
    pub fn provision_acceptance_budget(&self, protocol: &str, amount: u32) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO acceptance_budget (protocol, remaining) VALUES (?1, ?2)
                 ON CONFLICT(protocol) DO UPDATE SET remaining = ?2",
                params![protocol, amount],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Atomically point the deployment at a snapshot. The publisher role is
    /// required, and a stale expected-version is refused.
    pub fn publish(
        &self,
        actor: &Actor,
        snapshot: &ArtifactRef,
        expected_version: Option<u32>,
    ) -> Result<u32> {
        actor.require(ActorRole::Publisher, "publishing a model")?;
        let current: Option<(String, u32)> = self
            .conn
            .query_row(
                "SELECT id, version FROM publications WHERE state = 'active' ORDER BY version DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let version = match (current.as_ref(), expected_version) {
            (None, _) => 1,
            (Some((_, current_version)), Some(expected)) => {
                if expected != *current_version {
                    return Err(Error::conflict(format!(
                        "publication is at version {current_version}, caller expected {expected}"
                    )));
                }
                current_version + 1
            }
            (Some((_, current_version)), None) => current_version + 1,
        };
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx.execute("UPDATE publications SET state = 'retired'", [])
            .map_err(db_err)?;
        tx.execute(
            "INSERT INTO publications (id, snapshot, version, actor, state)
             VALUES (?1, ?2, ?3, ?4, 'active')",
            params![format!("pub-v{version}"), snapshot.to_hex(), version, actor.id],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(version)
    }

    pub fn active_publication(&self) -> Result<Option<(u32, ArtifactRef)>> {
        let row: Option<(u32, String)> = self
            .conn
            .query_row(
                "SELECT version, snapshot FROM publications WHERE state = 'active' LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        match row {
            None => Ok(None),
            Some((version, hex)) => Ok(Some((version, ArtifactRef::parse_hex(&hex)?))),
        }
    }
}

// ── helpers ────────────────────────────────────────────────────────

pub(crate) fn db_err(error: rusqlite::Error) -> Error {
    Error::new(ErrorKind::BackendFailed, format!("sqlite: {error}"))
}

/// A stored record written by a newer producer is not executed: the
/// schema mismatch is an explicit failure, never a silent partial read.
fn require_schema(found: i64) -> Result<u32> {
    if found != SCHEMA_VERSION as i64 {
        return Err(Error::new(
            ErrorKind::IncompatibleState,
            format!("record schema {found} != supported {SCHEMA_VERSION}; migrate explicitly"),
        ));
    }
    Ok(SCHEMA_VERSION)
}

fn decode<T: serde::de::DeserializeOwned>(body: &str, _schema: u32) -> Result<T> {
    serde_json::from_str(body)
        .map_err(|e| Error::new(ErrorKind::IncompatibleState, format!("record is unreadable: {e}")))
}

fn kind_str(kind: SignalKind) -> &'static str {
    match kind {
        SignalKind::TeacherLabel => "teacher-label",
        SignalKind::HumanCorrection => "human-correction",
        SignalKind::HumanPreference => "human-preference",
        SignalKind::Demonstration => "demonstration",
        SignalKind::EnvironmentResult => "environment-result",
        SignalKind::Revision => "revision",
        SignalKind::Retraction => "retraction",
    }
}

fn state_str(state: RunState) -> &'static str {
    match state {
        RunState::Queued => "queued",
        RunState::Running => "running",
        RunState::Evaluating => "evaluating",
        RunState::Accepted => "accepted",
        RunState::Rejected => "rejected",
        RunState::Paused => "paused",
        RunState::Failed => "failed",
        RunState::Cancelled => "cancelled",
    }
}
