use std::sync::{Arc, Mutex};

use crate::error::EvalError;

/// Host I/O interface for decoupling output and input from std::io.
/// Permits output capturing in tests, WASM embeddings, and sandbox environments (ADR-011).
pub trait IoHost: Send + Sync {
    fn print(&self, msg: &str) -> Result<(), EvalError>;
    fn println(&self, msg: &str) -> Result<(), EvalError>;
    fn read_line(&self) -> Result<String, EvalError>;
}

/// Standard I/O host using std::io stdout and stdin.
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
}

/// In-memory buffered I/O host for unit tests, REPL testing, and sandboxed execution.
#[derive(Debug, Default, Clone)]
pub struct BufferIoHost {
    output: Arc<Mutex<String>>,
    input: Arc<Mutex<Vec<String>>>,
}

impl BufferIoHost {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_input(input_lines: Vec<String>) -> Self {
        BufferIoHost {
            output: Arc::new(Mutex::new(String::new())),
            input: Arc::new(Mutex::new(input_lines)),
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
}
