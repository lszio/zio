use super::{
    backend, framing,
    isolation::{self, ReadOnlyMount},
};
use crate::{HostPolicy, values};
use parking_lot::Mutex;
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use zio_core::error::EvalError;
use zio_core::value::Value;

#[derive(Clone, Debug)]
pub struct ProcessConfig {
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub scratch: Option<PathBuf>,
    pub mounts: Vec<ReadOnlyMount>,
    pub timeout: Duration,
    pub write_timeout: Duration,
    pub max_frame_bytes: usize,
    pub max_output_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_address_space: u64,
    pub max_cpu_seconds: u64,
    pub max_processes: u64,
    pub content_length: bool,
    pub identity: BTreeMap<String, Json>,
}

fn invalid(message: impl AsRef<str>) -> EvalError {
    EvalError::custom(format!("invalid-input: {}", message.as_ref()))
}
fn bound(config: &Json, key: &str, default: u64, maximum: u64) -> Result<u64, EvalError> {
    let number = config.get(key).map_or(Ok(default), |value| {
        value
            .as_u64()
            .ok_or_else(|| invalid(format!("{key} must be a nonnegative integer")))
    })?;
    if number == 0 || number > maximum {
        return Err(invalid(format!("{key} must be between 1 and {maximum}")));
    }
    Ok(number)
}
fn text(config: &Json, key: &str) -> Result<String, EvalError> {
    let text = config
        .get(key)
        .and_then(Json::as_str)
        .ok_or_else(|| invalid(format!("missing string {key}")))?;
    if text.is_empty() || text.contains('\0') {
        return Err(invalid(format!("invalid {key}")));
    }
    Ok(text.into())
}

impl ProcessConfig {
    pub fn from_value(value: &Value, policy: &HostPolicy) -> Result<Self, EvalError> {
        Self::from_json(values::to_json(value)?, policy)
    }
    pub fn from_json(config: Json, policy: &HostPolicy) -> Result<Self, EvalError> {
        if !config.is_object() {
            return Err(invalid("process config must be a map"));
        }
        let executable = PathBuf::from(text(&config, "executable")?);
        if !isolation::absolute(&executable) {
            return Err(invalid("executable must be normalized absolute path"));
        }
        let mut argv = Vec::new();
        if let Some(arguments) = config.get("argv") {
            for value in arguments
                .as_array()
                .ok_or_else(|| invalid("argv must be a sequence"))?
            {
                let argument = value
                    .as_str()
                    .ok_or_else(|| invalid("argv elements must be strings"))?;
                if argument.contains('\0') {
                    return Err(invalid("argv contains NUL"));
                }
                argv.push(argument.into());
            }
        }
        if argv.len() > 4096 || argv.iter().map(String::len).sum::<usize>() > 1 << 20 {
            return Err(invalid("argv exceeds limit"));
        }
        let mut environment: BTreeMap<String, String> = BTreeMap::new();
        if let Some(env) = config.get("environment") {
            for (key, value) in env
                .as_object()
                .ok_or_else(|| invalid("environment must be a string map"))?
            {
                if !policy.environment.contains(key) {
                    return Err(EvalError::custom(format!(
                        "capability-denied: environment {key}"
                    )));
                }
                if key.is_empty() || key.contains(['=', '\0']) {
                    return Err(invalid("invalid environment name"));
                }
                let value = value
                    .as_str()
                    .ok_or_else(|| invalid("environment values must be strings"))?;
                if value.contains('\0') {
                    return Err(invalid("environment contains NUL"));
                }
                environment.insert(key.clone(), value.into());
            }
        }
        if environment.len() > 256
            || environment
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
                > 1 << 20
        {
            return Err(invalid("environment exceeds limit"));
        }
        let scratch = config
            .get("scratch")
            .map(|_| {
                text(&config, "scratch").and_then(|path| {
                    if !isolation::absolute(std::path::Path::new(&path)) {
                        return Err(invalid("scratch must be normalized absolute path"));
                    }
                    policy.write_path(path)
                })
            })
            .transpose()?;
        let cwd = config
            .get("cwd")
            .map_or(Ok(PathBuf::from("/scratch")), |_| {
                text(&config, "cwd").map(PathBuf::from)
            })?;
        if !isolation::absolute(&cwd) {
            return Err(invalid("cwd must be normalized absolute path"));
        }
        let mut mounts = Vec::new();
        if let Some(entries) = config.get("read-only-mounts") {
            for entry in entries
                .as_array()
                .ok_or_else(|| invalid("read-only-mounts must be a sequence"))?
            {
                let source = policy.read_path(text(entry, "source")?)?;
                let target = PathBuf::from(text(entry, "target")?);
                if !isolation::absolute(&target) {
                    return Err(invalid("mount target must be normalized absolute path"));
                }
                mounts.push(ReadOnlyMount { source, target });
            }
        }
        if mounts.len() > 128 {
            return Err(invalid("mount count exceeds limit"));
        }
        let content_length = match config
            .get("framing")
            .and_then(Json::as_str)
            .unwrap_or("ndjson")
        {
            "ndjson" => false,
            "content-length" => true,
            _ => return Err(invalid("framing must be ndjson or content-length")),
        };
        let mut identity = BTreeMap::new();
        if let Some(fields) = config.get("identity") {
            for (field, expected) in fields
                .as_object()
                .ok_or_else(|| invalid("identity must be an object"))?
            {
                if field.is_empty()
                    || expected.is_null()
                    || expected
                        .as_str()
                        .is_some_and(|value| value.trim().is_empty())
                    || !(expected.is_string() || expected.is_number())
                {
                    return Err(invalid(
                        "identity fields must have nonblank string or numeric values",
                    ));
                }
                identity.insert(field.clone(), expected.clone());
            }
            if identity.is_empty() {
                return Err(invalid("explicit identity must not be empty"));
            }
        }
        Ok(Self {
            executable,
            argv,
            environment,
            cwd,
            scratch,
            mounts,
            identity,
            content_length,
            timeout: Duration::from_millis(bound(&config, "timeout-ms", 300000, 86400000)?),
            write_timeout: Duration::from_millis(bound(&config, "write-timeout-ms", 5000, 60000)?),
            max_frame_bytes: bound(&config, "max-frame-bytes", 1 << 20, 16 << 20)? as usize,
            max_output_bytes: bound(&config, "max-output-bytes", 16 << 20, 1 << 30)? as usize,
            max_stderr_bytes: bound(&config, "max-stderr-bytes", 64 << 10, 16 << 20)? as usize,
            max_address_space: bound(&config, "max-address-space", 2 << 30, 1 << 46)?,
            max_cpu_seconds: bound(&config, "max-cpu-seconds", 300, 86400)?,
            max_processes: bound(&config, "max-processes", 64, 4096)?,
        })
    }
}

#[derive(Debug)]
pub enum ProcessRead {
    Frame(Json),
    Timeout,
    Eof,
}
#[derive(Clone, Debug)]
pub struct ProcessExit {
    pub success: bool,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub stderr: Vec<u8>,
}
struct Life {
    child: Child,
    exit: Option<ProcessExit>,
    failure: Option<(String, String)>,
    cancelled: bool,
    stderr: Vec<u8>,
}

pub struct Process {
    life: Arc<Mutex<Life>>,
    monitor: Option<JoinHandle<()>>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    config: ProcessConfig,
    buffered: Vec<u8>,
    output_bytes: usize,
    eof: bool,
    stage: Option<PathBuf>,
}

pub(crate) fn nonblocking(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
pub(crate) fn poll_fd(fd: RawFd, events: i16, timeout: Duration) -> std::io::Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Ok(result > 0
            && descriptor.revents & (events | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0);
    }
}
fn signal(life: &mut Life) {
    if life.exit.is_some() {
        return;
    }
    unsafe {
        libc::kill(-(life.child.id() as i32), libc::SIGKILL);
    }
    let _ = life.child.kill();
}

impl Process {
    pub fn start(config: &ProcessConfig, policy: &HostPolicy) -> Result<Self, EvalError> {
        if !policy.process {
            return Err(EvalError::custom("capability-denied: process"));
        }
        let (command, stage) = isolation::command(config, policy)?;
        let mut process = Self::launch(command, config, Some(stage))?;
        // This marker is emitted only after pivot_root and setpriv succeeded.
        // Always NDJSON for the bootstrap marker, even for JSON-RPC children.
        let framing = process.config.content_length;
        process.config.content_length = false;
        let ready = process.read(Duration::from_secs(15).min(config.timeout));
        process.config.content_length = framing;
        match ready {
            Ok(ProcessRead::Frame(frame))
                if frame == serde_json::json!({"__zio_jail_ready":true}) =>
            {
                Ok(process)
            }
            other => {
                let exit = process.kill()?;
                Err(EvalError::custom(format!(
                    "capability-denied: isolated root could not start: {other:?}; {}",
                    String::from_utf8_lossy(&exit.stderr)
                )))
            }
        }
    }

    /// Only the tensor installer may call this with its authenticated packaged
    /// backend. Source process-start has no route to this entry point.
    pub(crate) fn start_trusted_backend(
        config: &ProcessConfig,
        policy: &HostPolicy,
    ) -> Result<Self, EvalError> {
        if !policy.process || !policy.tensor {
            return Err(EvalError::custom(
                "capability-denied: trusted tensor process",
            ));
        }
        let executable = policy.read_path(&config.executable)?;
        let cwd = policy.read_path(&config.cwd)?;
        if !executable.is_file() || !cwd.is_dir() {
            return Err(invalid("trusted executable/cwd are unavailable"));
        }
        let mut command = Command::new(executable);
        command
            .args(&config.argv)
            .current_dir(cwd)
            .env_clear()
            .envs(&config.environment);
        Self::launch(command, config, None)
    }

    fn launch(
        mut command: Command,
        config: &ProcessConfig,
        stage: Option<PathBuf>,
    ) -> Result<Self, EvalError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stage.is_some() {
            command.env_clear().env("PATH", "/usr/bin:/bin");
        }
        let address = config.max_address_space;
        let cpu = config.max_cpu_seconds;
        let processes = config.max_processes;
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                for (kind, limit) in [
                    (libc::RLIMIT_AS, address),
                    (libc::RLIMIT_CPU, cpu),
                    (libc::RLIMIT_NPROC, processes),
                    (libc::RLIMIT_NOFILE, 256),
                    (libc::RLIMIT_CORE, 0),
                ] {
                    let limit = libc::rlimit {
                        rlim_cur: limit as libc::rlim_t,
                        rlim_max: limit as libc::rlim_t,
                    };
                    if libc::setrlimit(kind, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                // CLOEXEC keeps Rust's spawn error pipe until exec while closing
                // every unrelated inherited capability at the exec boundary.
                if libc::syscall(
                    libc::SYS_close_range,
                    3u32,
                    u32::MAX,
                    libc::CLOSE_RANGE_CLOEXEC,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                if let Some(stage) = stage {
                    let _ = std::fs::remove_dir_all(stage);
                }
                return Err(backend(error));
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| backend("missing process stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| backend("missing process stdout"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| backend("missing process stderr"))?;
        if let Err(error) = nonblocking(stdin.as_raw_fd())
            .and_then(|_| nonblocking(stdout.as_raw_fd()))
            .and_then(|_| nonblocking(stderr.as_raw_fd()))
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(backend(error));
        }
        let life = Arc::new(Mutex::new(Life {
            child,
            exit: None,
            failure: None,
            cancelled: false,
            stderr: Vec::new(),
        }));
        let monitor_life = life.clone();
        let deadline = Instant::now() + config.timeout;
        let cap = config.max_stderr_bytes;
        let monitor = thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            loop {
                let mut life = monitor_life.lock();
                loop {
                    match stderr.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            if life.stderr.len().saturating_add(n) > cap {
                                let remaining = cap.saturating_sub(life.stderr.len());
                                life.stderr.extend_from_slice(&chunk[..remaining]);
                                life.failure = Some((
                                    "protocol-violation".into(),
                                    "stderr exceeds byte limit".into(),
                                ));
                                signal(&mut life);
                                break;
                            }
                            life.stderr.extend_from_slice(&chunk[..n]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error) => {
                            life.failure =
                                Some(("backend-failed".into(), format!("stderr: {error}")));
                            signal(&mut life);
                            break;
                        }
                    }
                }
                if life.cancelled {
                    signal(&mut life);
                }
                if Instant::now() >= deadline && life.failure.is_none() && !life.cancelled {
                    life.failure = Some((
                        "timeout".into(),
                        "process exceeded lifetime deadline".into(),
                    ));
                    signal(&mut life);
                }
                match life.child.try_wait() {
                    Ok(Some(status)) => {
                        life.exit = Some(ProcessExit {
                            success: status.success(),
                            code: status.code(),
                            signal: status.signal(),
                            stderr: life.stderr.clone(),
                        });
                        break;
                    }
                    Ok(None) => (),
                    Err(error) => {
                        life.failure = Some(("backend-failed".into(), format!("reap: {error}")));
                        signal(&mut life);
                    }
                }
                drop(life);
                thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(Self {
            life,
            monitor: Some(monitor),
            stdin: Some(stdin),
            stdout: Some(stdout),
            config: config.clone(),
            buffered: Vec::new(),
            output_bytes: 0,
            eof: false,
            stage,
        })
    }

    fn failure(&self) -> Result<(), EvalError> {
        if let Some((class, message)) = &self.life.lock().failure {
            Err(EvalError::custom(format!("{class}: {message}")))
        } else {
            Ok(())
        }
    }
    fn verify(&self, frame: &Json) -> Result<(), EvalError> {
        if !frame.is_object() {
            return Err(EvalError::custom(
                "protocol-violation: process frame must be an object",
            ));
        }
        for (field, expected) in &self.config.identity {
            if frame.get(field) != Some(expected) {
                return Err(EvalError::custom(format!(
                    "protocol-violation: missing, blank or foreign frame identity {field}"
                )));
            }
        }
        Ok(())
    }
    fn next_buffered(&mut self) -> Result<Option<Json>, EvalError> {
        let (start, end, consumed) = if self.config.content_length {
            let Some(headers_end) = self
                .buffered
                .windows(4)
                .position(|bytes| bytes == b"\r\n\r\n")
                .map(|index| index + 4)
            else {
                if self.buffered.len() > 16384 {
                    return Err(EvalError::custom(
                        "protocol-violation: frame headers exceed byte limit",
                    ));
                }
                return Ok(None);
            };
            if headers_end > 16384 {
                return Err(EvalError::custom(
                    "protocol-violation: frame headers exceed byte limit",
                ));
            }
            let length =
                framing::content_length(&self.buffered[..headers_end], self.config.max_frame_bytes)
                    .map_err(|error| EvalError::custom(format!("protocol-violation: {error}")))?;
            if self.buffered.len() < headers_end + length {
                return Ok(None);
            }
            (headers_end, headers_end + length, headers_end + length)
        } else {
            let Some(newline) = self.buffered.iter().position(|byte| *byte == b'\n') else {
                if self.buffered.len() > self.config.max_frame_bytes {
                    return Err(EvalError::custom(
                        "protocol-violation: NDJSON frame exceeds byte limit",
                    ));
                }
                return Ok(None);
            };
            if newline > self.config.max_frame_bytes {
                return Err(EvalError::custom(
                    "protocol-violation: NDJSON frame exceeds byte limit",
                ));
            }
            (0, newline, newline + 1)
        };
        let frame = serde_json::from_slice(&self.buffered[start..end]).map_err(|error| {
            EvalError::custom(format!("protocol-violation: invalid JSON frame: {error}"))
        })?;
        self.verify(&frame)?;
        if self.config.content_length {
            framing::validate_rpc(&frame)
                .map_err(|error| EvalError::custom(format!("protocol-violation: {error}")))?;
        }
        self.buffered.drain(..consumed);
        Ok(Some(frame))
    }
    pub fn read(&mut self, timeout: Duration) -> Result<ProcessRead, EvalError> {
        let result = self.read_inner(timeout);
        if result.is_err() {
            let _ = self.kill();
        }
        result
    }
    fn read_inner(&mut self, timeout: Duration) -> Result<ProcessRead, EvalError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.failure()?;
            if let Some(frame) = self.next_buffered()? {
                return Ok(ProcessRead::Frame(frame));
            }
            if self.eof {
                if !self.buffered.is_empty() {
                    return Err(EvalError::custom(
                        "protocol-violation: truncated process frame at EOF",
                    ));
                }
                let life = self.life.lock();
                if life.exit.as_ref().is_some_and(|exit| !exit.success) {
                    return Err(backend(format!(
                        "process exited unsuccessfully: {:?}",
                        life.exit
                    )));
                }
                return Ok(ProcessRead::Eof);
            }
            let mut chunk = [0u8; 8192];
            let stdout = self
                .stdout
                .as_mut()
                .ok_or_else(|| backend("process stdout is closed"))?;
            match stdout.read(&mut chunk) {
                Ok(0) => {
                    self.eof = true;
                    continue;
                }
                Ok(n) => {
                    self.output_bytes = self.output_bytes.saturating_add(n);
                    if self.output_bytes > self.config.max_output_bytes {
                        return Err(EvalError::custom(
                            "protocol-violation: stdout exceeds byte limit",
                        ));
                    }
                    self.buffered.extend_from_slice(&chunk[..n]);
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(error) => return Err(backend(error)),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(ProcessRead::Timeout);
            }
            poll_fd(
                stdout.as_raw_fd(),
                libc::POLLIN,
                remaining.min(Duration::from_millis(10)),
            )
            .map_err(backend)?;
        }
    }
    pub fn readable(&self) -> Result<bool, EvalError> {
        self.failure()?;
        Ok(!self.buffered.is_empty()
            || self.eof
            || self
                .stdout
                .as_ref()
                .map_or(Ok(true), |out| {
                    poll_fd(out.as_raw_fd(), libc::POLLIN, Duration::ZERO)
                })
                .map_err(backend)?)
    }
    pub fn write(&mut self, frame: &Json) -> Result<(), EvalError> {
        self.failure()?;
        self.verify(frame)?;
        let mut wire = Vec::new();
        if self.config.content_length {
            framing::write_content_length(&mut wire, frame, self.config.max_frame_bytes)
                .map_err(|error| EvalError::custom(format!("protocol-violation: {error}")))?;
        } else {
            wire = serde_json::to_vec(frame).map_err(backend)?;
            if wire.len() > self.config.max_frame_bytes {
                return Err(EvalError::custom(
                    "protocol-violation: outbound frame exceeds byte limit",
                ));
            }
            wire.push(b'\n');
        }
        let deadline = Instant::now() + self.config.write_timeout;
        let mut position = 0;
        while position < wire.len() {
            self.failure()?;
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| backend("process stdin is closed"))?;
            match stdin.write(&wire[position..]) {
                Ok(0) => {
                    let _ = self.kill();
                    return Err(backend("process stdin returned zero write"));
                }
                Ok(n) => position += n,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        let _ = self.kill();
                        return Err(EvalError::custom("timeout: process write deadline"));
                    }
                    poll_fd(
                        stdin.as_raw_fd(),
                        libc::POLLOUT,
                        remaining.min(Duration::from_millis(10)),
                    )
                    .map_err(backend)?;
                }
                Err(error) => {
                    let _ = self.kill();
                    return Err(backend(error));
                }
            }
        }
        Ok(())
    }
    pub fn wait(&mut self, timeout: Duration) -> Result<Option<ProcessExit>, EvalError> {
        let deadline = Instant::now() + timeout;
        loop {
            self.failure()?;
            if let Some(exit) = self.life.lock().exit.clone() {
                return Ok(Some(exit));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(5)),
            );
        }
    }
    pub fn kill(&mut self) -> Result<ProcessExit, EvalError> {
        self.stdin.take();
        self.stdout.take();
        self.eof = true;
        {
            let mut life = self.life.lock();
            life.cancelled = true;
            signal(&mut life);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(exit) = self.life.lock().exit.clone() {
                if let Some(monitor) = self.monitor.take() {
                    let _ = monitor.join();
                }
                if let Some(stage) = self.stage.take() {
                    std::fs::remove_dir_all(stage).map_err(backend)?;
                }
                return Ok(exit);
            }
            if Instant::now() >= deadline {
                return Err(backend(
                    "killed process could not be reaped within deadline",
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
