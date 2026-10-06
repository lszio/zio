//! Product read model (W11).
//!
//! The product shell needs three reads the store did not expose as
//! query-shaped values:
//!
//! * every prediction, newest first — a prediction is bound to the exact
//!   snapshot that produced it, and the UI must show that binding;
//! * whether one signal is still in force — "submitted" and "still in
//!   force" are different facts and the product shows both;
//! * which frozen dataset revisions already contain a signal — this is
//!   what separates "the model has learned from your correction" from
//!   "your correction is filed and still counts".
//!
//! These are additive reads. They change no existing signature and no
//! write path.

use crate::contracts::{Prediction, Result};
use crate::store::Store;

impl Store {
    /// Every prediction, newest first. Ordered by creation time then id
    /// so a replay of an old batch does not reshuffle the product's
    /// default view.
    pub fn query_predictions(&self) -> Result<Vec<Prediction>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT body, schema FROM predictions
                 ORDER BY json_extract(body, '$.created_at_ms') DESC, id DESC",
            )
            .map_err(crate::store::db_err)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(crate::store::db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (body, _schema) = row.map_err(crate::store::db_err)?;
            out.push(serde_json::from_str(&body).map_err(|e| {
                crate::contracts::Error::new(
                    crate::contracts::ErrorKind::IncompatibleState,
                    format!("prediction record is unreadable: {e}"),
                )
            })?);
        }
        Ok(out)
    }

    /// Is this signal id still in force? After a retraction or a
    /// supersession it is not, and the product shows exactly that.
    pub fn signal_in_force(&self, id: &str) -> Result<bool> {
        let status: Option<String> = self
            .conn
            .query_row("SELECT status FROM signals WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .ok();
        Ok(matches!(status.as_deref(), Some("accepted")))
    }

    /// The frozen dataset revisions that already contain this signal.
    /// An empty list means the correction is filed but nothing has
    /// trained on it yet — the two states the product must not merge.
    pub fn datasets_containing_signal(&self, signal_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id FROM datasets
                 WHERE instr(body, ?1) > 0
                 ORDER BY id",
            )
            .map_err(crate::store::db_err)?;
        let needle = format!("\"{signal_id}\"");
        let rows = stmt
            .query_map([needle], |row| row.get::<_, String>(0))
            .map_err(crate::store::db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(crate::store::db_err)?);
        }
        Ok(out)
    }

    /// The recorded human decisions, newest first. A review board that
    /// cannot see *who approved what* has no audit, so this is the
    /// product's own read rather than a query the shell assembles.
    pub fn approval_log(&self) -> Result<Vec<serde_json::Value>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT body FROM approvals ORDER BY created_at_ms DESC, id",
            )
            .map_err(crate::store::db_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(crate::store::db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let body = row.map_err(crate::store::db_err)?;
            out.push(serde_json::from_str(&body).map_err(|e| {
                crate::contracts::Error::new(
                    crate::contracts::ErrorKind::IncompatibleState,
                    format!("approval record is unreadable: {e}"),
                )
            })?);
        }
        Ok(out)
    }
}
