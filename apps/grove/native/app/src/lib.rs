//! Existing native Grove CLI/HTTP application.
//!
//! Business orchestration is still implemented here and in the native
//! `grove` crate. It remains under `apps/grove/native/` until migrated to
//! Zio; it is neither language infrastructure nor the language playground.

#[cfg(feature = "http")]
pub mod api;
pub mod agent;
pub mod container;
pub mod demo;
pub mod inference;
pub mod modular;
pub mod population;
pub mod runner;

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

    // The decisions, not just the outcome: a version number with nobody
    // behind it is not an audit trail.
    let approvals = store.approval_log()?;
    out.push_str(&format!("\napprovals ({}):\n", approvals.len()));
    for approval in &approvals {
        out.push_str(&format!(
            "  {:<34} {:<12} v{version}\n",
            approval["id"].as_str().unwrap_or("?"),
            approval["authenticated_actor"].as_str().unwrap_or("?"),
            version = approval["expected_publication_version"]
                .as_i64()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "any".to_string()),
        ));
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
/// Adapt protocol comparison rows to Grove's embedded Zio selection policy.
/// Only typed input/output conversion lives here; no publication or model bindings.
pub fn select_candidates(
    rows: &[grove::evaluation::Comparison],
) -> Result<Vec<&grove::evaluation::Comparison>> {
    use zio_core::bootstrap::{eval_source, language_context, ModuleRoots};
    use zio_core::im::vector;
    use zio_core::sexp::Sexp;
    use zio_core::value::Value;

    let strategy_error = |e: zio_core::error::EvalError| {
        Error::new(ErrorKind::BackendFailed, format!("Grove selection strategy failed: {e}"))
    };
    let ctx = language_context(ModuleRoots::empty()).map_err(strategy_error)?;
    eval_source(&ctx, "apps/grove/selection.zio", include_str!("../../../selection.zio"))
        .map_err(strategy_error)?;
    let input = rows.iter().enumerate().map(|(index, row)| {
        Value::Map([
            (Value::Keyword("index".into()), Value::Integer(index as i64)),
            (Value::Keyword("mean".into()), Value::Vector(row.mean.iter().map(|(name, value)| {
                Value::Vector(vector![Value::String(name.clone()), Value::Float(*value)])
            }).collect())),
            // ponytail: uniform cost preserves existing CLI/HTTP semantics; use measured inference cost when available.
            (Value::Keyword("cost".into()), Value::Float(1.0)),
            (Value::Keyword("meets-gates".into()), Value::Boolean(row.meets_gates)),
        ].into_iter().collect())
    }).collect();
    ctx.env.set("grove-selection-rows".into(), Value::Vector(input));
    let result = zio_core::eval::eval_in_context(
        &Sexp::List(vector![
            Sexp::Symbol("grove-select".into(), None),
            Sexp::Symbol("grove-selection-rows".into(), None),
        ], None),
        &ctx,
    ).map_err(strategy_error)?;
    let indices = match result {
        Value::List(indices) | Value::Vector(indices) => indices,
        other => return Err(Error::new(
            ErrorKind::BackendFailed,
            format!("Grove selection returned {other}, not row indices"),
        )),
    };
    indices.iter().map(|index| {
        if let Value::Integer(index) = index {
            if let Ok(index) = usize::try_from(*index) {
                if let Some(row) = rows.get(index) {
                    return Ok(row);
                }
            }
        }
        Err(Error::new(ErrorKind::BackendFailed, "Grove selection returned an invalid row index"))
    }).collect()
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

    let kept = select_candidates(&rows)?;
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
            .ancestors().nth(4)
            .unwrap()
            .to_path_buf();
        Self {
            python: root.join(".venv/bin/python"),
            generate: root.join("examples/self-learning/generate.py"),
            worker: root.join("apps/grove/workers/torch/worker.py"),
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
