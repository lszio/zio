//! `grove` — the trusted learning host for zio (grove delivery plan W01).
//!
//! Layout mirrors the design's separation of concerns:
//!
//! * [`contracts`] — versioned record types and the single error vocabulary;
//! * [`artifacts`] — content-addressed, immutable artifact store;
//! * [`store`] — SQLite transactions that commit manifests and references
//!   only after their bytes are durable.
//!
//! Nothing here learns. The host validates, persists and authorizes; the
//! learning policy lives in zio (`lib/zio/learn.zio` and friends), and the
//! product shell lives in `grove-app`. `core` never depends on this crate
//! (ADR-009 direction holds: `grove` attaches to `zio-core`, not vice versa).

pub mod artifacts;
pub mod composition;
pub mod contracts;
pub mod coordinator;
pub mod ensemble;
pub mod evaluation;
pub mod events;
pub mod execution;
pub mod isolation;
pub mod checkpoint;
pub mod lineage;
pub mod logic;
pub mod memory;
pub mod product;
pub mod recipes;
pub mod runner_machine;
pub mod store;
pub mod worker;

pub use artifacts::{ArtifactDigest, ArtifactStore};
pub use contracts::{
    Actor, ActorRole, Branch, Checkpoint, DatasetRevision, EnsembleRule, EnsembleSpec, Error,
    ErrorKind, ExpertSpec, ModelSnapshot, ModuleSpec, Observation, Prediction, Recipe, Run,
    SchemaVersion, SignalKind,
};
pub use store::Store;
pub use isolation::{IsolationProfile, MountSpec, ReadOnlyRoot};
pub use worker::{Frame, Isolation, Worker, WorkerConfig};

/// Wall-clock milliseconds. Every record that needs a timestamp calls
/// this rather than `SystemTime::now()` inline, so a test can find every
/// time value in one place.
pub fn api_time_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Install grove's native bindings into an [`zio_core::context::EvalContext`].
///
/// The bindings are the Zio-visible surface of the host; capabilities that
/// are not provisioned fail with `capability-denied:` rather than a missing
/// symbol, matching the Loom attach convention (ADR-016/ADR-019).
pub fn install(
    ctx: &zio_core::context::EvalContext,
    store: Option<std::sync::Arc<Store>>,
) {
    artifacts::install(ctx, store);
}
