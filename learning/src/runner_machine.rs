//! G03: the run state machine, in one place.
//!
//! The plan's machine is short, and the reason to write it down as data
//! rather than as a `match` inside the store is that two callers need
//! it: the owner loop that moves a run, and the store that refuses to
//! write an impossible state. Two copies of a rule about what may
//! follow what is how a run ends up Queued → Accepted without running.
//!
//! ```text
//! Queued     -> Running | Cancelled | Failed
//! Running    -> Evaluating | Paused | Failed | Cancelled
//! Evaluating -> Accepted | Rejected | Failed | Cancelled
//! ```
//!
//! Two edges are deliberately absent. `Queued -> Evaluating` skips the
//! work. `Paused -> Running` reuses a run that has already produced a
//! result: the plan requires Paused to continue as a *new* Queued run,
//! so a resume cannot overwrite the run it came from.

use crate::contracts::{Error, ErrorKind, Result, RunState};

/// The edges the machine allows. Anything not listed is refused.
const EDGES: &[(RunState, RunState)] = &[
    (RunState::Queued, RunState::Running),
    (RunState::Queued, RunState::Cancelled),
    (RunState::Queued, RunState::Failed),
    (RunState::Running, RunState::Evaluating),
    (RunState::Running, RunState::Paused),
    (RunState::Running, RunState::Failed),
    (RunState::Running, RunState::Cancelled),
    (RunState::Evaluating, RunState::Accepted),
    (RunState::Evaluating, RunState::Rejected),
    (RunState::Evaluating, RunState::Failed),
    (RunState::Evaluating, RunState::Cancelled),
];

/// Is `to` reachable from `from`?
pub fn allowed(from: RunState, to: RunState) -> bool {
    if from == to {
        // Re-writing the same state is not a transition, and allowing it
        // would make every idempotent retry look like progress.
        return false;
    }
    EDGES.iter().any(|(f, t)| *f == from && *t == to)
}

/// Refuse an impossible transition, naming both ends so the refusal is
/// actionable rather than merely a rejection.
pub fn check_transition(from: RunState, to: RunState) -> Result<()> {
    if allowed(from, to) {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::InvalidInput,
        format!(
            "a run cannot go {} -> {}; the machine is queued→running→evaluating→accepted|rejected, \
             with paused only after a committed checkpoint and cancelled/failed from any live state",
            label(from),
            label(to)
        ),
    ))
}

/// The state machine as a string, for an error message or a report.
pub fn describe() -> String {
    EDGES
        .iter()
        .map(|(f, t)| format!("{}→{}", label(*f), label(*t)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn label(state: RunState) -> &'static str {
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
