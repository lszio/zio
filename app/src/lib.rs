//! The grove product shell.
//!
//! This crate is deliberately thin: every command maps onto a contract
//! the library already enforces. The CLI never re-implements training,
//! gating or selection — it builds the arguments, calls the library, and
//! reports what happened. That is what makes its output trustworthy: the
//! binary cannot be more permissive than the library.

#[cfg(feature = "http")]
pub mod api;
pub mod demo;
pub mod population;

use std::path::{Path, PathBuf};

use grove::contracts::{Actor, ActorRole, Error, ErrorKind, Result};
use grove::store::Store;

/// The CLI acts as the operator; publication escalates to the publisher
/// role explicitly — annotating, operating and publishing are different
/// grants even inside one binary.
pub fn operator() -> Actor {
    Actor::new("grove-cli", ActorRole::Operator)
}

pub fn publisher() -> Actor {
    Actor::new("grove-cli", ActorRole::Publisher)
}

/// Open (or create) a grove store rooted at `root`.
pub fn open_store(root: &Path) -> Result<Store> {
    Store::open(root)
}

/// `grove inspect --root PATH` — everything history remembers, as one
/// honest table: runs, checkpoints, branches, publications.
pub fn inspect(store: &Store) -> Result<String> {
    let mut out = String::new();
    out.push_str("grove store\n");
    out.push_str(&format!("  root: {}\n", store.artifacts().root().display()));

    let objects = count_objects(store.artifacts().root());
    out.push_str(&format!("  artifacts: {objects} objects\n"));

    // runs and their ledgers
    let runs = store.runs();
    out.push_str(&format!("\nruns ({}):\n", runs.len()));
    for run in &runs {
        out.push_str(&format!(
            "  {:<14} {:<10} spent {}/{} steps{}\n",
            run.id,
            format!("{:?}", run.state).to_lowercase(),
            run.steps_consumed,
            run.steps_budget,
            run.resumed_from
                .as_ref()
                .map(|r| format!(" (resumed from {r})"))
                .unwrap_or_default(),
        ));
    }

    let checkpoints = store.checkpoints();
    out.push_str(&format!("\ncheckpoints ({}):\n", checkpoints.len()));
    for cp in &checkpoints {
        out.push_str(&format!(
            "  {:<28} run {} at {} steps ({:?})\n",
            cp.id,
            cp.run_id,
            cp.budget_spent_steps,
            cp.resume_level,
        ));
    }

    let branches = store.branches();
    out.push_str(&format!("\nbranches ({}):\n", branches.len()));
    for b in &branches {
        out.push_str(&format!(
            "  {:<14} head {:?} (v{}) quota {} steps\n",
            b.id, b.head, b.head_version, b.budget_quota
        ));
    }

    match store.active_publication()? {
        Some((version, snapshot)) => out.push_str(&format!(
            "\npublication: v{version} → {}\n",
            snapshot.to_hex()
        )),
        None => out.push_str("\npublication: none\n"),
    }
    Ok(out)
}

fn count_objects(root: &Path) -> usize {
    let mut count = 0;
    let Ok(entries) = std::fs::read_dir(root.join("objects")) else {
        return 0;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(files) = std::fs::read_dir(&path) {
                count += files.count();
            }
        }
    }
    count
}

/// `grove select --root PATH --protocol ID --snapshots a,b,c` — run the
/// protocol comparison and the non-dominated selection, printing the
/// verdict. Selection is advisory; publication stays a separate command.
pub fn select(
    store: &Store,
    protocol: grove::evaluation::EvaluationProtocol,
    snapshots: &[grove::contracts::ArtifactRef],
) -> Result<String> {
    let rows = grove::evaluation::compare(store, &protocol, snapshots)?;
    if rows.is_empty() {
        return Err(Error::new(
            ErrorKind::ArtifactUnavailable,
            "no evaluation records under this protocol",
        ));
    }
    let mut out = String::new();
    out.push_str(&format!(
        "comparison under {} (gates: {:?}):\n",
        protocol.id,
        protocol.gates
    ));
    for row in &rows {
        let metrics = row
            .mean
            .iter()
            .map(|(n, v)| format!("{n}={v:.4}"))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            "  {}… repeats={} {metrics} → {}\n",
            &row.snapshot.to_hex()[..12],
            row.repeats,
            if row.meets_gates {
                "meets gates".to_string()
            } else {
                format!("GATE FAILED ({})", row.gate_failures.join("; "))
            }
        ));
    }

    let candidates: Vec<grove::evaluation::Candidate> = rows
        .iter()
        .map(|r| grove::evaluation::Candidate {
            snapshot: r.snapshot,
            quality: r
                .mean
                .iter()
                .find(|(n, _)| n == "accuracy")
                .map(|(_, v)| *v)
                .unwrap_or(0.0),
            // inference cost accounting is a W08+ concern; uniform here so
            // the selection axis is honest about being a placeholder
            cost: 1.0,
            meets_gates: r.meets_gates,
        })
        .collect();
    let kept = grove::evaluation::non_dominated(&candidates);
    out.push_str(&format!(
        "\nnon-dominated candidates: {}\n",
        kept.iter()
            .map(|c| c.snapshot.to_hex()[..12].to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    Ok(out)
}

/// Where the python pieces live, relative to the repo the binary was
/// built from. Overridable for packaged installs.
pub struct Paths {
    pub python: PathBuf,
    pub generate: PathBuf,
    pub worker: PathBuf,
    pub task: PathBuf,
    /// The workspace root: the worker's cwd, so the frozen data paths in
    /// requests resolve the same way from any harness.
    pub root: PathBuf,
}

impl Paths {
    pub fn from_repo_root() -> Self {
        // CARGO_MANIFEST_DIR of the app crate → workspace root
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        Self {
            python: root.join(".venv/bin/python"),
            generate: root.join("examples/self-learning/generate.py"),
            worker: root.join("workers/torch/worker.py"),
            task: root.join("examples/self-learning"),
            root,
        }
    }
}

/// Load a provisioned protocol. Rebuilding one from CLI flags would let a
/// flag drop the frozen gates — the exact "shell rewrites policy" the
/// delivery plan forbids — so an unknown protocol is a hard stop.
pub fn load_protocol(store: &Store, id: &str) -> Result<grove::evaluation::EvaluationProtocol> {
    store.get_protocol(id).map_err(|e| {
        Error::new(
            ErrorKind::ArtifactUnavailable,
            format!("protocol {id:?} is not provisioned in this store: {e}"),
        )
    })
}

/// Arg-parsing error with the CLI's stable vocabulary.
pub fn usage(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message.to_string())
}
