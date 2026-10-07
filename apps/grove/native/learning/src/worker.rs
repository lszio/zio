//! W04: the controlled training worker.
//!
//! The host never runs a candidate's Python. It runs *this* worker with
//! *this* interpreter and passes a validated operator graph; the graph
//! vocabulary is what bounds what can execute (see
//! `apps/grove/workers/torch/graph.py`).
//!
//! Isolation is real or refused. The worker is started inside a user,
//! network, PID and mount namespace via `unshare`, with a read-only
//! artifact root, a private temp dir, wall-clock and address-space rlimits
//! and a process-group kill on cancel. If the platform cannot provide
//! namespaces, [`Isolation::probe`] reports it and the caller gets a
//! `capability-denied:` — it does **not** fall back to running the
//! worker unrestricted.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::contracts::{ArtifactRef, Error, ErrorKind, Result, digest_bytes};

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
    Hello {
        v: u32,
        protocol: String,
        capabilities: Vec<String>,
        torch: String,
    },
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
    Failed {
        v: u32,
        #[serde(default)]
        run_id: String,
        #[serde(default)]
        attempt_id: String,
        error: String,
    },
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

    pub fn attempt_id(&self) -> &str {
        match self {
            Self::Train { attempt_id, .. }
            | Self::Predict { attempt_id, .. }
            | Self::Progress { attempt_id, .. }
            | Self::Done { attempt_id, .. }
            | Self::Failed { attempt_id, .. } => attempt_id,
            _ => "",
        }
    }

    pub fn decode(line: &str) -> Result<Frame> {
        if line.len() > MAX_FRAME_BYTES {
            return Err(Error::new(
                ErrorKind::BackendFailed,
                format!(
                    "worker frame is {} bytes, over the {MAX_FRAME_BYTES} cap",
                    line.len()
                ),
            ));
        }
        serde_json::from_str(line).map_err(|e| {
            Error::new(
                ErrorKind::BackendFailed,
                format!("worker frame is not valid JSON: {e}"),
            )
        })
    }
}

// The plan's error vocabulary has no separate protocol class: an unparsable
// or oversized frame is a backend that broke its contract, so it maps to
// backend-failed.

/// The outcome of one bounded stdout read.
enum FrameRead {
    Line,
    Eof,
    TooLong(usize),
    Err(std::io::Error),
}

/// Read one newline-terminated frame, refusing before the allocation
/// happens: bytes past `MAX_FRAME_BYTES` are counted and dropped, never
/// buffered. A frame that never terminates is a protocol violation, not
/// an allocation the host has to survive.
fn read_frame_line<R: std::io::Read>(reader: &mut R, out: &mut Vec<u8>) -> FrameRead {
    let mut seen = 0usize;
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                return if out.is_empty() && seen == 0 {
                    FrameRead::Eof
                } else {
                    FrameRead::Line
                };
            }
            Ok(_) => {
                seen += 1;
                if byte[0] == b'\n' {
                    return FrameRead::Line;
                }
                if seen > MAX_FRAME_BYTES {
                    return FrameRead::TooLong(seen);
                }
                out.push(byte[0]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return FrameRead::Err(e),
        }
    }
}

/// The (run, attempt) a request is answered under.
fn identity_of(frame: &Frame) -> (String, String) {
    (frame.run_id().to_string(), frame.attempt_id().to_string())
}

/// A frame that names a different run or attempt than the request it
/// arrived during is refused. A worker's own claim about *whose* result
/// this is is not evidence — the request is.
fn verify_identity(frame: &Frame, run_id: &str, attempt_id: &str) -> Result<()> {
    let seen_run = frame.run_id();
    if !run_id.is_empty() && !seen_run.is_empty() && seen_run != run_id {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            format!("worker answered run {seen_run:?} while run {run_id:?} was requested"),
        ));
    }
    let seen_attempt = frame.attempt_id();
    if !attempt_id.is_empty() && !seen_attempt.is_empty() && seen_attempt != attempt_id {
        return Err(Error::new(
            ErrorKind::ProtocolViolation,
            format!(
                "worker answered attempt {seen_attempt:?} while attempt {attempt_id:?} was requested; \
                 a foreign attempt's result is never committed"
            ),
        ));
    }
    Ok(())
}

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
    /// Seconds the worker has to open the protocol. A jailed worker
    /// assembles a mount root and imports its backend first, so this is
    /// larger than an unjailed worker's startup.
    pub handshake_timeout: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            python: PathBuf::from("python3"),
            worker_script: PathBuf::from("apps/grove/workers/torch/worker.py"),
            unshare: PathBuf::from("unshare"),
            scratch: std::env::temp_dir(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            timeout: Duration::from_secs(300),
            max_address_space: 2 * 1024 * 1024 * 1024,
            handshake_timeout: Duration::from_secs(60),
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
        Self::start(command, config.handshake_timeout)
    }

    /// Spawn a worker inside an explicit G00 profile: a minimal mount
    /// root, one writable scratch, and privileges dropped before exec.
    /// The profile is validated first — a profile that cannot be
    /// enforced never reaches a process.
    pub fn spawn_under(
        config: &WorkerConfig,
        isolation: &Isolation,
        profile: &crate::isolation::IsolationProfile,
    ) -> Result<Worker> {
        isolation.enforce()?;
        profile.validate()?;
        // The worker's script is mounted at a jail-local path; the host
        // path it was configured with is not part of the worker's root.
        let script = profile
            .read_only
            .declared_worker_script()
            .ok_or_else(|| Error::denied("the profile does not declare the worker script"))?;
        let shell = crate::isolation::worker_shell(profile, &config.python, &script)?;
        let mut command = Command::new(&config.unshare);
        command
            .args(["-Urnm", "--pid", "--fork", "--"])
            .arg("/bin/sh")
            .arg("-c")
            .arg(shell)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_limits(&mut command, config.max_address_space);
        Self::start(command, config.handshake_timeout)
    }

    fn start(mut command: Command, handshake_timeout: Duration) -> Result<Worker> {
        let mut child = command.spawn().map_err(|e| {
            Error::new(
                ErrorKind::BackendFailed,
                format!("spawn isolated worker: {e}"),
            )
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::new(ErrorKind::BackendFailed, "worker stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::new(ErrorKind::BackendFailed, "worker stderr was not piped"))?;
        // One reader thread owns stdout for the worker's lifetime.
        let (tx, rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            // Bounded reads: `read_line` grows its buffer until it sees a
            // newline, so a worker that emits one gigabyte without one
            // would allocate it all. `read_until` with a pre-sized buffer
            // and an explicit cap turns that into a protocol error.
            let mut raw = Vec::with_capacity(MAX_FRAME_BYTES);
            let mut lines = BufReader::new(stdout);
            loop {
                raw.clear();
                match read_frame_line(&mut lines, &mut raw) {
                    FrameRead::Eof => break,
                    FrameRead::TooLong(n) => {
                        let _ = tx.send(Err(Error::new(
                            ErrorKind::ProtocolViolation,
                            format!(
                                "worker frame is {n} bytes before a newline, over the {MAX_FRAME_BYTES} cap"
                            ),
                        )));
                        break;
                    }
                    FrameRead::Err(e) => {
                        let _ = tx.send(Err(Error::new(
                            ErrorKind::BackendFailed,
                            format!("worker output read failed: {e}"),
                        )));
                        break;
                    }
                    FrameRead::Line => {
                        // Terminal frames end a request, not the stream: one
                        // worker process serves many requests in sequence.
                        let Ok(text) = std::str::from_utf8(&raw) else {
                            let _ = tx.send(Err(Error::new(
                                ErrorKind::ProtocolViolation,
                                "worker frame is not valid UTF-8",
                            )));
                            break;
                        };
                        let frame = Frame::decode(text.trim_end());
                        if tx.send(frame).is_err() {
                            break;
                        }
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
        let hello = worker.next_frame(handshake_timeout)?;
        match hello {
            Frame::Hello { v, protocol, .. } => {
                if v != PROTOCOL_VERSION || protocol != PROTOCOL {
                    return Err(Error::new(
                        ErrorKind::IncompatibleState,
                        format!(
                            "worker speaks {protocol} v{v}, host requires {PROTOCOL} v{PROTOCOL_VERSION}"
                        ),
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
    /// Override the handshake budget. A jailed worker assembles a mount
    /// root and imports torch before it can say hello, so the default 60s
    /// is tight for that path.
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
    ///
    /// Every frame the worker produces is checked against the identity of
    /// the request it is answering. A worker that answers a *different*
    /// run or attempt — a leftover from a cancelled request, a stale
    /// process after a restart — is a protocol violation, not a result:
    /// its bytes never reach the ledger.
    pub fn request(&mut self, frame: Frame, timeout: Duration) -> Result<Frame> {
        let (run_id, attempt_id) = identity_of(&frame);
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
            verify_identity(&next, &run_id, &attempt_id)?;
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
            Error::new(
                ErrorKind::BackendFailed,
                format!("frame is not encodable: {e}"),
            )
        })?;
        if text.len() > MAX_FRAME_BYTES {
            return Err(Error::new(
                ErrorKind::BackendFailed,
                format!("outbound frame is {} bytes, over the cap", text.len()),
            ));
        }
        let stdin =
            self.child.stdin.as_mut().ok_or_else(|| {
                Error::new(ErrorKind::BackendFailed, "worker stdin was not piped")
            })?;
        stdin
            .write_all(text.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| {
                Error::new(
                    ErrorKind::BackendFailed,
                    format!("worker write failed: {e}"),
                )
            })
    }

    /// Reap the whole process group. The direct child is `unshare`; its
    /// python grandchild would otherwise survive as an orphan holding the
    /// stdout pipe open, and the reader join in Drop would block forever.
    ///
    /// Closing the read end first is what actually unblocks the reader:
    /// the signal is asynchronous, so a join that runs immediately after
    /// it can still see an open pipe and wait for a writer that is on its
    /// way out.
    pub fn kill(&mut self) {
        // Negative pid = signal the group; the child was put in its own
        // group by `apply_limits`'s setpgid(0, 0) before exec.
        let minus_pid = -(self.child.id() as i32);
        let rc = unsafe { libc::kill(minus_pid, libc::SIGKILL) };
        // Fall back to the direct child (rc != 0 means no group existed).
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Drop our read end so the reader thread sees EOF even if a
        // grandchild in another namespace still holds the write end.
        drop(self.child.stdout.take());
        // The reader thread must not be able to block a shutdown. A
        // jailed worker's python grandchild lives in its own PID
        // namespace, where a signal from here does not reach it: the
        // unshare parent dies, the pipe write end survives on the orphan,
        // and an unbounded join waits forever. Bounding the join is the
        // only way out; the process group is dead either way.
        self.join_reader_bounded();
        let _ = rc;
    }

    /// Join the stdout reader, giving up after a short grace period.
    fn join_reader_bounded(&mut self) {
        let Some(handle) = self.reader.take() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let joined = std::thread::spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
        if rx.recv_timeout(Duration::from_millis(500)).is_err() {
            // The thread is parked on a pipe an orphan holds; detach it
            // rather than block the host on a dead namespace.
            std::mem::forget(joined);
        }
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
    ///
    /// Strictly bounded: the wait ends on the *cap* or the *deadline*,
    /// whichever comes first. The previous shape joined a reader that
    /// blocks until EOF, so a worker still alive with its pipe open made
    /// a failure report block forever — the report is the last thing a
    /// host can afford to wait on.
    pub fn drain_stderr(&mut self, cap: usize) -> String {
        use std::io::Read as _;
        let mut collected = String::new();
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut chunk = [0u8; 4096];
        while collected.len() < cap && Instant::now() < deadline {
            match self.stderr.read(&mut chunk) {
                Ok(0) => break, // the worker closed its stderr
                Ok(n) => collected.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        collected.chars().take(cap).collect()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // `kill` already signals the process group, drops the read end,
        // and bounds the reader join. Re-joining here would reintroduce
        // the unbounded wait.
        self.kill();
    }
}

/// Contract-test surface for the bounded reader. Same code path the
/// reader thread uses; exposed so a test can drive it without spawning a
/// worker that misbehaves.
#[derive(Debug)]
pub enum FrameReadProbe {
    Line,
    Eof,
    TooLong(usize),
    Err(std::io::Error),
}

pub fn probe_frame_read<R: std::io::Read>(reader: &mut R, out: &mut Vec<u8>) -> FrameReadProbe {
    match read_frame_line(reader, out) {
        FrameRead::Line => FrameReadProbe::Line,
        FrameRead::Eof => FrameReadProbe::Eof,
        FrameRead::TooLong(n) => FrameReadProbe::TooLong(n),
        FrameRead::Err(e) => FrameReadProbe::Err(e),
    }
}

/// Contract-test surface for the receipt identity check.
pub fn probe_identity(frame: &Frame, run_id: &str, attempt_id: &str) -> Result<()> {
    verify_identity(frame, run_id, attempt_id)
}

pub fn probe_identity_of(frame: &Frame) -> (String, String) {
    identity_of(frame)
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
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
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
    store.artifacts().put(&bytes)
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
