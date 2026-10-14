// The WASI runtime — the only target that depends on nothing but WASI
// imports. It never opens the browser `wasm` feature, so no js-sys or
// globalThis ends up in the module. The payload is compiled in per
// application (WASI gives a module no way to read its own file), which
// is what makes the artifact a single self-contained .wasm.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zio_core::bundle::run_payload_with;
use zio_core::error::EvalError;
use zio_core::io::IoHost;

include!(concat!(env!("OUT_DIR"), "/payload.rs"));

/// stdin/stdout/stderr pass through; every file operation must land
/// inside a directory the caller granted with `--grant DIR`.
struct WasiIoHost {
    grants: Vec<PathBuf>,
}

fn normalize(path: &str) -> Result<PathBuf, EvalError> {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| EvalError::custom(format!("cannot get cwd: {error}")))?
            .join(candidate)
    };
    // Lexical normalization: wasi-libc's canonicalize needs capabilities
    // a plain preopen does not always grant. The host read that follows
    // still obeys the preopen, so this only decides the grant boundary,
    // not the data.
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

impl WasiIoHost {
    fn check(&self, path: &str) -> Result<PathBuf, EvalError> {
        let resolved = normalize(path)?;
        if self.grants.iter().any(|root| resolved.starts_with(root)) {
            return Ok(resolved);
        }
        Err(EvalError::custom(format!(
            "file access denied: {path} is outside every granted root"
        )))
    }
}

impl IoHost for WasiIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        use std::io::Write;
        print!("{msg}");
        std::io::stdout()
            .flush()
            .map_err(|error| EvalError::custom(format!("stdout flush: {error}")))
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        println!("{msg}");
        Ok(())
    }

    fn read_line(&self) -> Result<String, EvalError> {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|error| EvalError::custom(format!("stdin read: {error}")))?;
        Ok(line)
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|error| EvalError::custom(format!("cannot get cwd: {error}")))
    }

    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        std::fs::read_to_string(self.check(path)?)
            .map_err(|error| EvalError::custom(format!("cannot read {path}: {error}")))
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        std::fs::write(self.check(path)?, data)
            .map_err(|error| EvalError::custom(format!("cannot write {path}: {error}")))
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        Ok(self.check(path)?.exists())
    }

    fn canonicalize_path(&self, path: &str) -> Result<String, EvalError> {
        Ok(self.check(path)?.to_string_lossy().into_owned())
    }

    fn is_directory(&self, path: &str) -> Result<bool, EvalError> {
        Ok(std::fs::metadata(self.check(path)?)
            .map_err(|error| EvalError::custom(format!("cannot inspect {path}: {error}")))?
            .is_dir())
    }
}

fn main() {
    if PAYLOAD.is_empty() {
        eprintln!(
            "zio-wasi: this build carries no payload (build it with `zio build --target wasm-wasi`)"
        );
        std::process::exit(2);
    }
    let mut grants = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--grant" => match args.next() {
                Some(dir) => match normalize(&dir) {
                    Ok(root) => grants.push(root),
                    Err(error) => {
                        eprintln!("cannot grant {dir}: {error}");
                        std::process::exit(1);
                    }
                },
                None => {
                    eprintln!("--grant needs a directory");
                    std::process::exit(2);
                }
            },
            other => {
                eprintln!("unknown argument {other:?}; usage: zio-wasi [--grant DIR]...");
                std::process::exit(2);
            }
        }
    }
    let host: Arc<dyn IoHost> = Arc::new(WasiIoHost { grants });
    match run_payload_with(host, PAYLOAD, true) {
        Ok(value) => {
            if !value.is_empty() && value != "nil" {
                println!("{value}");
            }
        }
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    }
}
