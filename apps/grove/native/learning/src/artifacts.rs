//! Content-addressed immutable artifact store.
//!
//! The write order is the whole point: bytes land in a temporary file, are
//! fsynced, and only then are they linked to their digest name. A process
//! killed mid-write leaves a stray temporary file that no manifest can
//! reference; it can never be mistaken for a committed artifact.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use zio_core::context::EvalContext;
use zio_core::error::EvalError;
use zio_core::value::{NativeFn, Value};

use crate::contracts::{ArtifactRef, Error, ErrorKind, Result, digest_bytes};

/// Digests as hex strings, so a manifest is greppable and a test can assert
/// on it without decoding bytes.
pub type ArtifactDigest = String;

/// Filesystem layout: `objects/<aa>/<full-hex>`.
#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

/// A handle to a stored object, kept private to the write path.
struct PendingWrite {
    path: PathBuf,
    artifact: ArtifactRef,
}

impl ArtifactStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("objects").join("tmp")).map_err(|e| {
            Error::new(
                ErrorKind::BackendFailed,
                format!("create artifact root: {e}"),
            )
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The on-disk location of one object. Public so retention and failure
    /// paths act on the real bytes a user would delete, not on a copy.
    pub fn object_path(&self, digest: &ArtifactRef) -> PathBuf {
        let hex = digest.to_hex();
        self.root.join("objects").join(&hex[..2]).join(&hex)
    }

    /// Every stored object with the modification time retention reads.
    /// Walks the real directory: an object a crashed writer left behind
    /// shows up here, which is exactly what cleanup is for.
    pub fn objects_with_age(&self) -> Result<Vec<(ArtifactRef, std::time::SystemTime)>> {
        let mut out = Vec::new();
        let shard_root = self.root.join("objects");
        let shards = match fs::read_dir(&shard_root) {
            Ok(shards) => shards,
            Err(_) => return Ok(out),
        };
        for shard in shards.flatten() {
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let objects = match fs::read_dir(shard.path()) {
                Ok(objects) => objects,
                Err(_) => continue,
            };
            for object in objects.flatten() {
                let name = object.file_name();
                let Some(hex) = name.to_str() else { continue };
                let Ok(digest) = ArtifactRef::parse_hex(hex) else {
                    continue; // a temp or foreign file is not a live object
                };
                let mtime = object.metadata().and_then(|m| m.modified()).map_err(|e| {
                    Error::new(ErrorKind::BackendFailed, format!("stat object {hex}: {e}"))
                })?;
                out.push((digest, mtime));
            }
        }
        Ok(out)
    }

    fn tmp_path(&self) -> PathBuf {
        let unique = format!(
            "w-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        );
        self.root.join("objects").join("tmp").join(unique)
    }

    /// Store bytes under their digest. Idempotent: an object that already
    /// exists is left untouched, so re-committing a manifest is harmless.
    pub fn put(&self, bytes: &[u8]) -> Result<ArtifactRef> {
        let artifact = digest_bytes(bytes);
        let final_path = self.object_path(&artifact);
        if final_path.exists() {
            return Ok(artifact);
        }
        let pending = self.write_temp(bytes)?;
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                Error::new(ErrorKind::BackendFailed, format!("create object dir: {e}"))
            })?;
        }
        match fs::hard_link(&pending.path, &final_path) {
            Ok(()) => {
                // The object is durable under its digest name; the temp copy
                // is now redundant.
                let _ = fs::remove_file(&pending.path);
                Ok(artifact)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&pending.path);
                Ok(artifact)
            }
            Err(e) => {
                let _ = fs::remove_file(&pending.path);
                Err(Error::new(
                    ErrorKind::BackendFailed,
                    format!("commit artifact {}: {e}", pending.artifact),
                ))
            }
        }
    }

    fn write_temp(&self, bytes: &[u8]) -> Result<PendingWrite> {
        let path = self.tmp_path();
        let artifact = digest_bytes(bytes);
        {
            let mut file = fs::File::create(&path).map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("create temp artifact: {e}"),
                )
            })?;
            file.write_all(bytes).map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("write temp artifact: {e}"),
                )
            })?;
            file.sync_all().map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("fsync temp artifact: {e}"),
                )
            })?;
        }
        Ok(PendingWrite { path, artifact })
    }

    /// Read bytes back, verifying the digest. A corrupted body is rejected
    /// here rather than handed on as if it described the right object.
    pub fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>> {
        let path = self.object_path(artifact);
        let bytes = fs::read(&path).map_err(|e| {
            Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("read artifact {}: {e}", artifact),
            )
        })?;
        let actual = digest_bytes(&bytes);
        if actual != *artifact {
            return Err(Error::new(
                ErrorKind::ArtifactUnavailable,
                format!("artifact {artifact} failed digest check (content is {actual})"),
            ));
        }
        Ok(bytes)
    }

    pub fn exists(&self, artifact: &ArtifactRef) -> bool {
        self.object_path(artifact).exists()
    }

    /// Delete an object. Only the retention path calls this, and only after
    /// the store has proved no root reaches it; a missing object is already
    /// gone, so it is not an error.
    pub fn remove(&self, artifact: &ArtifactRef) -> Result<bool> {
        match fs::remove_file(self.object_path(artifact)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(Error::new(
                ErrorKind::BackendFailed,
                format!("remove artifact {artifact}: {e}"),
            )),
        }
    }

    /// Drop stray temporary files from interrupted writes. Objects with a
    /// digest name are untouched.
    pub fn sweep_temp(&self) -> Result<usize> {
        let tmp_dir = self.root.join("objects").join("tmp");
        let mut removed = 0;
        let entries = match fs::read_dir(&tmp_dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(0),
        };
        for entry in entries.flatten() {
            if fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

// ── Zio attach ─────────────────────────────────────────────────────

/// Register `grove-artifact-put` / `grove-artifact-get` for Zio scripts.
///
/// Content crosses the boundary as a UTF-8 string, which keeps the binding
/// surface small; binary payloads go through the host API directly. A
/// missing store is `capability-denied:`, not a missing symbol.
pub fn install(ctx: &EvalContext, store: Option<std::sync::Arc<crate::store::Store>>) {
    let put_store = store.clone();
    ctx.env.set(
        "grove-artifact-put".to_string(),
        Value::NativeFunction(NativeFn::new("grove-artifact-put", move |args, _engine| {
            let store = put_store.as_ref().ok_or_else(|| {
                EvalError::custom(
                    "capability-denied: grove store not installed (grove-artifact-put)",
                )
            })?;
            let text = match args.get(0) {
                Some(Value::String(s)) => s,
                _ => {
                    return Err(EvalError::custom(
                        "grove-artifact-put: expected a string body",
                    ));
                }
            };
            let artifact = store.artifacts().put(text.as_bytes()).map_err(eval_err)?;
            Ok(Value::String(artifact.to_hex()))
        })),
    );

    let get_store = store;
    ctx.env.set(
        "grove-artifact-get".to_string(),
        Value::NativeFunction(NativeFn::new("grove-artifact-get", move |args, _engine| {
            let store = get_store.as_ref().ok_or_else(|| {
                EvalError::custom(
                    "capability-denied: grove store not installed (grove-artifact-get)",
                )
            })?;
            let hex = match args.get(0) {
                Some(Value::String(s)) => s,
                _ => {
                    return Err(EvalError::custom(
                        "grove-artifact-get: expected a digest string",
                    ));
                }
            };
            let artifact = ArtifactRef::parse_hex(hex).map_err(eval_err)?;
            let bytes = store.artifacts().get(&artifact).map_err(eval_err)?;
            let text = String::from_utf8(bytes)
                .map_err(|e| EvalError::custom(format!("grove-artifact-get: {e}")))?;
            Ok(Value::String(text))
        })),
    );
}

fn eval_err(error: Error) -> EvalError {
    EvalError::custom(error.to_string())
}
