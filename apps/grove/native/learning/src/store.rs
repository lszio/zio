//! SQLite-backed store: manifests, references and optimistic head updates.
//!
//! The invariant this module defends is *no committed reference to a byte
//! that is not durable*. Every write path is therefore: persist artifact
//! bytes first, verify the digest, then commit the manifest row in a
//! transaction that re-checks the schema version. A crash between the two
//! steps leaves an unreferenced object, which is garbage-collectable; it
//! never leaves a committed record that cannot be loaded.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

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

-- Operation receipts (G03). A replayed request must get the answer the
-- first one got, and it must get it *after a restart* — which is why
-- this is a table and not the API's memory. `body` is the exact
-- response, so the replay is the original rather than a re-derivation.
CREATE TABLE IF NOT EXISTS receipts (
    operation_id TEXT PRIMARY KEY,
    status       INTEGER NOT NULL,
    body         TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

-- What a queued run is for (G03). The queue stores the *request*, not
-- an inference rule: a run that says what to build, so an owner that
-- restarts can pick the work up without the original caller in memory.
CREATE TABLE IF NOT EXISTS queued_work (
    run      TEXT PRIMARY KEY,
    kind     TEXT NOT NULL,      -- zio | training
    payload  TEXT NOT NULL,
    schema   INTEGER NOT NULL
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

-- Retraction propagation (W12): a frozen view keeps its members, but every
-- snapshot whose lineage consumed a revoked signal stops being deployable.
CREATE TABLE IF NOT EXISTS invalidations (
    snapshot   TEXT NOT NULL,   -- the invalidated ModelSnapshot digest
    signal     TEXT NOT NULL,   -- the retracted signal
    dataset    TEXT NOT NULL,   -- the frozen revision that carried it
    PRIMARY KEY (snapshot, signal)
);

CREATE INDEX IF NOT EXISTS invalidations_by_signal ON invalidations (signal);

-- Logic candidates (G04). A candidate is immutable; only `state` moves,
-- and only through this crate's governance functions. The evaluation
-- lives in `evaluations`, bound to the candidate's *source* digest — so
-- a qualification cannot be talked about without naming bytes.
CREATE TABLE IF NOT EXISTS candidates (
    id     TEXT PRIMARY KEY,
    owner  TEXT NOT NULL,
    state  TEXT NOT NULL,      -- proposed|qualified|active|rejected|declined
    source TEXT NOT NULL,      -- the proposed source artifact
    schema INTEGER NOT NULL,
    body   TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

-- Human approvals (G04). Written only for an authenticated publisher,
-- and only after the evaluation it names is checked against the
-- artifact being published. There is deliberately no boolean here:
-- an approval IS this row.
CREATE TABLE IF NOT EXISTS approvals (
    id                 TEXT PRIMARY KEY,
    subject            TEXT NOT NULL,
    candidate          TEXT NOT NULL,   -- the artifact to be published
    actor              TEXT NOT NULL,
    expected_version   INTEGER,         -- NULL = no version was claimed
    decision           TEXT NOT NULL,
    schema             INTEGER NOT NULL,
    body               TEXT NOT NULL,
    created_at_ms      INTEGER NOT NULL
);

-- Every evaluation an approval rests on. Kept as its own rows rather
-- than a blob inside the approval, because "is this approval still
-- backed by real records?" is a question this store must be able to
-- answer without trusting the approval's own summary of itself.
CREATE TABLE IF NOT EXISTS approval_evaluations (
    approval  TEXT NOT NULL,
    candidate TEXT NOT NULL,
    PRIMARY KEY (approval, candidate)
);

-- Declines and refusals (G04). Kept, not deleted: "we looked at this
-- and said no" is the fact that stops the same proposal returning.
CREATE TABLE IF NOT EXISTS rejections (
    subject TEXT PRIMARY KEY,
    actor   TEXT NOT NULL,
    reason  TEXT NOT NULL,
    at_ms   INTEGER NOT NULL
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

/// What a retraction actually invalidated. The frozen revisions keep their
/// members — this is the other half of the fact, and the half that governs
/// what may still be deployed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetractionPropagation {
    pub signal_id: String,
    /// Frozen dataset revisions that carried the signal.
    pub dataset_revisions: Vec<String>,
    /// Runs trained on those revisions.
    pub invalidated_runs: Vec<String>,
    /// Checkpoints of those runs.
    pub invalidated_checkpoints: Vec<String>,
    /// Snapshots those checkpoints and runs deployed to.
    pub invalidated_snapshots: Vec<ArtifactRef>,
}

/// Why a snapshot may no longer be deployed. `recoverable` is always false
/// here: retracting data does not un-train weights, so the only way back is
/// retraining, not redeploying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invalidation {
    pub snapshot: ArtifactRef,
    pub revoked_signals: Vec<String>,
    pub dataset_revisions: Vec<String>,
    pub recoverable: bool,
}

/// The outcome of one cleanup pass: exactly what went, and exactly what was
/// left alone. A cleanup that cannot say which is which is not auditable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CleanupReport {
    pub horizon_ms: u64,
    pub deleted: Vec<ArtifactRef>,
    /// Reachable objects older than the horizon. These are kept, every
    /// time, and naming them is how a reader can tell the difference.
    pub refused: Vec<ArtifactRef>,
    /// Unreachable objects still inside the retention window.
    pub retained_young: Vec<ArtifactRef>,
}

/// What a successful claim took. Enough for the owner to run the work
/// and to know what it may still say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedRun {
    pub run_id: String,
    pub attempt_id: String,
    pub reserved_steps: u32,
    pub lease_expires_ms: i64,
}

/// The trusted host handle.
#[derive(Debug)]
pub struct Store {
    pub(crate) conn: Connection,
    artifacts: Arc<ArtifactStore>,
}

impl Store {
    /// The connection, for the modules that own their own tables.
    ///
    /// Not a general escape hatch: it is here so a table's owner is the
    /// only code that writes it, rather than every writer reaching into
    /// one shared pool of SQL.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Open (or create) a store rooted at `root`, with artifacts under
    /// `root/artifacts`. An existing database written by an unknown schema
    /// version is refused rather than migrated on the fly.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        std::fs::create_dir_all(root)
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("create store root: {e}")))?;
        let artifacts = Arc::new(ArtifactStore::open(root.join("artifacts"))?);
        let conn = Connection::open(root.join("grove.db"))
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("open store: {e}")))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("init schema: {e}")))?;
        crate::events::install_schema(&conn).map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("init events schema: {e}"))
        })?;

        let existing: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |row| {
                row.get(0)
            })
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
                        format!(
                            "store schema {found} != supported {SCHEMA_VERSION}; migrate explicitly"
                        ),
                    ));
                }
            }
        }
        let store = Self { conn, artifacts };
        // Restart recovery: a lease whose worker died with its process must
        // not keep a slot open, and no receipt from before the restart may
        // land. Every lease is released here; ownership is claimed by the
        // coordinator, which knows the epoch a receipt was stamped with.
        store.reclaim_leases()?;
        Ok(store)
    }

    /// Release every lease. Their workers are gone, so keeping a slot busy
    /// would only idle the budget and let a zombie receipt through.
    pub(crate) fn reclaim_leases(&self) -> Result<()> {
        self.conn
            .execute("UPDATE attempts SET active = 0 WHERE active = 1", [])
            .map_err(db_err)
            .map(|_| ())
    }

    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    // ── manifests ──────────────────────────────────────────────────

    /// Commit any serializable record as an immutable manifest. The bytes
    /// are durable and digest-checked before the row is written.
    pub fn commit_manifest<T: serde::Serialize>(
        &self,
        kind: &str,
        owner: &str,
        value: &T,
    ) -> Result<ArtifactRef> {
        let bytes = crate::contracts::canonical_json(value)?;
        let digest = self.artifacts.put(&bytes)?;
        self.conn
            .execute(
                "INSERT OR IGNORE INTO manifests (digest, kind, owner, schema, body)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    digest.to_hex(),
                    kind,
                    owner,
                    SCHEMA_VERSION,
                    String::from_utf8_lossy(&bytes)
                ],
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

    /// Load a committed snapshot manifest, verifying the digest.
    pub fn load_snapshot(&self, digest: &ArtifactRef) -> Result<ModelSnapshot> {
        let body = self.load_manifest(digest)?;
        serde_json::from_str(&body).map_err(|e| {
            Error::new(
                ErrorKind::IncompatibleState,
                format!("snapshot {digest} is unreadable: {e}"),
            )
        })
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
            .query_row(
                "SELECT body, schema FROM observations WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no observation {id}"),
                )
            })?;
        decode(&body, require_schema(schema)?)
    }

    /// Record a prediction against the exact snapshot that produced it. A
    /// late human correction therefore reaches *that* version, not whatever
    /// the current head happens to be.
    ///
    /// Ownership is *not* required here, and that is deliberate: the
    /// ownership rule protects a **citation** of someone's artifact (a
    /// run must not build on, or a module compose from, weights it did
    /// not produce). A prediction is not a citation — it is the record
    /// that a reader ran the model the deployment already serves. If the
    /// active publication required ownership, no product could ever
    /// record what its own users saw, which is the one thing a
    /// correction must be able to point at.
    ///
    /// The binding is still exact: the snapshot must be committed, so a
    /// prediction can never name a model that does not exist.
    pub fn put_prediction(&self, actor: &Actor, prediction: &Prediction) -> Result<()> {
        actor.require(ActorRole::Reader, "recording a prediction")?;
        self.load_manifest(&prediction.snapshot_digest)?;
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
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no prediction {id}"),
                )
            })?;
        decode(&body, require_schema(schema)?)
    }

    // ── signals ────────────────────────────────────────────────────

    /// Submit a signal. Returns `Duplicate` when this producer already
    /// submitted the same idempotency key — the caller gets a receipt, the
    /// dataset gets no second sample.
    pub fn submit_signal(&self, actor: &Actor, signal: &LearningSignal) -> Result<Receipt> {
        let required = match signal.kind {
            SignalKind::HumanCorrection
            | SignalKind::HumanPreference
            | SignalKind::Demonstration => ActorRole::Annotator,
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
                Error::invalid(format!(
                    "{:?} signal must name the signal it revises",
                    signal.kind
                ))
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
                    format!(
                        "signal {signal_id} revises unknown signal {referred}",
                        signal_id = signal.id
                    ),
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
            .query_row(
                "SELECT status FROM signals WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no signal {id}")))
    }

    /// Mark a signal retracted. Frozen dataset revisions keep their members;
    /// only later views see the change. The propagation of what this
    /// invalidates is [`Store::retract_signal`], which calls this and then
    /// walks the lineage.
    pub fn withdraw_signal(&self, actor: &Actor, signal_id: &str) -> Result<()> {
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
    pub fn resolve_conflict(&self, actor: &Actor, keep: &str, supersede: &str) -> Result<()> {
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
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no signal {supersede}"),
                )
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
                    SignalKind::HumanCorrection
                        | SignalKind::HumanPreference
                        | SignalKind::Demonstration
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
    pub fn freeze_dataset(&self, actor: &Actor, revision: &DatasetRevision) -> Result<()> {
        actor.require(ActorRole::Operator, "freezing a dataset revision")?;
        for signal_id in &revision.signal_ids {
            let status: Option<String> = self
                .conn
                .query_row(
                    "SELECT status FROM signals WHERE id = ?1",
                    params![signal_id],
                    |r| r.get(0),
                )
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
                    ));
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
            .query_row(
                "SELECT body, schema FROM datasets WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
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
            .query_row(
                "SELECT body, schema FROM recipes WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no recipe {id}")))?;
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
        let mut stmt = match self
            .conn
            .prepare("SELECT body, schema FROM runs ORDER BY id")
        {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
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
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
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
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query_map cannot fail on a prepared select");
        rows.flatten()
            .filter_map(|(body, schema)| decode(&body, require_schema(schema).ok()?).ok())
            .collect()
    }

    pub fn get_run(&self, id: &str) -> Result<Run> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row(
                "SELECT body, schema FROM runs WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no run {id}")))?;
        decode(&body, require_schema(schema)?)
    }

    pub fn set_run_state(&self, run: &Run) -> Result<()> {
        self.conn
            .execute(
                "UPDATE runs SET state = ?1, body = ?2 WHERE id = ?3",
                params![
                    state_str(run.state),
                    String::from_utf8_lossy(&run.canonical_bytes()?),
                    run.id
                ],
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
                format!(
                    "run {run_id} asked for {spent} steps, budget is {}",
                    run.steps_budget
                ),
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
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no checkpoint {id}"),
                )
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
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no branch {branch_id}"),
            )
        })?;
        if version != expected_version {
            return Err(Error::conflict(format!(
                "branch {branch_id} is at version {version}, writer expected {expected_version}"
            )));
        }
        let mut branch: Branch = serde_json::from_str(&body).map_err(|e| {
            Error::new(
                ErrorKind::IncompatibleState,
                format!("branch {branch_id} is unreadable: {e}"),
            )
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
            .query_row(
                "SELECT body, schema FROM branches WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| Error::new(ErrorKind::ArtifactUnavailable, format!("no branch {id}")))?;
        decode(&body, require_schema(schema)?)
    }

    // ── the run queue ──────────────────────────────────────────────

    /// Runs waiting to be claimed, oldest first.
    ///
    /// Reading a list and then claiming one is a race, so this is only
    /// for inspection; claiming goes through [`Store::claim_queued_run`],
    /// which does the selection and the write in one transaction.
    pub fn queued_runs(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM runs WHERE state = 'queued' ORDER BY id")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_err)?);
        }
        Ok(out)
    }

    /// Take one queued run, reserving its steps and opening an attempt —
    /// in a single transaction.
    ///
    /// The three writes belong together: if the state moved but the
    /// steps did not, a crash would hand the same budget out twice; if
    /// the steps moved but the state did not, the run would look queued
    /// forever with its grant already spent. The conditional UPDATE on
    /// `state = 'queued'` is what makes two owners resolve: SQLite
    /// serialises the writes, so exactly one sees a row it changed.
    pub fn claim_queued_run(
        &self,
        actor: &Actor,
        run_id: &str,
        attempt_id: &str,
        reserve_steps: u32,
        lease_ms: i64,
        now_ms: i64,
    ) -> Result<ClaimedRun> {
        actor.require(ActorRole::Operator, "claiming a queued run")?;
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;

        // `runs` indexes the state but keeps the budget in the record
        // body, so the budget is read from the decoded run rather than
        // from a column that does not exist.
        let (state, body): (String, String) = tx
            .query_row(
                "SELECT state, body FROM runs WHERE id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(ErrorKind::ArtifactUnavailable, format!("no run {run_id}"))
            })?;
        if state != "queued" {
            // Another owner got here first. That is a lost race, not a
            // broken queue, and the caller treats it as "nothing to do".
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("run {run_id} is {state}, not queued"),
            ));
        }
        let mut record: Run = serde_json::from_str(&body).map_err(|e| {
            Error::new(
                ErrorKind::IncompatibleState,
                format!("run {run_id} is unreadable: {e}"),
            )
        })?;
        let consumed = record.steps_consumed as u64;
        if record.steps_budget as u64 - consumed < reserve_steps as u64 {
            // Refused, not truncated: a run that cannot afford its own
            // work needs a decision, and a quietly smaller run is a
            // result nobody asked for.
            return Err(Error::new(
                ErrorKind::BudgetExhausted,
                format!(
                    "run {run_id} has {} steps left, the claim needs {reserve_steps}",
                    record.steps_budget as i64 - consumed as i64
                ),
            ));
        }

        record.steps_consumed = consumed as u32 + reserve_steps;
        record.state = RunState::Running;
        let updated = tx
            .execute(
                "UPDATE runs SET state = 'running', body = ?1 WHERE id = ?2 AND state = 'queued'",
                params![String::from_utf8_lossy(&record.canonical_bytes()?), run_id],
            )
            .map_err(db_err)?;
        if updated != 1 {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("run {run_id} was claimed by another owner"),
            ));
        }

        tx.execute(
            "INSERT INTO attempts (id, branch, run, lease_ms, active, billed)
             VALUES (?1, '', ?2, ?3, 1, ?4)",
            params![attempt_id, run_id, now_ms + lease_ms, reserve_steps],
        )
        .map_err(|e| {
            if matches!(&e, rusqlite::Error::SqliteFailure(ffi, _)
                if ffi.code == rusqlite::ErrorCode::ConstraintViolation)
            {
                Error::new(
                    ErrorKind::Conflict,
                    format!("attempt {attempt_id} already exists"),
                )
            } else {
                db_err(e)
            }
        })?;

        tx.commit().map_err(db_err)?;
        Ok(ClaimedRun {
            run_id: run_id.to_string(),
            attempt_id: attempt_id.to_string(),
            reserved_steps: reserve_steps,
            lease_expires_ms: now_ms + lease_ms,
        })
    }

    /// Finish a claimed run, if the caller still may speak for it.
    ///
    /// The epoch is checked here, in the same transaction that writes
    /// the terminal state: a worker that was mid-flight when the process
    /// restarted holds an epoch the store has moved past, and its result
    /// must not become the record of what happened.
    pub fn finish_claimed_run(
        &self,
        actor: &Actor,
        run_id: &str,
        attempt_id: &str,
        epoch: u64,
        final_state: RunState,
        now_ms: i64,
    ) -> Result<()> {
        actor.require(ActorRole::Operator, "completing a run")?;
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        let lease: Option<(i64, i64)> = tx
            .query_row(
                "SELECT lease_ms, active FROM attempts WHERE id = ?1",
                params![attempt_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let (lease_ms, active) = lease.ok_or_else(|| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no attempt {attempt_id}"),
            )
        })?;
        if active == 0 {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("attempt {attempt_id} is no longer active; it was reclaimed or completed"),
            ));
        }
        if lease_ms <= now_ms {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("attempt {attempt_id} lease expired at {lease_ms}"),
            ));
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT body FROM runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let body = current.ok_or_else(|| {
            Error::new(ErrorKind::ArtifactUnavailable, format!("no run {run_id}"))
        })?;
        let mut record: Run = serde_json::from_str(&body).map_err(|e| {
            Error::new(
                ErrorKind::IncompatibleState,
                format!("run {run_id} is unreadable: {e}"),
            )
        })?;
        crate::coordinator::check_epoch(&tx, epoch)?;
        crate::runner_machine::check_transition(record.state, final_state)?;
        record.state = final_state;
        tx.execute(
            "UPDATE runs SET state = ?1, body = ?2 WHERE id = ?3",
            params![
                state_str(final_state),
                String::from_utf8_lossy(&record.canonical_bytes()?),
                run_id
            ],
        )
        .map_err(db_err)?;
        // The attempt is spent before the state is visible as final, so a
        // replayed receipt finds a closed attempt rather than a live one.
        tx.execute(
            "UPDATE attempts SET active = 0 WHERE id = ?1",
            params![attempt_id],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(())
    }

    /// Claim an operation id, recording the answer it produced.
    ///
    /// `INSERT` and not `INSERT OR REPLACE`: an id that already exists
    /// is a replay, and the second caller must be *told* so rather than
    /// overwriting the first answer. A re-run whose result differs from
    /// what the id already stands for is a contradiction between two
    /// claims about the same operation, and the ledger says so.
    pub fn put_receipt(
        &self,
        actor: &Actor,
        operation_id: &str,
        status: u16,
        body: &str,
    ) -> Result<()> {
        actor.require(ActorRole::Operator, "recording an operation receipt")?;
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM receipts WHERE operation_id = ?1",
                params![operation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if let Some(previous) = existing {
            if previous == body {
                // The same answer twice is the same operation twice:
                // idempotent, and not an error.
                return Ok(());
            }
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "operation {operation_id} already produced a different answer; \
                     one id cannot mean two things"
                ),
            ));
        }
        self.conn
            .execute(
                "INSERT INTO receipts (operation_id, status, body, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![operation_id, status as i64, body, crate::api_time_ms()],
            )
            .map_err(|e| {
                if is_constraint(&e) {
                    // Lost a race with another writer between the read
                    // and the write. The other one owns the id now.
                    Error::new(
                        ErrorKind::Conflict,
                        format!("operation {operation_id} was claimed by another writer"),
                    )
                } else {
                    db_err(e)
                }
            })?;
        Ok(())
    }

    /// The answer an operation id produced, if it produced one.
    pub fn get_receipt(&self, operation_id: &str) -> Result<Option<(u16, String)>> {
        self.conn
            .query_row(
                "SELECT status, body FROM receipts WHERE operation_id = ?1",
                params![operation_id],
                |row| Ok((row.get::<_, i64>(0)? as u16, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)
    }

    /// Record what a queued run is for.
    ///
    /// Durable because the owner that runs it may be a different
    /// process from the one that queued it — including after a restart.
    /// Re-queueing the same run with different work is refused: a run id
    /// that meant two things is an ambiguity nobody can resolve later.
    pub fn put_queued_work(
        &self,
        actor: &Actor,
        run_id: &str,
        kind: &str,
        payload: &str,
    ) -> Result<()> {
        actor.require(ActorRole::Operator, "queuing work for a run")?;
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM queued_work WHERE run = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        if let Some(previous) = existing {
            if previous != payload {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    format!(
                        "run {run_id} is already queued for different work; one run id cannot mean two things"
                    ),
                ));
            }
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO queued_work (run, kind, payload, schema) VALUES (?1, ?2, ?3, ?4)",
                params![run_id, kind, payload, SCHEMA_VERSION],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// What a run is for, if anything was recorded.
    pub fn queued_work(&self, run_id: &str) -> Result<Option<(String, String)>> {
        self.conn
            .query_row(
                "SELECT kind, payload FROM queued_work WHERE run = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)
    }

    /// Record a cancel intent. A run cancelled while queued is never
    /// claimed; one cancelled while running is torn down by the owner
    /// that holds its attempt.
    pub fn request_cancel(&self, actor: &Actor, run_id: &str, now_ms: i64) -> Result<()> {
        actor.require(ActorRole::Operator, "cancelling a run")?;
        let current = self.get_run(run_id)?;
        if current.state.is_terminal() {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("run {run_id} is already {}", state_str(current.state)),
            ));
        }
        let mut next = current;
        next.state = RunState::Cancelled;
        self.conn
            .execute(
                "UPDATE runs SET state = 'cancelled', body = ?1 WHERE id = ?2",
                params![String::from_utf8_lossy(&next.canonical_bytes()?), run_id],
            )
            .map_err(db_err)?;
        let _ = now_ms;
        Ok(())
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
            .query_row(
                "SELECT body, schema FROM populations WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no population {id}"),
                )
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

    pub fn get_protocol(&self, id: &str) -> Result<crate::evaluation::EvaluationProtocol> {
        let (body, schema): (String, i64) = self
            .conn
            .query_row(
                "SELECT body, schema FROM protocols WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
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
    ///
    /// `pub(crate)` on purpose (G04). This is the lowest write pointer in
    /// the system, and the *only* thing it lacks is the human decision:
    /// the role check is not an approval. The public paths are
    /// `grove::logic::approve_and_publish` and the API handler, both of
    /// which arrive here through `logic::publish_approved` having checked
    /// a recorded approval. A caller that can reach this function
    /// directly can deploy without anyone having looked.
    pub(crate) fn publish(
        &self,
        actor: &Actor,
        snapshot: &ArtifactRef,
        expected_version: Option<u32>,
    ) -> Result<u32> {
        actor.require(ActorRole::Publisher, "publishing a model")?;
        // the atomic pointer flip is the last boundary a retracted snapshot
        // could slip through; it is checked here, not only upstream
        self.require_deployable(snapshot)?;
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
            params![
                format!("pub-v{version}"),
                snapshot.to_hex(),
                version,
                actor.id
            ],
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

    // ── G04: logic candidates and human approval ────────────────────

    /// Record a proposed candidate. `INSERT` and not `INSERT OR REPLACE`:
    /// a candidate is immutable once written, so a second proposal under
    /// the same id is a contradiction somebody must resolve, not a
    /// quiet overwrite of the first one's record.
    pub(crate) fn insert_candidate(
        &self,
        actor: &Actor,
        candidate: &crate::logic::LogicCandidate,
        digest: ArtifactRef,
    ) -> Result<()> {
        let body = candidate.canonical_bytes()?;
        // `INSERT OR IGNORE`, not `INSERT`: a plain insert *raises* on a
        // duplicate id, and the exception is not distinguishable from any
        // other constraint failure. Every column here is a non-optional
        // Rust field, so the primary key is the only constraint this can
        // hit — which makes a row count of zero an answer, not a guess.
        // (Same reasoning as the receipts table.)
        let inserted = self
            .conn
            .execute(
                "INSERT OR IGNORE INTO candidates (id, owner, state, source, schema, body, updated_at_ms)
                 VALUES (?1, ?2, 'proposed', ?3, ?4, ?5, ?6)",
                params![
                    candidate.id,
                    actor.id,
                    candidate.source_ref.to_hex(),
                    candidate.schema,
                    String::from_utf8_lossy(&body),
                    candidate.created_at_ms
                ],
            )
            .map_err(db_err)?;
        if inserted == 0 {
            // A candidate is immutable, so a second proposal under one id
            // is a claim that contradicts the first, not a revision.
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "candidate {} already exists; a candidate is immutable, so a changed \
                     proposal is a new candidate",
                    candidate.id
                ),
            ));
        }
        let _ = digest;
        Ok(())
    }

    pub fn get_candidate(&self, id: &str) -> Result<crate::logic::LogicCandidate> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM candidates WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let body = body.ok_or_else(|| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no logic candidate {id}"),
            )
        })?;
        decode(&body, SCHEMA_VERSION)
    }

    pub fn candidate_state(&self, id: &str) -> Result<crate::logic::CandidateState> {
        let state: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM candidates WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let state = state.ok_or_else(|| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no logic candidate {id}"),
            )
        })?;
        crate::logic::CandidateState::parse(&state)
    }

    /// Move a candidate's state. Candidates are immutable; the state is
    /// the one field that legitimately changes, and it only ever moves
    /// through this function.
    pub(crate) fn set_candidate_state(
        &self,
        actor: &Actor,
        id: &str,
        state: crate::logic::CandidateState,
        now_ms: i64,
    ) -> Result<()> {
        actor.require(ActorRole::Operator, "moving a candidate's state")?;
        let changed = self
            .conn
            .execute(
                "UPDATE candidates SET state = ?1, updated_at_ms = ?2 WHERE id = ?3",
                params![state.as_str(), now_ms, id],
            )
            .map_err(db_err)?;
        if changed == 0 {
            return Err(Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("no logic candidate {id}"),
            ));
        }
        Ok(())
    }

    /// The candidate id behind the active pointer, when the pointer
    /// names one. `None` for a model publication, which is not a logic
    /// candidate — and is exactly why the answer is an `Option`.
    pub fn active_candidate_id(&self) -> Result<Option<String>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM candidates
                 WHERE state = 'active' AND source = (
                   SELECT snapshot FROM publications WHERE state = 'active' LIMIT 1)
                 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(id)
    }

    /// Write the human decision, and bind it to the evaluations it rests
    /// on.
    ///
    /// One transaction, because an approval row whose evidence is
    /// missing is the exact shape of "somebody clicked approve" — the
    /// approval and what justified it land together or not at all.
    pub(crate) fn put_approval(
        &self,
        actor: &Actor,
        approval: &crate::logic::HumanApproval,
    ) -> Result<()> {
        actor.require(ActorRole::Publisher, "recording an approval")?;
        let body = approval.canonical_bytes()?;
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        // `INSERT OR IGNORE` so a repeated id is a row count of zero and
        // therefore a *decision* the caller can be told about, rather
        // than an exception indistinguishable from any other constraint
        // failure. The same artifact approved twice at the same version
        // produces the same id, and that is a genuine conflict.
        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO approvals
                   (id, subject, candidate, actor, expected_version, decision, schema, body, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    approval.id,
                    approval.subject_id,
                    approval.candidate_ref.to_hex(),
                    approval.authenticated_actor,
                    approval.expected_publication_version,
                    approval.decision,
                    approval.schema,
                    String::from_utf8_lossy(&body),
                    approval.created_at_ms
                ],
            )
            .map_err(db_err)?;
        if inserted == 0 {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "approval {} already exists; a second decision about the same \
                     candidate is a different approval, not an update",
                    approval.id
                ),
            ));
        }
        for evaluation in &approval.evaluation_refs {
            tx.execute(
                "INSERT INTO approval_evaluations (approval, candidate) VALUES (?1, ?2)",
                params![approval.id, evaluation.to_hex()],
            )
            .map_err(db_err)?;
        }
        tx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn get_approval(&self, id: &str) -> Result<crate::logic::HumanApproval> {
        let body: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM approvals WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let body = body.ok_or_else(|| {
            Error::new(ErrorKind::ArtifactUnavailable, format!("no approval {id}"))
        })?;
        decode(&body, SCHEMA_VERSION)
    }

    /// Is this approval still one that may be applied?
    ///
    /// Four checks, and each of them is a way a recorded approval can be
    /// true while being wrong now:
    ///
    /// * it names at least one evaluation, and every evaluation it names
    ///   is a real `EvaluationRecord` whose bytes still hash to that
    ///   digest — an approval resting on a record that was never
    ///   written rests on nothing;
    /// * those records are for the artifact being published, so
    ///   approving one candidate's numbers does not approve another's
    ///   bytes;
    /// * the recorded subject is the subject being published;
    /// * if it committed to a publication version, the pointer is still
    ///   there.
    ///
    /// The version check is a *separate* function on purpose: a stale
    /// pointer is a lost race between two publishers, which is a
    /// `Conflict`, and folding it into this boolean would report a
    /// concurrency loss as a malformed approval.
    pub(crate) fn approval_is_current(
        &self,
        approval: &crate::logic::HumanApproval,
    ) -> Result<bool> {
        if approval.decision != "approved" {
            return Ok(false);
        }
        let named = self.approval_evaluation_refs(&approval.id)?;
        if named.is_empty() {
            return Ok(false);
        }
        // Re-derive each named digest from the record's stored bytes.
        // The digest is over `canonical_json(record)`, which is exactly
        // what the `body` column holds, so this is a real check rather
        // than a match against a string the row happens to contain.
        let mut stmt = self
            .conn
            .prepare("SELECT body FROM evaluations WHERE snapshot = ?1")
            .map_err(db_err)?;
        let bodies = stmt
            .query_map(params![approval.candidate_ref.to_hex()], |row| {
                row.get::<_, String>(0)
            })
            .map_err(db_err)?;
        let mut present = Vec::new();
        for body in bodies {
            let body = body.map_err(db_err)?;
            present.push(crate::contracts::digest_bytes(body.as_bytes()));
        }
        if !named.iter().all(|e| present.contains(e)) {
            return Ok(false);
        }
        let subject: Option<String> = self
            .conn
            .query_row(
                "SELECT subject FROM approvals WHERE id = ?1",
                params![approval.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        Ok(subject.as_deref() == Some(approval.subject_id.as_str()))
    }

    /// The version race, as its own check. `None` on either side means
    /// "no version was claimed", and then there is nothing to race
    /// against.
    pub(crate) fn approval_version_is_current(
        &self,
        approval: &crate::logic::HumanApproval,
    ) -> Result<bool> {
        let (Some(expected), Some((current, _))) = (
            approval.expected_publication_version,
            self.active_publication()?,
        ) else {
            return Ok(true);
        };
        Ok(expected == current)
    }

    /// The evaluation digests an approval names.
    pub(crate) fn approval_evaluation_refs(&self, approval_id: &str) -> Result<Vec<ArtifactRef>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT candidate FROM approval_evaluations WHERE approval = ?1 ORDER BY candidate",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![approval_id], |row| row.get::<_, String>(0))
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(ArtifactRef::parse_hex(&row.map_err(db_err)?)?);
        }
        Ok(out)
    }

    /// Record a human refusal, keeping the reason. A second refusal is
    /// refused rather than replacing the first: two people declining the
    /// same candidate with different reasons is two facts, and the store
    /// keeps the first by refusing the second rather than pretending it
    /// never happened.
    pub(crate) fn note_rejection(
        &self,
        actor: &Actor,
        subject_id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<()> {
        actor.require(ActorRole::Publisher, "declining a deployment change")?;
        self.conn
            .execute(
                "INSERT INTO rejections (subject, actor, reason, at_ms) VALUES (?1, ?2, ?3, ?4)",
                params![subject_id, actor.id, reason, now_ms],
            )
            .map_err(db_err)?;
        Ok(())
    }

    pub fn rejection(&self, subject_id: &str) -> Result<Option<(String, String, i64)>> {
        let row: Option<(String, String, i64)> = self
            .conn
            .query_row(
                "SELECT actor, reason, at_ms FROM rejections WHERE subject = ?1",
                params![subject_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_err)?;
        Ok(row)
    }

    // ── W12: retraction propagation ───────────────────────────────

    /// Retract a signal and propagate the consequence through the whole
    /// lineage it poisoned. The frozen revisions keep their members — the
    /// historical fact is unchanged — but every snapshot whose training
    /// consumed the signal is marked non-deployable, and that marking is
    /// what `publish_candidate` and `resume_plan` read. Returns exactly
    /// what was invalidated so the caller can show it, not infer it.
    pub fn retract_signal(&self, actor: &Actor, signal_id: &str) -> Result<RetractionPropagation> {
        self.withdraw_signal(actor, signal_id)?;
        self.propagate_retraction(signal_id)
    }

    /// The invalidation record for a snapshot, if any: which signal was
    /// revoked and through which frozen view. `None` means the snapshot's
    /// data is all still in force.
    pub fn snapshot_invalidation(&self, snapshot: &ArtifactRef) -> Result<Option<Invalidation>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT signal, dataset FROM invalidations WHERE snapshot = ?1
                 ORDER BY signal, dataset",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![snapshot.to_hex()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        let mut signals = Vec::new();
        let mut datasets = Vec::new();
        for row in rows {
            let (signal, dataset) = row.map_err(db_err)?;
            signals.push(signal);
            datasets.push(dataset);
        }
        if signals.is_empty() {
            return Ok(None);
        }
        Ok(Some(Invalidation {
            snapshot: *snapshot,
            revoked_signals: signals,
            dataset_revisions: datasets,
            // weights already trained on withdrawn data are not made whole
            // by un-retracting the record; only retraining can restore it
            recoverable: false,
        }))
    }

    /// Refuse to deploy an invalidated snapshot. This is the checkpoint the
    /// publication path calls before it looks at gates.
    pub fn require_deployable(&self, snapshot: &ArtifactRef) -> Result<()> {
        if let Some(invalidation) = self.snapshot_invalidation(snapshot)? {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!(
                    "snapshot {snapshot} was trained on retracted signal(s) {:?} \
                     (frozen view(s) {:?}); retrain without them — deployment of these \
                     weights is refused",
                    invalidation.revoked_signals, invalidation.dataset_revisions
                ),
            ));
        }
        Ok(())
    }

    /// Refuse to continue an invalidated lineage. The same reasoning as
    /// deployment: the data the run learned from is withdrawn.
    pub fn require_resumable(&self, snapshot: &ArtifactRef) -> Result<()> {
        self.require_deployable(snapshot)
            .map_err(|e| Error::new(e.kind, format!("lineage cannot be resumed: {}", e.context)))
    }

    /// Mark the signal retracted and record, for every affected snapshot,
    /// the revoked signal and the frozen view that carried it. Does the
    /// reverse walk: signal → datasets → runs → checkpoints → snapshots.
    fn propagate_retraction(&self, signal_id: &str) -> Result<RetractionPropagation> {
        // every frozen revision that froze this signal
        let mut datasets = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT id, body FROM datasets ORDER BY id")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(db_err)?;
            for row in rows {
                let (id, body) = row.map_err(db_err)?;
                let revision: DatasetRevision = decode(&body, SCHEMA_VERSION)?;
                if revision.signal_ids.iter().any(|s| s == signal_id) {
                    datasets.push((id, revision));
                }
            }
        }

        // every run whose dataset revision is one of them
        let dataset_ids: Vec<String> = datasets.iter().map(|(id, _)| id.clone()).collect();
        let mut runs: Vec<Run> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT body, schema FROM runs ORDER BY id")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(db_err)?;
            for row in rows {
                let (body, schema) = row.map_err(db_err)?;
                let run: Run = decode(&body, require_schema(schema)?)?;
                if dataset_ids.contains(&run.dataset_revision) {
                    runs.push(run);
                }
            }
        }
        let run_ids: Vec<String> = runs.iter().map(|r| r.id.clone()).collect();

        // every checkpoint of those runs, and the snapshots they carry
        let mut checkpoints: Vec<Checkpoint> = Vec::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT body, schema FROM checkpoints ORDER BY id")
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(db_err)?;
            for row in rows {
                let (body, schema) = row.map_err(db_err)?;
                let ckpt: Checkpoint = decode(&body, require_schema(schema)?)?;
                if run_ids.contains(&ckpt.run_id) {
                    checkpoints.push(ckpt);
                }
            }
        }

        // A run trains *from* a base snapshot; every checkpoint of a tainted
        // run carries that base. The base is what was deployed, so it is
        // what must stop being deployable.
        let mut snapshots: Vec<ArtifactRef> = runs.iter().map(|r| r.base_snapshot).collect();
        for ckpt in &checkpoints {
            snapshots.push(ckpt.snapshot);
        }
        snapshots.sort();
        snapshots.dedup();

        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        for snapshot in &snapshots {
            for (dataset, _) in &datasets {
                tx.execute(
                    "INSERT OR IGNORE INTO invalidations (snapshot, signal, dataset)
                     VALUES (?1, ?2, ?3)",
                    params![snapshot.to_hex(), signal_id, dataset],
                )
                .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)?;

        Ok(RetractionPropagation {
            signal_id: signal_id.to_string(),
            dataset_revisions: datasets.into_iter().map(|(id, _)| id).collect(),
            invalidated_runs: run_ids,
            invalidated_checkpoints: checkpoints.into_iter().map(|c| c.id).collect(),
            invalidated_snapshots: snapshots,
        })
    }

    // ── W12: reachability and retention ────────────────────────────

    /// The declared retention horizon in milliseconds, or `None` when the
    /// operator has declared none — and an undeclared policy collects
    /// nothing.
    pub fn retention_horizon_ms(&self) -> Result<Option<u64>> {
        match self.meta("retention.horizon_ms")? {
            None => Ok(None),
            Some(v) => v.parse().map(Some).map_err(|_| {
                Error::new(
                    ErrorKind::IncompatibleState,
                    format!("retention.horizon_ms is not a number: {v}"),
                )
            }),
        }
    }

    /// Declare (or clear, with `None`) the retention horizon: how old an
    /// unreachable artifact must be before cleanup may delete it. A real
    /// policy value the operator sets, not a constant pretending to be one.
    pub fn set_retention_horizon_ms(&self, horizon_ms: Option<u64>) -> Result<()> {
        match horizon_ms {
            Some(ms) => self.set_meta("retention.horizon_ms", &ms.to_string()),
            None => self
                .conn
                .execute("DELETE FROM meta WHERE key = 'retention.horizon_ms'", [])
                .map_err(db_err)
                .map(|_| ()),
        }
    }

    /// Read a `meta` key. Used for the retention policy and the coordinator
    /// ownership record.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)
    }

    pub(crate) fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![key, value],
            )
            .map_err(db_err)
            .map(|_| ())
    }

    /// The set of artifacts reachable from a root that must survive
    /// cleanup. Walks every `ArtifactRef` each record actually holds:
    ///
    /// * the active publication's snapshot, and each snapshot's parameters;
    /// * each branch's head checkpoint, its whole parent chain, and each
    ///   of those checkpoints' state artifacts and snapshots;
    /// * every run that is not terminal, and its base snapshot;
    /// * every frozen dataset revision's observations and their content
    ///   blocks;
    /// * every in-force signal's observation and its content blocks.
    ///
    /// Anything not in this set is a candidate for collection.
    pub fn live_artifacts(&self) -> Result<BTreeSet<ArtifactRef>> {
        let mut live: BTreeSet<ArtifactRef> = BTreeSet::new();

        // the active publication is a root
        if let Some((_version, snapshot)) = self.active_publication()? {
            self.mark_snapshot(&mut live, &snapshot)?;
        }

        // every branch head: the checkpoint and its whole ancestry
        for branch in self.branches() {
            let mut cursor = branch.head.clone();
            while let Some(checkpoint_id) = cursor {
                let Ok(ckpt) = self.get_checkpoint(&checkpoint_id) else {
                    break; // a head pointing at reclaimed history adds nothing
                };
                live.insert(ckpt.state_artifact);
                self.mark_snapshot(&mut live, &ckpt.snapshot)?;
                cursor = ckpt.parent.clone();
            }
        }

        // non-terminal runs still owe their base snapshot
        for run in self.runs() {
            if !run.state.is_terminal() {
                self.mark_snapshot(&mut live, &run.base_snapshot)?;
            }
        }

        // frozen views and in-force signals keep their evidence readable:
        // the observation records live in their own tables, so the bytes
        // they protect are the content blocks
        for observation in self
            .revision_observations()?
            .into_iter()
            .chain(self.signal_roots()?)
        {
            for block in observation.blocks {
                live.insert(block.artifact);
            }
        }

        Ok(live)
    }

    /// A live snapshot keeps its own manifest bytes and the parameters that
    /// manifest names. A manifest that cannot be read is a damaged record,
    /// not a reason to pretend its parameters are garbage.
    fn mark_snapshot(&self, live: &mut BTreeSet<ArtifactRef>, digest: &ArtifactRef) -> Result<()> {
        if !live.insert(*digest) {
            return Ok(());
        }
        if let Ok(snapshot) = self.load_snapshot(digest) {
            for param in snapshot.params {
                live.insert(param.artifact);
            }
        }
        Ok(())
    }

    /// The observations a frozen dataset revision names. A frozen view must
    /// stay reproducible, so its evidence is a root; an observation a user
    /// already deleted is simply not walked.
    fn revision_observations(&self) -> Result<Vec<Observation>> {
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare("SELECT body, schema FROM datasets ORDER BY id")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(db_err)?;
        for row in rows {
            let (body, schema) = row.map_err(db_err)?;
            let revision: DatasetRevision = decode(&body, require_schema(schema)?)?;
            for observation_id in &revision.observation_ids {
                if let Ok(observation) = self.get_observation(observation_id) {
                    out.push(observation);
                }
            }
        }
        Ok(out)
    }

    /// In-force signals keep their referenced evidence reachable.
    fn signal_roots(&self) -> Result<Vec<Observation>> {
        let mut out = Vec::new();
        let mut stmt = self
            .conn
            .prepare("SELECT body, schema FROM signals WHERE status = 'accepted' ORDER BY id")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(db_err)?;
        for row in rows {
            let (body, schema) = row.map_err(db_err)?;
            let signal: LearningSignal = decode(&body, require_schema(schema)?)?;
            if let Some(observation_id) = signal.observation_id {
                if let Ok(observation) = self.get_observation(&observation_id) {
                    out.push(observation);
                }
            }
        }
        Ok(out)
    }

    /// Run one cleanup pass. Deletes only artifacts that are (a) not in the
    /// live set and (b) older than the declared retention horizon. With no
    /// declared horizon, nothing is deleted — an undeclared policy is not a
    /// license to destroy history. The report names every deletion and every
    /// refusal, so the pass is auditable.
    pub fn collect_artifacts(&self) -> Result<CleanupReport> {
        let Some(horizon_ms) = self.retention_horizon_ms()? else {
            // no declared policy: collect nothing
            return Ok(CleanupReport::default());
        };
        let live = self.live_artifacts()?;
        let now_ms = epoch_millis();
        let objects = self.artifacts.objects_with_age()?;
        let mut report = CleanupReport {
            horizon_ms,
            ..CleanupReport::default()
        };
        for (digest, mtime) in objects {
            let age_ms = now_ms.saturating_sub(epoch_millis_of(mtime));
            if live.contains(&digest) {
                // reachable: kept regardless of age, and named
                if age_ms > horizon_ms as i64 {
                    report.refused.push(digest);
                }
                continue;
            }
            if age_ms <= horizon_ms as i64 {
                report.retained_young.push(digest);
                continue;
            }
            if self.artifacts.remove(&digest)? {
                report.deleted.push(digest);
            }
        }
        report.deleted.sort();
        report.refused.sort();
        report.retained_young.sort();
        Ok(report)
    }

    // ── helpers ────────────────────────────────────────────────────
}

pub(crate) fn db_err(error: rusqlite::Error) -> Error {
    Error::new(ErrorKind::BackendFailed, format!("sqlite: {error}"))
}

/// A uniqueness violation, which for a claim means "someone else got
/// there first" rather than "the database is broken".
fn is_constraint(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(ffi, _)
        if ffi.code == rusqlite::ErrorCode::ConstraintViolation)
}

/// Wall-clock milliseconds. Used by the retention horizon, which is a real
/// policy about real files, not a synthetic step counter.
pub(crate) fn epoch_millis() -> i64 {
    epoch_millis_of(SystemTime::now())
}

pub(crate) fn epoch_millis_of(when: SystemTime) -> i64 {
    when.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
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
    serde_json::from_str(body).map_err(|e| {
        Error::new(
            ErrorKind::IncompatibleState,
            format!("record is unreadable: {e}"),
        )
    })
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
