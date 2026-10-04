//! Real inference for the product surface.
//!
//! A prediction the product displays must come from the same worker the
//! trainer uses, on the same task data, bound to the exact snapshot the
//! caller asked about. The product does not compute an answer itself and
//! does not cache one: a prediction record whose answer the API invented
//! would be a lie with a digest attached.
//!
//! The worker is spawned per request and killed after the frame stream
//! ends, so an idle server holds no training-adjacent process. Isolation
//! is the same enforced configuration the demo uses, and a platform
//! without it fails closed rather than running the worker unrestricted.

use std::path::PathBuf;
use std::time::Duration;

use grove::contracts::{Actor, ArtifactRef, Error, ErrorKind, Observation, Prediction, Result, SCHEMA_VERSION};
use grove::worker::{Frame, Isolation, Worker, WorkerConfig};
use grove::store::Store;

use crate::Paths;

/// What one prediction call produced.
#[derive(Debug, Clone, PartialEq)]
pub struct InferenceResult {
    pub prediction: Prediction,
    /// The class the worker answered, or `None` when the input could not
    /// be decided. Abstention is a real output.
    pub answer: Option<u32>,
    /// Rows the worker actually saw and decided on.
    pub decided: usize,
    pub abstain_rows: usize,
}

/// Run a real prediction through an isolated worker.
///
/// The weights come from the snapshot's own parameter artifact, so a
/// prediction always names the model version that produced it. The data
/// is the frozen W00 task split — the product does not accept an
/// arbitrary feature vector and call it an observation, because a
/// prediction over bytes nobody can inspect is not evidence.
pub fn predict(
    store: &Store,
    actor: &Actor,
    paths: &Paths,
    snapshot_digest: &ArtifactRef,
    snapshot_graph: &serde_json::Value,
    observation: &Observation,
    scratch: &std::path::Path,
) -> Result<InferenceResult> {
    let isolation = Isolation::probe("unshare").ok_or_else(|| {
        Error::denied("worker isolation unavailable; refusing to predict unrestricted")
    })?;
    isolation.enforce()?;

    // The snapshot's own parameter artifact IS the weights file the
    // worker loads. Any other file would make the prediction describe a
    // different model than the digest names.
    let weights_path = scratch.join(format!("weights-{}.json", &snapshot_digest.to_hex()[..12]));
    let bytes = store
        .artifacts()
        .get(&first_param_artifact(store, snapshot_digest)?)?;
    std::fs::write(&weights_path, &bytes).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("cannot stage the snapshot's weights: {e}"),
        )
    })?;

    // The observation's stored content block IS the input file, so what
    // the worker reads is the bytes the observation actually carries.
    let data_path = scratch.join(format!("obs-{}.bin", observation.id));
    let content = store.artifacts().get(&observation.blocks[0].artifact)?;
    std::fs::write(&data_path, &content).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("cannot stage the observation: {e}"),
        )
    })?;

    let out_path = scratch.join(format!("pred-{}.json", observation.id));
    let config = WorkerConfig {
        python: paths.python.clone(),
        worker_script: paths.worker.clone(),
        unshare: PathBuf::from("unshare"),
        scratch: std::env::temp_dir(),
        working_dir: paths.root.clone(),
        timeout: Duration::from_secs(120),
        max_address_space: 4 * 1024 * 1024 * 1024,
    };
    let mut worker = Worker::spawn(&config, &isolation)?;
    let result = (|| -> Result<(Vec<u8>, Vec<(u32, u32)>)> {
        worker.send(&Frame::Predict {
            v: grove::worker::PROTOCOL_VERSION,
            run_id: format!("predict-{}", observation.id),
            attempt_id: format!("api-{}", observation.id),
            graph: snapshot_graph.clone(),
            weights: weights_path.to_string_lossy().into_owned(),
            data: data_path.to_string_lossy().into_owned(),
            out: out_path.to_string_lossy().into_owned(),
        })?;
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::new(
                    ErrorKind::Timeout,
                    "the worker did not answer within the prediction deadline",
                ));
            }
            match worker.next_frame(remaining)? {
                Frame::Done { predictions, .. } => {
                    let path = predictions.ok_or_else(|| {
                        Error::new(ErrorKind::BackendFailed, "done frame without a prediction file")
                    })?;
                    let raw = std::fs::read(&path).map_err(|e| {
                        Error::new(
                            ErrorKind::BackendFailed,
                            format!("prediction file vanished: {e}"),
                        )
                    })?;
                    return Ok((raw, Vec::new()));
                }
                Frame::Failed { error, .. } => {
                    return Err(Error::new(
                        ErrorKind::BackendFailed,
                        format!("worker refused the prediction: {error}"),
                    ));
                }
                _ => continue,
            }
        }
    })();
    // The worker is killed on every path, including the error paths: a
    // leaked python holding the stdout pipe would hang the server's
    // next request.
    worker.kill();
    let (raw, _) = result?;

    let parsed: serde_json::Value = serde_json::from_slice(&raw).map_err(|e| {
        Error::new(
            ErrorKind::BackendFailed,
            format!("prediction output is not JSON: {e}"),
        )
    })?;
    let rows = parsed
        .get("predictions")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::BackendFailed,
                "prediction output has no predictions array",
            )
        })?;
    let mut answers = Vec::new();
    for row in rows {
        let index = row.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        let answer = row.get("answer").and_then(|v| v.as_u64()).map(|v| v as u32);
        answers.push((index, answer.unwrap_or(u32::MAX)));
    }

    // The store owns the prediction record and the observation must be
    // one the caller can see: a prediction about an unknown observation
    // is a dangling claim.
    let first = answers.first().map(|(i, a)| (*i, *a));
    let (answer, abstain_rows) = match first {
        None => (None, answers.len()),
        Some((_, u32::MAX)) => (None, answers.len()),
        Some((_, class)) => (
            Some(class),
            answers.iter().filter(|(_, a)| *a == u32::MAX).count(),
        ),
    };
    let decided = answers.len() - abstain_rows;

    let prediction = Prediction {
        schema: SCHEMA_VERSION,
        id: format!("pred-{}-{}", observation.id, &snapshot_digest.to_hex()[..8]),
        owner: actor.id.clone(),
        observation_id: observation.id.clone(),
        snapshot_digest: *snapshot_digest,
        snapshot_schema: SCHEMA_VERSION,
        output: match answer {
            Some(class) => class.to_string(),
            None => "abstain".to_string(),
        },
        abstained: answer.is_none(),
        created_at_ms: grove::api_time_ms(),
    };
    Ok(InferenceResult {
        prediction,
        answer,
        decided,
        abstain_rows,
    })
}

fn first_param_artifact(store: &Store, snapshot: &ArtifactRef) -> Result<ArtifactRef> {
    let snapshot = store.load_snapshot(snapshot)?;
    snapshot
        .params
        .first()
        .map(|p| p.artifact)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::IncompatibleState,
                "this snapshot carries no parameters: there is nothing to predict with",
            )
        })
}
