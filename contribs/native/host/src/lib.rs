//! Generic native mechanisms. Authority is supplied by the embedding process,
//! never inferred from source, manifests, serialized handles or application data.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use zio_core::context::EvalContext;
use zio_core::error::EvalError;

pub mod execution;
pub mod storage;
pub mod tensor;
pub mod transport;
pub mod values;

#[derive(Clone, Debug, Default)]
pub struct HostPolicy {
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub environment: BTreeSet<String>,
    pub argv: Vec<String>,
    pub network: bool,
    pub process: bool,
    pub tensor: bool,
    pub stdio: bool,
    /// Fixed installed backend authority; source configuration cannot replace it.
    pub trusted_tensor_backend: Option<PathBuf>,
    pub trusted_tensor_python: Option<PathBuf>,
}

impl HostPolicy {
    pub fn read_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, EvalError> {
        let path = path.as_ref();
        let canonical = path.canonicalize().map_err(|error| {
            EvalError::custom(format!("artifact-unavailable: {}: {error}", path.display()))
        })?;
        if self
            .read_roots
            .iter()
            .chain(&self.write_roots)
            .any(|root| canonical.starts_with(root))
        {
            Ok(canonical)
        } else {
            Err(EvalError::custom(format!(
                "capability-denied: read {}",
                path.display()
            )))
        }
    }

    pub fn write_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, EvalError> {
        let original = path.as_ref();
        let absolute = if original.is_absolute() {
            original.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| EvalError::custom(format!("backend-failed: cwd: {error}")))?
                .join(original)
        };
        let mut ancestor = absolute.as_path();
        let mut suffix = Vec::new();
        let canonical = loop {
            match ancestor.canonicalize() {
                Ok(path) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let name = ancestor.file_name().ok_or_else(|| {
                        EvalError::custom(format!(
                            "invalid-input: write path {}",
                            original.display()
                        ))
                    })?;
                    if ancestor
                        .components()
                        .next_back()
                        .is_some_and(|part| !matches!(part, Component::Normal(_)))
                    {
                        return Err(EvalError::custom(
                            "capability-denied: unresolved parent component",
                        ));
                    }
                    suffix.push(name.to_os_string());
                    ancestor = ancestor.parent().ok_or_else(|| {
                        EvalError::custom("invalid-input: write path has no parent")
                    })?;
                }
                Err(error) => {
                    return Err(EvalError::custom(format!(
                        "backend-failed: {}: {error}",
                        ancestor.display()
                    )));
                }
            }
        };
        let mut resolved = canonical;
        for name in suffix.into_iter().rev() {
            resolved.push(name);
        }
        if self
            .write_roots
            .iter()
            .any(|root| resolved.starts_with(root))
        {
            Ok(resolved)
        } else {
            Err(EvalError::custom(format!(
                "capability-denied: write {}",
                original.display()
            )))
        }
    }

    /// Capture canonical roots before handing policy to a source execution.
    pub fn canonicalized(mut self) -> Result<Self, EvalError> {
        for root in self.read_roots.iter_mut().chain(&mut self.write_roots) {
            *root = root.canonicalize().map_err(|error| {
                EvalError::custom(format!(
                    "invalid-input: capability root {}: {error}",
                    root.display()
                ))
            })?;
            if !root.is_dir() {
                return Err(EvalError::custom(format!(
                    "invalid-input: capability root is not a directory: {}",
                    root.display()
                )));
            }
        }
        Ok(self)
    }
}

pub fn install(ctx: &EvalContext, policy: HostPolicy) -> Result<(), EvalError> {
    let policy = policy.canonicalized()?;
    storage::install(ctx, &policy);
    transport::install(ctx, &policy);
    tensor::install(ctx, &policy);
    // The bounded runner grants nothing; it is the one primitive that
    // must be present even when every capability flag is false, because
    // its whole job is to run code with no capabilities at all.
    execution::install(ctx, &policy);
    Ok(())
}
