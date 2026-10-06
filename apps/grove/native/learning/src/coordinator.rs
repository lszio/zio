//! W09: the population coordinator.
//!
//! One trusted coordinator owns the population's ledger, branch heads and
//! execution slots. Workers commit results against an *attempt*: a
//! short-lived lease that speaks for one branch of one run. The contracts
//! this module enforces are the reason a population is safe to run in
//! parallel:
//!
//! * **A stale worker cannot overwrite new progress.** Commits carry the
//!   attempt id and the branch head's expected version; an expired lease
//!   or a superseded attempt is rejected with `conflict`, and the branch
//!   head stays at whatever newer state it reached.
//! * **A failure is not contagious.** Workers are independent processes;
//!   a branch that dies leaves the others' slots, leases and ledger
//!   untouched.
//! * **Billing is idempotent.** Debits are keyed by message id; a
//!   replayed receipt bills once.
//! * **Unknown outcomes stay visible.** An external call whose result is
//!   lost is recorded as unknown — never silently assumed to have
//!   happened exactly once.

use std::sync::Arc;

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::contracts::{
    Actor, ActorRole, Branch, Error, ErrorKind, Population, Result, SCHEMA_VERSION,
};
use crate::store::Store;

/// An execution slot: one worker process speaking for one branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    pub schema: u32,
    pub id: String,
    pub branch: String,
    pub run: String,
    /// Millis timestamp after which the lease is expired. An expired
    /// attempt cannot commit — a zombie worker must not write history.
    pub lease_expires_ms: i64,
    /// While true the attempt may still speak for its branch; committing
    /// or cancelling turns this off.
    pub active: bool,
    pub billed_steps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Billing {
    /// The debit was applied.
    Charged,
    /// The message id was already billed; the ledger did not move.
    Duplicate,
}

/// How an attempt's terminal report is known.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// The attempt finished and its result was committed.
    Committed,
    /// The attempt died; its lease is dead and the slot is free.
    Failed(String),
    /// An external call's result was never learned. Recorded as unknown —
    /// the ledger shows the debit, the outcome stays unresolved.
    Unknown(String),
}

/// Who owns scheduling right now. Ownership is a database fact, not a
/// property of a live process: after a crash the old owner is gone, and the
/// next opener must be able to prove the receipts it inherited are stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    /// Monotonic; every claim is one more than the last.
    pub epoch: u64,
    pub pid: i64,
}

/// The ownership row as it is persisted. Kept separate from the public
/// [`Ownership`] so the stored shape can change without changing callers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct OwnershipRecord {
    epoch: u64,
    pid: i64,
}

pub struct Coordinator {
    store: Arc<Store>,
}

impl Coordinator {
    pub fn new(store: Arc<Store>) -> Self {
        let coordinator = Self { store };
        // Taking ownership is the first thing a process does with a store:
        // leaving the previous owner's epoch in place would let a zombie
        // worker's receipt land after a restart.
        let _ = coordinator.claim_ownership();
        coordinator
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Take coordinator ownership, invalidating every receipt stamped with
    /// an earlier epoch. A claim is always a new epoch — including from the
    /// same pid, because a restart is a new coordinator even when the
    /// operating system reused the process id. Everything the previous
    /// owner held is reclaimed with it.
    pub fn claim_ownership(&self) -> Result<Ownership> {
        let previous = self
            .store
            .meta("coordinator.owner")?
            .and_then(|raw| serde_json::from_str::<OwnershipRecord>(&raw).ok());
        let pid = std::process::id() as i64;
        let next = Ownership {
            epoch: previous.map_or(1, |p| p.epoch + 1),
            pid,
        };
        // A receipt from before this claim cannot be honoured, so every
        // lease the previous owner held is released here.
        self.store.reclaim_leases()?;
        let record = OwnershipRecord { epoch: next.epoch, pid };
        self.store
            .set_meta("coordinator.owner", &serde_json::to_string(&record).map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("serialize ownership record: {e}"),
                )
            })?)
            .map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("record coordinator ownership: {}", e.context),
                )
            })?;
        Ok(next)
    }

    /// The current owner, or `None` when no process has claimed the store.
    pub fn ownership(&self) -> Result<Option<Ownership>> {
        Ok(self
            .store
            .meta("coordinator.owner")?
            .and_then(|raw| serde_json::from_str::<OwnershipRecord>(&raw).ok())
            .map(|r| Ownership {
                epoch: r.epoch,
                pid: r.pid,
            }))
    }

    /// Attempts that still hold a live lease at `now_ms`. A reclaimed or
    /// expired lease is not a slot anybody may finish.
    pub fn active_attempts(&self, now_ms: i64) -> Result<Vec<String>> {
        let mut stmt = self
            .store
            .conn
            .prepare("SELECT id FROM attempts WHERE active = 1 AND lease_ms > ?1 ORDER BY id")
            .map_err(crate::store::db_err)?;
        let rows = stmt
            .query_map(rusqlite::params![now_ms], |row| row.get::<_, String>(0))
            .map_err(crate::store::db_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(crate::store::db_err)?);
        }
        Ok(out)
    }

    /// Open an execution slot: the attempt takes a lease on the branch and
    /// the population's ledger reserves the steps. Starting new work with
    /// an exhausted ledger is refused — pausing running work is the safe
    /// response, not expanding the grant.
    pub fn start_attempt(
        &self,
        actor: &Actor,
        population_id: &str,
        branch_id: &str,
        run_id: &str,
        steps: u32,
        lease_ms: i64,
        now_ms: i64,
        attempt_id: &str,
    ) -> Result<Attempt> {
        actor.require(ActorRole::Operator, "starting an attempt")?;
        let population = self.store.get_population(population_id)?;

        // the branch must belong to the population
        if !population.members.iter().any(|m| m == branch_id) {
            return Err(Error::new(
                ErrorKind::IncompatibleState,
                format!("branch {branch_id} is not a member of population {population_id}"),
            ));
        }
        // a fork does not amplify the grant: the whole population spends
        // one ledger, whatever its members are
        self.store.spend_population(&population, steps)?;

        let attempt = Attempt {
            schema: SCHEMA_VERSION,
            id: attempt_id.to_string(),
            branch: branch_id.to_string(),
            run: run_id.to_string(),
            lease_expires_ms: now_ms + lease_ms,
            active: true,
            billed_steps: steps,
        };
        self.store
            .conn
            .execute(
                "INSERT INTO attempts (id, branch, run, lease_ms, active, billed)
                 VALUES (?1, ?2, ?3, ?4, 1, ?5)",
                rusqlite::params![
                    attempt.id,
                    attempt.branch,
                    attempt.run,
                    attempt.lease_expires_ms,
                    attempt.billed_steps
                ],
            )
            .map_err(|e| {
                if is_unique_violation(&e) {
                    Error::new(
                        ErrorKind::Conflict,
                        format!("attempt {attempt_id} already exists"),
                    )
                } else {
                    crate::store::db_err(e)
                }
            })?;
        Ok(attempt)
    }

    /// Extend a lease for an attempt that is still working.
    pub fn renew_lease(&self, actor: &Actor, attempt_id: &str, lease_ms: i64, now_ms: i64) -> Result<i64> {
        actor.require(ActorRole::Operator, "renewing a lease")?;
        let attempt = self.get_attempt(attempt_id)?;
        if attempt.lease_expires_ms <= now_ms {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("attempt {attempt_id} lease expired at {}; renewals cannot revive it", attempt.lease_expires_ms),
            ));
        }
        let next = now_ms + lease_ms;
        self.store
            .conn
            .execute(
                "UPDATE attempts SET lease_ms = ?1 WHERE id = ?2",
                rusqlite::params![next, attempt_id],
            )
            .map_err(crate::store::db_err)?;
        Ok(next)
    }

    /// Commit an attempt's result as the branch's new head. Rejected when
    /// the lease expired (a zombie worker) or when the branch head moved
    /// past the version the worker saw (a stale writer). Either way the
    /// branch keeps its newer state.
    pub fn commit_attempt(
        &self,
        actor: &Actor,
        attempt_id: &str,
        new_head: &str,
        expected_head_version: u32,
        now_ms: i64,
    ) -> Result<u32> {
        let epoch = self.ownership()?.map(|o| o.epoch).unwrap_or(0);
        self.commit_attempt_stamped(actor, attempt_id, new_head, expected_head_version, now_ms, epoch)
    }

    /// Commit an attempt's result, refusing one stamped with a coordinator
    /// epoch that has since been superseded. A worker that was mid-flight
    /// when the process restarted must not land its result: the branch may
    /// have moved on under a new owner, and the old owner's accounting is
    /// no longer the ledger of record.
    pub fn commit_attempt_stamped(
        &self,
        actor: &Actor,
        attempt_id: &str,
        new_head: &str,
        expected_head_version: u32,
        now_ms: i64,
        epoch: u64,
    ) -> Result<u32> {
        actor.require(ActorRole::Operator, "committing an attempt")?;
        let current = self.ownership()?.map(|o| o.epoch).unwrap_or(0);
        if epoch != current {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!(
                    "attempt {attempt_id} was stamped with ownership epoch {epoch}, \
                     the store is now at epoch {current}; a superseded coordinator's \
                     receipt is not a result"
                ),
            ));
        }
        let attempt = self.get_attempt(attempt_id)?;
        if !attempt.active {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("attempt {attempt_id} is no longer active"),
            ));
        }
        if attempt.lease_expires_ms <= now_ms {
            return Err(Error::new(
                ErrorKind::Conflict,
                format!("attempt {attempt_id} lease expired; its result must not overwrite newer progress"),
            ));
        }
        // mark the attempt spent BEFORE the head move: one attempt commits once
        self.store
            .conn
            .execute(
                "UPDATE attempts SET active = 0 WHERE id = ?1",
                rusqlite::params![attempt_id],
            )
            .map_err(crate::store::db_err)?;
        self.store.advance_head(actor, &attempt.branch, new_head, expected_head_version)
    }

    /// Kill an attempt: free the slot, deactivate the lease. The steps it
    /// already billed stay billed — spent compute is spent.
    pub fn cancel_attempt(&self, actor: &Actor, attempt_id: &str) -> Result<()> {
        actor.require(ActorRole::Operator, "cancelling an attempt")?;
        self.store
            .conn
            .execute(
                "UPDATE attempts SET active = 0 WHERE id = ?1",
                rusqlite::params![attempt_id],
            )
            .map_err(crate::store::db_err)?;
        Ok(())
    }

    /// Idempotent billing keyed by message id. Replays bill once.
    pub fn bill(&self, message_id: &str, steps: u32) -> Result<Billing> {
        let inserted = self
            .store
            .conn
            .execute(
                "INSERT OR IGNORE INTO billing (message_id, steps, outcome)
                 VALUES (?1, ?2, 'charged')",
                rusqlite::params![message_id, steps],
            )
            .map_err(crate::store::db_err)?;
        if inserted == 0 {
            return Ok(Billing::Duplicate);
        }
        Ok(Billing::Charged)
    }

    /// Record an unknown outcome for an external call. The ledger keeps
    /// the debit; the outcome is neither success nor failure — a later
    /// reconciliation decides.
    pub fn record_unknown_outcome(&self, message_id: &str, steps: u32, detail: &str) -> Result<()> {
        self.bill(message_id, steps)?;
        self.store
            .conn
            .execute(
                "UPDATE billing SET outcome = 'unknown', detail = ?1 WHERE message_id = ?2",
                rusqlite::params![detail, message_id],
            )
            .map_err(crate::store::db_err)?;
        Ok(())
    }

    /// Billing rows, unknown outcomes included — visibility is the point.
    pub fn unknown_outcomes(&self) -> Result<Vec<(String, u32, String)>> {
        let mut stmt = self
            .store
            .conn
            .prepare("SELECT message_id, steps, detail FROM billing WHERE outcome = 'unknown'")
            .map_err(crate::store::db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(crate::store::db_err)?;
        rows.collect::<std::result::Result<Vec<_>, _>>().map_err(crate::store::db_err)
    }

    fn get_attempt(&self, attempt_id: &str) -> Result<Attempt> {
        self.store
            .conn
            .query_row(
                "SELECT id, branch, run, lease_ms, active, billed
                 FROM attempts WHERE id = ?1",
                rusqlite::params![attempt_id],
                |row| {
                    Ok(Attempt {
                        schema: SCHEMA_VERSION,
                        id: row.get(0)?,
                        branch: row.get(1)?,
                        run: row.get(2)?,
                        lease_expires_ms: row.get(3)?,
                        active: row.get::<_, i64>(4)? != 0,
                        billed_steps: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(crate::store::db_err)?
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::ArtifactUnavailable,
                    format!("no attempt {attempt_id}"),
                )
            })
    }
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(ffi, _)
        if ffi.code == rusqlite::ErrorCode::ConstraintViolation)
}

/// Check a coordinator epoch against the store's, from inside a caller's
/// transaction.
///
/// The comparison lives here so the store's own commit path and this
/// module's attempt path cannot disagree about what a superseded owner
/// is. A receipt stamped with an older epoch is a process that was
/// mid-flight across a restart: its result is not the record of what
/// happened, because a new owner has been working since.
pub fn check_epoch(tx: &rusqlite::Transaction<'_>, epoch: u64) -> Result<()> {
    let current: Option<String> = tx
        .query_row(
            "SELECT value FROM meta WHERE key = 'coordinator.owner'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(crate::store::db_err)?;
    let current_epoch = current
        .and_then(|raw| serde_json::from_str::<OwnershipRecord>(&raw).ok())
        .map_or(0, |r| r.epoch);
    if epoch != current_epoch {
        return Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "this work was stamped with ownership epoch {epoch}, the store is now at \
                 epoch {current_epoch}; a superseded coordinator's result is not a result"
            ),
        ));
    }
    Ok(())
}

/// The population record the Zio policy proposes and the coordinator
/// enforces. Re-exported for the shell's inspect surface.
pub fn population_summary(population: &Population) -> String {
    format!(
        "population {} members [{}] spent {}/{} steps (policy {})",
        population.id,
        population.members.join(", "),
        population.spent_steps,
        population.total_budget_steps,
        population.policy
    )
}

/// Branch identity for the Zio allocation policy's inputs (W09 keeps the
/// policy pure; the coordinator enforces whatever it proposes).
pub fn branch_record(branch: &Branch) -> (String, Option<String>, u32) {
    (branch.id.clone(), branch.head.clone(), branch.budget_quota)
}
