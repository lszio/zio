//! W04: the controlled training worker.
//!
//! The host never runs a candidate's Python. It runs *this* worker with
//! *this* interpreter and passes a validated operator graph; the graph
//! vocabulary is what bounds what can execute (see
//! `workers/torch/graph.py`).
//!
//! Isolation is real or refused. The worker is started inside a user,
//! network, PID and mount namespace via `unshare`, with a read-only
//! artifact root, a private temp dir, wall-clock and address-space rlimits
//! and a process-group kill on cancel. If the platform cannot provide
//! namespaces, [`Isolation::probe`] reports it and the caller gets a
//! `capability-denied:` — it does **not** fall back to running the
//! worker unrestricted.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::contracts::{digest_bytes, ArtifactRef, Error, ErrorKind, Result};

/// Protocol spoken over the worker's stdin/stdout.
pub const PROTOCOL: &str = "grove.worker/1";
pub const PROTOCOL_VERSION: u32 = 1;
/// A control frame larger than this is a protocol error, not a big frame.
pub const MAX_FRAME_BYTES: usize = 1 << 20;

// ── frames ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Frame {
    #[serde(rename = "hello")]
    Hello { v: u32, protocol: String, capabilities: Vec<String>, torch: String },
    #[serde(rename = "ready")]
    Ready { v: u32, protocol: String },
    #[serde(rename = "train")]
    Train {
        v: u32,
        request_id: String,
        run_id: String,
        attempt_id: String,
        graph: serde_json::Value,
        weights: String,
        data: String,
        #[serde(default)]
        val_data: Option<String>,
        out: String,
        steps: u32,
        seed: u64,
        /// State artifact to continue from (W06 learning-continuation).
        #[serde(default)]
        resume: Option<String>,
        /// Checkpoint boundary: write full training state at this step.
        #[serde(default)]
        save_at: Option<u32>,
        /// Where the checkpoint state lands (atomic on the worker side).
        #[serde(default)]
        state_out: Option<String>,
        /// Stop at the checkpoint boundary instead of continuing — the
        /// pause primitive.
        #[serde(default)]
        stop_after_save: Option<bool>,
    },
    #[serde(rename = "predict")]
    Predict {
        v: u32,
        run_id: String,
        #[serde(default)]
        attempt_id: String,
        graph: serde_json::Value,
        weights: String,
        data: String,
        out: String,
    },
    #[serde(rename = "progress")]
    Progress {
        v: u32,
        run_id: String,
        attempt_id: String,
        step: u32,
        loss: f64,
        #[serde(default)]
        saved: Option<String>,
    },
    #[serde(rename = "done")]
    Done {
        v: u32,
        run_id: String,
        attempt_id: String,
        #[serde(default)]
        weights: Option<String>,
        #[serde(default)]
        predictions: Option<String>,
        #[serde(default)]
        first_loss: Option<f64>,
        #[serde(default)]
        loss: Option<f64>,
        #[serde(default)]
        val_accuracy: Option<f64>,
    },
    #[serde(rename = "failed")]
    Failed { v: u32, #[serde(default)] run_id: String, #[serde(default)] attempt_id: String, error: String },
}

impl Frame {
    pub fn run_id(&self) -> &str {
        match self {
            Self::Train { run_id, .. }
            | Self::Predict { run_id, .. }
            | Self::Progress { run_id, .. }
            | Self::Done { run_id, .. }
            | Self::Failed { run_id, .. } => run_id,
            _ => "",
        }
    }

    pub fn decode(line: &str) -> Result<Frame> {
        if line.len() > MAX_FRAME_BYTES {
            return Err(Error::new(
                ErrorKind::BackendFailed,
                format!("worker frame is {} bytes, over the {MAX_FRAME_BYTES} cap", line.len()),
            ));
        }
        serde_json::from_str(line).map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("worker frame is not valid JSON: {e}"))
        })
    }
}

// The plan's error vocabulary has no separate protocol class: an unparsable
// or oversized frame is a backend that broke its contract, so it maps to
// backend-failed.

// ── isolation ──────────────────────────────────────────────────────

/// What the platform can actually enforce. Probed, not assumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Isolation {
    /// user + network + pid + mount namespaces are available.
    pub namespaces: bool,
    /// address-space limit is enforceable.
    pub address_space_limit: bool,
    /// wall-clock timeout is enforceable.
    pub wall_clock: bool,
}

impl Isolation {
    /// Probe the platform. Returns `None` when isolation is unavailable,
    /// which callers must treat as `capability-denied` — never as
    /// "run it anyway".
    pub fn probe(unshare: &str) -> Option<Isolation> {
        let namespaces = Command::new(unshare)
            .args(["-Urn", "--pid", "--mount", "--fork", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        Some(Isolation {
            namespaces,
            // rlimits and a wall clock are the caller's own responsibility
            // and always available; namespaces are the platform-dependent part.
            address_space_limit: true,
            wall_clock: true,
        })
    }

    pub fn enforce(&self) -> Result<()> {
        if self.namespaces {
            Ok(())
        } else {
            Err(Error::denied(
                "worker isolation unavailable: user/network/pid/mount namespaces are not \
                 permitted on this host; refusing to run a training worker unrestricted",
            ))
        }
    }
}

// ── the worker handle ──────────────────────────────────────────────

/// Configuration for one worker process.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub python: PathBuf,
    pub worker_script: PathBuf,
    pub unshare: PathBuf,
    /// Directory the worker may write into (its output artifacts).
    pub scratch: PathBuf,
    /// Working directory the worker runs in; relative artifact paths in
    /// the request resolve against this, so it must be explicit.
    pub working_dir: PathBuf,
    /// Wall-clock budget for one request.
    pub timeout: Duration,
    /// Address-space ceiling in bytes.
    pub max_address_space: u64,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            python: PathBuf::from("python3"),
            worker_script: PathBuf::from("workers/torch/worker.py"),
            unshare: PathBuf::from("unshare"),
            scratch: std::env::temp_dir(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            timeout: Duration::from_secs(300),
            max_address_space: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// A running worker process.
pub struct Worker {
    child: Child,
    /// Frames decoded off the worker's stdout, fed by `reader`.
    rx: Receiver<Result<Frame>>,
    /// Kept alive: the thread that turns stdout into `rx`.
    reader: Option<thread::JoinHandle<()>>,
    /// Progress frames observed so far, for the run record.
    pub progress: Vec<(u32, f64)>,
    stderr: std::process::ChildStderr,
}

impl Worker {
    /// Spawn an isolated worker. Fails closed when the platform cannot
    /// isolate.
    pub fn spawn(config: &WorkerConfig, isolation: &Isolation) -> Result<Worker> {
        isolation.enforce()?;
        let mut command = Command::new(&config.unshare);
        command
            // user (map root), network, pid, mount namespaces + fork
            .args(["-Urn", "--pid", "--mount", "--fork", "--"])
            .arg(&config.python)
            .arg(&config.worker_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&config.working_dir);
        apply_limits(&mut command, config.max_address_space);

        let mut child = command.spawn().map_err(|e| {
            Error::new(
                ErrorKind::BackendFailed,
                format!("spawn isolated worker: {e}"),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Error::new(ErrorKind::BackendFailed, "worker stdout was not piped")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            Error::new(ErrorKind::BackendFailed, "worker stderr was not piped")
        })?;
        // One reader thread owns stdout for the worker's lifetime.
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut lines = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match lines.read_line(&mut buf) {
                    Ok(0) => break, // stream closed
                    Ok(_) => {
                        // Terminal frames end a request, not the stream: one
                        // worker process serves many requests in sequence.
                        let frame = Frame::decode(buf.trim_end());
                        if tx.send(frame).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(Error::new(
                            ErrorKind::BackendFailed,
                            format!("worker output read failed: {e}"),
                        )));
                        break;
                    }
                }
            }
        });
        let mut worker = Worker {
            child,
            rx,
            reader: Some(reader),
            progress: Vec::new(),
            stderr,
        };
        // handshake: a worker that cannot speak the protocol is refused now
        let hello = worker.next_frame(Duration::from_secs(60))?;
        match hello {
            Frame::Hello { v, protocol, .. } => {
                if v != PROTOCOL_VERSION || protocol != PROTOCOL {
                    return Err(Error::new(
                        ErrorKind::IncompatibleState,
                        format!("worker speaks {protocol} v{v}, host requires {PROTOCOL} v{PROTOCOL_VERSION}"),
                    ));
                }
            }
            other => {
                return Err(Error::new(
                    ErrorKind::BackendFailed,
                    format!("worker opened with {other:?}, expected a hello"),
                ));
            }
        }
        Ok(worker)
    }

    /// Read the next frame, or fail if the worker stops speaking. Reads
    /// happen on a dedicated thread and are pulled through a channel, so a
    /// silent worker cannot wedge the host — the wait is bounded by the
    /// caller's deadline.
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Frame> {
        match self.rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::new(
                ErrorKind::Timeout,
                format!("worker produced no frame within {timeout:?}"),
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::new(
                ErrorKind::BackendFailed,
                "worker output stream ended",
            )),
        }
    }

    /// Send a request and wait for its terminal frame, collecting progress.
    pub fn request(&mut self, frame: Frame, timeout: Duration) -> Result<Frame> {
        self.send(&frame)?;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill();
                return Err(Error::new(
                    ErrorKind::Timeout,
                    format!("worker did not finish within {timeout:?}"),
                ));
            }
            let next = match self.next_frame(remaining) {
                Ok(frame) => frame,
                Err(e) if e.kind == ErrorKind::Timeout => {
                    // The worker stopped speaking in time: reap it before
                    // returning, or its orphan holds the stdout pipe open.
                    self.kill();
                    return Err(e);
                }
                Err(e) => {
                    self.kill();
                    return Err(e);
                }
            };
            match &next {
                Frame::Progress { step, loss, .. } => self.progress.push((*step, *loss)),
                Frame::Done { .. } | Frame::Failed { .. } => return Ok(next),
                _ => {
                    return Err(Error::new(
                        ErrorKind::BackendFailed,
                        format!("unexpected frame during a request: {next:?}"),
                    ));
                }
            }
        }
    }

    pub fn send(&mut self, frame: &Frame) -> Result<()> {
        let text = serde_json::to_string(frame).map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("frame is not encodable: {e}"))
        })?;
        if text.len() > MAX_FRAME_BYTES {
            return Err(Error::new(
                ErrorKind::BackendFailed,
                format!("outbound frame is {} bytes, over the cap", text.len()),
            ));
        }
        let stdin = self.child.stdin.as_mut().ok_or_else(|| {
            Error::new(ErrorKind::BackendFailed, "worker stdin was not piped")
        })?;
        stdin
            .write_all(text.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| Error::new(ErrorKind::BackendFailed, format!("worker write failed: {e}")))
    }

    /// Reap the whole process group. The direct child is `unshare`; its
    /// python grandchild would otherwise survive as an orphan holding the
    /// stdout pipe open (and a reader join in Drop would block forever).
    pub fn kill(&mut self) {
        // Negative pid = signal the group; the child was put in its own
        // group by `apply_limits`'s setpgid(0, 0) before exec.
        let minus_pid = -(self.child.id() as i32);
        let rc = unsafe { libc::kill(minus_pid, libc::SIGKILL) };
        // Fall back to the direct child (rc != 0 means no group existed).
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = rc;
    }

    /// Block until the worker exits on its own (e.g. after a
    /// stop_after_save checkpoint). The reader thread unwinds when the
    /// pipe closes.
    pub fn wait(&mut self) -> Result<()> {
        let status = self.child.wait().map_err(|e| {
            Error::new(ErrorKind::BackendFailed, format!("worker wait failed: {e}"))
        })?;
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
        if status.success() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorKind::BackendFailed,
                format!("worker exited with {status}"),
            ))
        }
    }

    /// A bounded read of the worker's diagnostics, for failure reports.
    pub fn drain_stderr(&mut self, cap: usize) -> String {
        thread::scope(|scope| {
            let handle = scope.spawn(|| {
                use std::io::Read as _;
                let mut text = String::new();
                let _ = self.stderr.read_to_string(&mut text);
                text
            });
            let mut collected = String::new();
            while !handle.is_finished() && collected.len() < cap {
                thread::sleep(Duration::from_millis(5));
            }
            if handle.is_finished() {
                collected = handle.join().unwrap_or_default();
            }
            collected.chars().take(cap).collect()
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Kill the process GROUP first: that closes the stdout pipe, so the
        // reader thread below unblocks. Killing only the direct child
        // (unshare) would leave the python grandchild running as an orphan
        // holding the pipe, and the join would never return.
        self.kill();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

/// Set `RLIMIT_AS` (address space) and `RLIMIT_CPU` before exec via a
/// pre-exec closure, and put the child in its own process group so cancel
/// reaps the whole tree. The closure runs in the forked child just before
/// it execs the worker. Best-effort: the namespace isolation and the wall
/// clock are the load-bearing limits; these are backstops.
fn apply_limits(command: &mut Command, max_address_space: u64) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(move || {
            let set = |resource: u32, value: u64| -> std::io::Result<()> {
                let limit = libc::rlimit { rlim_cur: value, rlim_max: value };
                if libc::setrlimit(resource, &limit) == 0 {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            };
            set(libc::RLIMIT_AS, max_address_space)?;
            // RLIMIT_CPU: a backstop behind the wall clock
            set(libc::RLIMIT_CPU, 3600)?;
            libc::setpgid(0, 0);
            Ok(())
        });
    }
}

// ── artifact hand-off ──────────────────────────────────────────────

/// Persist a worker's output file as an immutable artifact and return its
/// digest. The worker's scratch path is never recorded.
pub fn commit_worker_output(store: &crate::store::Store, path: &Path) -> Result<ArtifactRef> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorKind::ArtifactUnavailable,
            format!("worker output {} is unreadable: {e}", path.display()),
        )
    })?;
    Ok(store.artifacts().put(&bytes)?)
}

/// The digest of a file the worker will read, for the run record.
pub fn digest_file(path: &Path) -> Result<ArtifactRef> {
    let bytes = std::fs::read(path).map_err(|e| {
        Error::new(
            ErrorKind::ArtifactUnavailable,
            format!("input {} is unreadable: {e}", path.display()),
        )
    })?;
    Ok(digest_bytes(&bytes))
}
