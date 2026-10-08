//! Bounded NDJSON and Content-Length framing over the host's own stdin/stdout.
//!
//! The reader thread (or socket thread) of a worker or an ACP server is
//! literally the host process; this module owns the buffering for that
//! stream so the eval side never has to touch a raw `Read`/`Write`.
//! NDJSON framing is used by workers; Content-Length framing is used by
//! the language server and any JSON-RPC peer. A truncated frame is an
//! error; clean EOF is a status. Both distinguish themselves so callers
//! can decide whether to retry, reconnect or shut down.

use std::io::{self, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::{LazyLock, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value as Json;

use super::framing;

/// Frame byte caps enforced on every host-side stdio and rpc read.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Caps {
    pub ndjson_line: usize,
    pub rpc_headers: usize,
    pub rpc_body: usize,
}

impl Default for Caps {
    fn default() -> Self {
        // Defaults mirror ProcessConfig: 1 MiB JSON line, 16 KiB headers,
        // 1 MiB body. Anything larger than these is overwhelmingly a bug.
        Self {
            ndjson_line: 1 << 20,
            rpc_headers: 16 * 1024,
            rpc_body: 1 << 20,
        }
    }
}

static CAPS: OnceLock<Caps> = OnceLock::new();

pub(crate) fn configure(caps: Caps) {
    let _ = CAPS.set(caps);
}

fn current() -> Caps {
    CAPS.get().copied().unwrap_or_default()
}

struct State {
    reader: BufReader<std::io::Stdin>,
    eof: bool,
}

impl State {
    fn new() -> Self {
        let stdin = std::io::stdin();
        // Non-blocking lets a read give up on the deadline without spawning
        // a thread for the host. A single eval thread serialises stdin.
        if let Err(error) = nonblocking(stdin.as_raw_fd()) {
            // If O_NONBLOCK can't be set (rare; would require a non-Unix
            // build), stdio read falls back to blocking and the deadline
            // is enforced only by the wait above it.
            let _ = error;
        }
        Self {
            reader: BufReader::with_capacity(8 * 1024, stdin),
            eof: false,
        }
    }
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::new()));

fn state() -> &'static Mutex<State> {
    &*STATE
}

fn nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn poll_fd(fd: std::os::fd::RawFd, events: i16, timeout: Duration) -> std::io::Result<bool> {
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
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Ok(result > 0
            && descriptor.revents & (events | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                != 0);
    }
}

/// Outcome of a single read attempt; matches the wire envelope callers expect.
pub(crate) enum Frame {
    Value(Json),
    Timeout,
    Eof,
}

fn bad(message: impl AsRef<str>) -> zio_core::error::EvalError {
    zio_core::error::EvalError::custom(format!("protocol-violation: stdio: {}", message.as_ref()))
}

fn into_io(error: io::Error) -> zio_core::error::EvalError {
    zio_core::error::EvalError::custom(format!("backend-failed: stdio: {error}"))
}

/// Bounded NDJSON frame read over the host's stdin.
///
/// - clean EOF before any byte: `Frame::Eof`
/// - clean EOF after a partial line: `protocol-violation` (truncated frame)
/// - line longer than the cap: `protocol-violation`
pub(crate) fn read_ndjson(timeout: Duration) -> Result<Frame, zio_core::error::EvalError> {
    let caps = current();
    let mut state = state().lock();
    if state.eof {
        return Ok(Frame::Eof);
    }
    let stdin_fd = std::io::stdin().as_raw_fd();
    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    loop {
        let mut chunk_bytes = [0u8; 8192];
        match state.reader.read(&mut chunk_bytes) {
            Ok(0) => {
                state.eof = true;
                if buffer.is_empty() {
                    return Ok(Frame::Eof);
                }
                return Err(bad("truncated NDJSON frame at EOF"));
            }
            Ok(n) => {
                if let Some(newline) = chunk_bytes[..n].iter().position(|byte| *byte == b'\n') {
                    if buffer.len() + newline + 1 > caps.ndjson_line {
                        return Err(bad("NDJSON frame exceeds byte limit"));
                    }
                    buffer.extend_from_slice(&chunk_bytes[..=newline]);
                    let value = serde_json::from_slice(&buffer)
                        .map_err(|error| bad(format!("invalid NDJSON frame: {error}")))?;
                    return Ok(Frame::Value(value));
                }
                if buffer.len() + n > caps.ndjson_line {
                    return Err(bad("NDJSON frame exceeds byte limit"));
                }
                buffer.extend_from_slice(&chunk_bytes[..n]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(into_io(error)),
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(Frame::Timeout);
        }
        if !poll_fd(
            stdin_fd,
            libc::POLLIN,
            remaining.min(Duration::from_millis(10)),
        )
        .map_err(into_io)?
        {
            return Ok(Frame::Timeout);
        }
    }
}

/// Content-Length framed read; delegates to `framing::read_content_length`
/// so the wire format matches every other JSON-RPC peer in the project.
pub(crate) fn read_rpc(timeout: Duration) -> Result<Frame, zio_core::error::EvalError> {
    let caps = current();
    let mut state = state().lock();
    if state.eof {
        return Ok(Frame::Eof);
    }
    let stdin_fd = std::io::stdin().as_raw_fd();
    let deadline = Instant::now() + timeout;

    // Wait for the header before reading: `read_content_length` on a
    // pipe blocks until bytes arrive, so a timeout that is only checked
    // after the read is a timeout that never fires. Polling first is
    // what makes "no frame yet" a real answer rather than a hang.
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(Frame::Timeout);
        }
        if poll_fd(
            stdin_fd,
            libc::POLLIN,
            remaining.min(Duration::from_millis(50)),
        )
        .map_err(into_io)?
        {
            break;
        }
    }

    match framing::read_content_length(&mut state.reader, caps.rpc_headers, caps.rpc_body) {
        Ok(Some(value)) => Ok(Frame::Value(value)),
        // A clean end of stream is not a frame and not an error; the
        // caller decides what a closed peer means.
        Ok(None) => {
            state.eof = true;
            Ok(Frame::Eof)
        }
        Err(error) => Err(into_io(error)),
    }
}

pub(crate) fn write_ndjson(value: &Json) -> Result<(), zio_core::error::EvalError> {
    let caps = current();
    let bytes = serde_json::to_vec(value).map_err(|error| bad(format!("serialize: {error}")))?;
    if bytes.len() + 1 > caps.ndjson_line {
        return Err(bad("NDJSON frame exceeds byte limit"));
    }
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    lock.write_all(&bytes).map_err(into_io)?;
    lock.write_all(b"\n").map_err(into_io)?;
    lock.flush().map_err(into_io)?;
    Ok(())
}

pub(crate) fn write_rpc(value: &Json) -> Result<(), zio_core::error::EvalError> {
    let caps = current();
    let mut stdout = std::io::stdout().lock();
    framing::write_content_length(&mut stdout, value, caps.rpc_body).map_err(into_io)
}

#[allow(dead_code)]
pub(crate) fn flush() -> io::Result<()> {
    std::io::stdout().flush()
}
