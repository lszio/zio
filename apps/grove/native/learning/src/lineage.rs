//! Multi-parent lineage edges (W13/W16).
//!
//! A composition or a distillation has more than one parent, so the
//! parent list cannot live in a column. It is a table: one row per
//! (child, parent, derivation). This lives in its own module rather than
//! inside `store.rs` so the schema and the two call sites that use it
//! are one readable unit.
//!
//! The table is append-only. A snapshot's parents are part of its
//! identity: re-deriving is a new snapshot id, never an edit of an old
//! one.

use rusqlite::{OptionalExtension, params};

use crate::contracts::{Actor, ArtifactRef, Derivation, Error, ErrorKind, Result};
use crate::store::Store;

const LINEAGE_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lineage (
    child       TEXT NOT NULL,
    parent      TEXT NOT NULL,
    derivation  TEXT NOT NULL,
    at_ms       INTEGER NOT NULL,
    PRIMARY KEY (child, parent, derivation)
);

-- a named snapshot id (composite-1, distill-2, …) points at the digest it
-- resolved to. The name is the human handle; the digest is the identity.
CREATE TABLE IF NOT EXISTS named_snapshots (
    name   TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    at_ms  INTEGER NOT NULL
);
"#;

impl Store {
    /// Ensure the lineage tables exist. Called from the first write path
    /// so a store created by an older binary gains them on use.
    pub fn ensure_lineage(&self) -> Result<()> {
        self.conn
            .execute_batch(LINEAGE_SCHEMA)
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("init lineage: {e}")))
    }

    /// Record that `child_id` was derived from every one of `parents`.
    /// Re-inserting the same edge is a no-op, so a retried composition
    /// does not invent a second derivation.
    pub fn record_derivation(
        &self,
        _actor: &Actor,
        child_id: &str,
        parents: Vec<ArtifactRef>,
        derivation: Derivation,
    ) -> Result<()> {
        // The edge is already permission-checked by the caller that owns
        // the module contract; recording it is bookkeeping, not a grant.
        self.ensure_lineage()?;
        for parent in &parents {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO lineage (child, parent, derivation, at_ms)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        child_id,
                        parent.to_hex(),
                        format!("{derivation:?}").to_lowercase(),
                        crate::api_time_ms()
                    ],
                )
                .map_err(|e| {
                    Error::new(
                        ErrorKind::BackendFailed,
                        format!("record derivation for {child_id}: {e}"),
                    )
                })?;
        }
        Ok(())
    }

    /// The parents of a child, with the edge that explains each. A child
    /// with no recorded edge (an initial seed) returns an empty list —
    /// that is history's start, not a missing row.
    pub fn parents_of(&self, child_id: &str) -> Result<Vec<(ArtifactRef, Derivation)>> {
        self.ensure_lineage()?;
        let mut stmt = self
            .conn
            .prepare("SELECT parent, derivation FROM lineage WHERE child = ?1 ORDER BY parent")
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("read lineage: {e}")))?;
        let rows = stmt
            .query_map(params![child_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("read lineage: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let (parent, derivation) = row
                .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("read lineage: {e}")))?;
            out.push((
                ArtifactRef::parse_hex(&parent)?,
                parse_derivation(&derivation)?,
            ));
        }
        Ok(out)
    }

    /// Bind a human-readable name to a committed digest. The name is a
    /// handle only: the digest remains the identity, so two names for
    /// one digest are the same model and a name never overrides history.
    pub fn name_snapshot(&self, name: &str, digest: &ArtifactRef) -> Result<()> {
        self.ensure_lineage()?;
        // refuse to point a name at a digest that is not committed
        self.load_manifest(digest)?;
        self.conn
            .execute(
                "INSERT INTO named_snapshots (name, digest, at_ms) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET digest = ?2, at_ms = ?3",
                params![name, digest.to_hex(), crate::api_time_ms()],
            )
            .map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("name snapshot {name}: {e}"),
                )
            })?;
        Ok(())
    }

    /// Resolve a name to its digest. An unbound name is
    /// `artifact-unavailable`, not a fresh empty model: composing against
    /// a name that never existed would silently produce something else.
    pub fn snapshot_digest(&self, name: &str) -> Result<ArtifactRef> {
        self.ensure_lineage()?;
        let hex: String = self
            .conn
            .query_row(
                "SELECT digest FROM named_snapshots WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("resolve {name}: {e}")))?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no snapshot named {name:?} in this store"),
                )
            })?;
        ArtifactRef::parse_hex(&hex)
    }

    /// Every named snapshot, newest first. This is what the product's
    /// lineage view renders.
    pub fn named_snapshots(&self) -> Result<Vec<(String, ArtifactRef, i64)>> {
        self.ensure_lineage()?;
        let mut stmt = self
            .conn
            .prepare("SELECT name, digest, at_ms FROM named_snapshots ORDER BY at_ms DESC, name")
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("list snapshots: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("list snapshots: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let (name, hex, at_ms) = row.map_err(|e| {
                Error::new(ErrorKind::BackendFailed, format!("list snapshots: {e}"))
            })?;
            out.push((name, ArtifactRef::parse_hex(&hex)?, at_ms));
        }
        Ok(out)
    }
}

fn parse_derivation(text: &str) -> Result<Derivation> {
    Ok(match text {
        "parametertraining" | "parameter-training" => Derivation::ParameterTraining,
        "coderewrite" | "code-rewrite" => Derivation::CodeRewrite,
        "weightmigration" | "weight-migration" => Derivation::WeightMigration,
        "modulecomposition" | "module-composition" => Derivation::ModuleComposition,
        "distillation" => Derivation::Distillation,
        other => {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!("unknown derivation edge {other:?}"),
            ));
        }
    })
}
