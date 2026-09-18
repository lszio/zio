use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::EvalError;

/// Host I/O interface for decoupling output, input, and file access
/// from std::io. Permits output capturing in tests, WASM embeddings,
/// and sandbox environments (ADR-011).
pub trait IoHost: Send + Sync {
    fn print(&self, msg: &str) -> Result<(), EvalError>;
    fn println(&self, msg: &str) -> Result<(), EvalError>;
    fn read_line(&self) -> Result<String, EvalError>;

    /// Working directory used to resolve relative paths. Sandboxed hosts
    /// may return an error to disallow path resolution.
    fn current_dir(&self) -> Result<String, EvalError>;
    /// Read an entire file as a string.
    fn read_file(&self, path: &str) -> Result<String, EvalError>;
    /// Write a string to a file.
    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError>;
    /// Check whether a file exists.
    fn file_exists(&self, path: &str) -> Result<bool, EvalError>;
}

/// Standard I/O host using std::io stdout/stdin and the real filesystem.
#[derive(Debug, Default, Clone)]
pub struct StdIoHost;

impl IoHost for StdIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        use std::io::Write;
        print!("{msg}");
        std::io::stdout()
            .flush()
            .map_err(|e| EvalError::custom(format!("stdout flush error: {e}")))
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        println!("{msg}");
        Ok(())
    }

    fn read_line(&self) -> Result<String, EvalError> {
        let mut buffer = String::new();
        std::io::stdin()
            .read_line(&mut buffer)
            .map_err(|e| EvalError::custom(format!("stdin read error: {e}")))?;
        Ok(buffer)
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| EvalError::custom(format!("cannot get cwd: {e}")))
    }

    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        std::fs::read_to_string(path)
            .map_err(|e| EvalError::custom(format!("cannot read {path}: {e}")))
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        std::fs::write(path, data)
            .map_err(|e| EvalError::custom(format!("cannot write {path}: {e}")))
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        Ok(std::path::Path::new(path).exists())
    }
}

/// In-memory buffered I/O host for unit tests, REPL testing, and sandboxed
/// execution. Files live in a virtual in-memory filesystem — no real
/// filesystem access.
#[derive(Debug, Default, Clone)]
pub struct BufferIoHost {
    output: Arc<Mutex<String>>,
    input: Arc<Mutex<Vec<String>>>,
    files: Arc<Mutex<HashMap<String, String>>>,
}

impl BufferIoHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_input(input_lines: Vec<String>) -> Self {
        BufferIoHost {
            output: Arc::new(Mutex::new(String::new())),
            input: Arc::new(Mutex::new(input_lines)),
            files: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Pre-populate the virtual filesystem (path → content).
    pub fn with_files(files: Vec<(String, String)>) -> Self {
        BufferIoHost {
            output: Arc::new(Mutex::new(String::new())),
            input: Arc::new(Mutex::new(Vec::new())),
            files: Arc::new(Mutex::new(files.into_iter().collect())),
        }
    }

    pub fn get_output(&self) -> String {
        self.output.lock().unwrap().clone()
    }

    pub fn clear_output(&self) {
        self.output.lock().unwrap().clear();
    }

    pub fn push_input(&self, line: impl Into<String>) {
        self.input.lock().unwrap().push(line.into());
    }
}

impl IoHost for BufferIoHost {
    fn print(&self, msg: &str) -> Result<(), EvalError> {
        self.output.lock().unwrap().push_str(msg);
        Ok(())
    }

    fn println(&self, msg: &str) -> Result<(), EvalError> {
        let mut out = self.output.lock().unwrap();
        out.push_str(msg);
        out.push('\n');
        Ok(())
    }

    fn read_line(&self) -> Result<String, EvalError> {
        let mut in_queue = self.input.lock().unwrap();
        if in_queue.is_empty() {
            Err(EvalError::custom("BufferIoHost: end of input stream"))
        } else {
            Ok(in_queue.remove(0))
        }
    }

    fn current_dir(&self) -> Result<String, EvalError> {
        Err(EvalError::custom("BufferIoHost: sandboxed, no working directory"))
    }

    fn read_file(&self, path: &str) -> Result<String, EvalError> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| EvalError::custom(format!("cannot read {path}: no such file")))
    }

    fn write_file(&self, path: &str, data: &str) -> Result<(), EvalError> {
        self.files
            .lock()
            .unwrap()
            .insert(path.to_string(), data.to_string());
        Ok(())
    }

    fn file_exists(&self, path: &str) -> Result<bool, EvalError> {
        Ok(self.files.lock().unwrap().contains_key(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffer_io_host() {
        let io = BufferIoHost::with_input(vec!["hello\n".to_string()]);
        io.print("Foo ").unwrap();
        io.println("Bar").unwrap();

        assert_eq!(io.get_output(), "Foo Bar\n");
        assert_eq!(io.read_line().unwrap(), "hello\n");
        assert!(io.read_line().is_err());
    }

    #[test]
    fn test_buffer_io_host_virtual_files() {
        let io = BufferIoHost::new();
        assert!(!io.file_exists("v.zio").unwrap());
        io.write_file("v.zio", "(def x 1)").unwrap();
        assert!(io.file_exists("v.zio").unwrap());
        assert_eq!(io.read_file("v.zio").unwrap(), "(def x 1)");
        assert!(io.current_dir().is_err(), "sandboxed host has no cwd");
    }
}
