use super::{backend, invalid};
use serde_json::Value as Json;
use std::collections::BTreeMap;
use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use zio_core::error::EvalError;

#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub address: SocketAddr,
    pub max_body: usize,
    pub max_headers: usize,
    pub timeout: Duration,
    pub max_queue: usize,
}
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
#[derive(Debug)]
pub struct HttpRequest {
    pub id: i64,
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
struct Incoming {
    request: HttpRequest,
    reply: oneshot::Sender<HttpResponse>,
    deadline: Instant,
}
struct Pending {
    reply: oneshot::Sender<HttpResponse>,
    deadline: Instant,
}
pub struct Server {
    pub address: SocketAddr,
    config: HttpConfig,
    receiver: mpsc::Receiver<Incoming>,
    buffered: Option<Incoming>,
    pending: BTreeMap<i64, Pending>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn number(config: &Json, key: &str, default: u64, maximum: u64) -> Result<u64, EvalError> {
    let value = config.get(key).map_or(Ok(default), |value| {
        value
            .as_u64()
            .ok_or_else(|| invalid(format!("{key} must be an integer")))
    })?;
    if value == 0 || value > maximum {
        return Err(invalid(format!("{key} exceeds permitted bounds")));
    }
    Ok(value)
}
impl HttpConfig {
    pub fn from_json(config: &Json) -> Result<Self, EvalError> {
        if !config.is_object() {
            return Err(invalid("HTTP config must be a map"));
        }
        let address = config
            .get("address")
            .and_then(Json::as_str)
            .unwrap_or("127.0.0.1:8080")
            .parse()
            .map_err(|_| invalid("address must be an IP:port socket address"))?;
        Ok(Self {
            address,
            max_body: number(config, "max-body-bytes", 8 << 20, 64 << 20)? as usize,
            max_headers: number(config, "max-header-bytes", 16 << 10, 64 << 10)? as usize,
            timeout: Duration::from_millis(number(config, "timeout-ms", 30000, 300000)?),
            max_queue: number(config, "max-queue", 64, 1024)? as usize,
        })
    }
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}
fn header_value(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte == b'\t' || (byte >= 32 && byte != 127))
}
pub fn headers(value: Option<&Json>, cap: usize) -> Result<BTreeMap<String, String>, EvalError> {
    let mut output = BTreeMap::new();
    let mut bytes = 0usize;
    if let Some(value) = value {
        for (key, value) in value
            .as_object()
            .ok_or_else(|| invalid("headers must be a string map"))?
        {
            let value = value
                .as_str()
                .ok_or_else(|| invalid("header values must be strings"))?;
            if !token(key) || !header_value(value) {
                return Err(invalid("invalid HTTP header"));
            }
            bytes = bytes.saturating_add(key.len() + value.len() + 4);
            if bytes > cap || output.len() >= 128 {
                return Err(invalid("headers exceed limit"));
            }
            if output
                .insert(key.to_ascii_lowercase(), value.into())
                .is_some()
            {
                return Err(invalid("duplicate normalized header"));
            }
        }
    }
    Ok(output)
}
impl Server {
    pub fn listen(config: HttpConfig) -> Result<Self, EvalError> {
        let listener = TcpListener::bind(config.address).map_err(backend)?;
        listener.set_nonblocking(true).map_err(backend)?;
        let address = listener.local_addr().map_err(backend)?;
        let (sender, receiver) = mpsc::sync_channel(config.max_queue);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let limits = config.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = thread::spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                let active = Arc::new(AtomicUsize::new(0));
                let mut next_id = 1i64;
                while !thread_stop.load(Ordering::Acquire) {
                    match tokio::time::timeout(Duration::from_millis(20), listener.accept()).await {
                        Ok(Ok((stream, _))) => {
                            if active.load(Ordering::Acquire) >= limits.max_queue {
                                tokio::spawn(async move {
                                    let _ = tokio::time::timeout(
                                        Duration::from_millis(100),
                                        reject(stream, 503),
                                    )
                                    .await;
                                });
                                continue;
                            }
                            let Some(following) = next_id.checked_add(1) else {
                                break;
                            };
                            let id = next_id;
                            next_id = following;
                            active.fetch_add(1, Ordering::AcqRel);
                            let active = active.clone();
                            let limits = limits.clone();
                            let sender = sender.clone();
                            tokio::spawn(async move {
                                let _ = tokio::time::timeout(
                                    limits.timeout,
                                    exchange(stream, id, &limits, sender),
                                )
                                .await;
                                active.fetch_sub(1, Ordering::AcqRel);
                            });
                        }
                        Ok(Err(_)) => break,
                        Err(_) => (),
                    }
                }
            });
            // Dropping the runtime cancels all outstanding native socket tasks.
        });
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(Self {
                address,
                config,
                receiver,
                buffered: None,
                pending: BTreeMap::new(),
                stop,
                thread: Some(thread),
            }),
            other => {
                stop.store(true, Ordering::Release);
                let _ = thread.join();
                Err(backend(format!("HTTP listener startup: {other:?}")))
            }
        }
    }
    fn prune(&mut self) {
        self.pending
            .retain(|_, pending| pending.deadline > Instant::now() && !pending.reply.is_closed());
    }
    pub fn next(&mut self, timeout: Duration) -> Result<Option<HttpRequest>, EvalError> {
        self.prune();
        if self.stop.load(Ordering::Acquire) {
            return Err(backend("HTTP server is closed"));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let incoming = match self.buffered.take() {
                Some(incoming) => incoming,
                None => match self
                    .receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    Ok(incoming) => incoming,
                    Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(backend("HTTP listener stopped"));
                    }
                },
            };
            if incoming.deadline <= Instant::now() || incoming.reply.is_closed() {
                continue;
            }
            self.pending.insert(
                incoming.request.id,
                Pending {
                    reply: incoming.reply,
                    deadline: incoming.deadline,
                },
            );
            return Ok(Some(incoming.request));
        }
    }
    pub fn readable(&mut self) -> Result<bool, EvalError> {
        self.prune();
        if self.stop.load(Ordering::Acquire) {
            return Err(backend("HTTP server is closed"));
        }
        loop {
            if self.buffered.as_ref().is_some_and(|incoming| {
                incoming.deadline > Instant::now() && !incoming.reply.is_closed()
            }) {
                return Ok(true);
            }
            self.buffered = None;
            match self.receiver.try_recv() {
                Ok(incoming) => self.buffered = Some(incoming),
                Err(mpsc::TryRecvError::Empty) => return Ok(false),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(backend("HTTP listener stopped"));
                }
            }
        }
    }
    pub fn reply(&mut self, id: i64, response: HttpResponse) -> Result<(), EvalError> {
        self.prune();
        if !(100..=599).contains(&response.status) || response.body.len() > self.config.max_body {
            return Err(invalid("HTTP response status/body exceeds limits"));
        }
        let header_json = serde_json::to_value(&response.headers).map_err(backend)?;
        headers(Some(&header_json), self.config.max_headers)?;
        let pending = self
            .pending
            .remove(&id)
            .ok_or_else(|| invalid("HTTP request id is unknown, expired or already replied"))?;
        pending
            .reply
            .send(response)
            .map_err(|_| backend("HTTP request expired before reply"))
    }
    pub fn close(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.pending.clear();
        self.buffered = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.close();
    }
}

async fn line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    cap: usize,
) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated HTTP line",
            ));
        }
        let n = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if output.len().saturating_add(n) > cap {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP header exceeds limit",
            ));
        }
        let complete = buffer[n - 1] == b'\n';
        output.extend_from_slice(&buffer[..n]);
        reader.consume(n);
        if complete {
            if !output.ends_with(b"\r\n") {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "HTTP requires CRLF",
                ));
            }
            output.truncate(output.len() - 2);
            return Ok(output);
        }
    }
}
fn bad(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}
async fn parse(
    reader: &mut BufReader<tokio::net::TcpStream>,
    id: i64,
    limits: &HttpConfig,
) -> std::io::Result<HttpRequest> {
    let start = line(reader, limits.max_headers).await?;
    let mut used = start.len() + 2;
    let start = std::str::from_utf8(&start).map_err(|_| bad("request is not UTF-8"))?;
    let mut parts = start.split(' ');
    let method = parts.next().ok_or_else(|| bad("missing method"))?;
    let target = parts.next().ok_or_else(|| bad("missing target"))?;
    let version = parts.next().ok_or_else(|| bad("missing version"))?;
    if !token(method)
        || !(target.starts_with('/') || target == "*")
        || target
            .bytes()
            .any(|byte| byte <= 32 || byte == 127 || byte == b'#')
        || !matches!(version, "HTTP/1.1" | "HTTP/1.0")
        || parts.next().is_some()
    {
        return Err(bad("invalid request line"));
    }
    let mut headers = BTreeMap::new();
    loop {
        let bytes = line(reader, limits.max_headers.saturating_sub(used)).await?;
        used = used.saturating_add(bytes.len() + 2);
        if used > limits.max_headers {
            return Err(bad("headers exceed byte limit"));
        }
        if bytes.is_empty() {
            break;
        }
        if headers.len() >= 128 {
            return Err(bad("too many headers"));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| bad("header is not UTF-8"))?;
        let (name, value) = text
            .split_once(':')
            .ok_or_else(|| bad("malformed header"))?;
        if !token(name) || !header_value(value) {
            return Err(bad("invalid header"));
        }
        if headers
            .insert(name.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err(bad("duplicate header"));
        }
    }
    let mut body = Vec::new();
    match (
        headers.get("content-length"),
        headers.get("transfer-encoding"),
    ) {
        (Some(_), Some(_)) => return Err(bad("ambiguous body framing")),
        (Some(length), None) => {
            if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(bad("invalid Content-Length"));
            }
            let length = length
                .parse::<usize>()
                .map_err(|_| bad("invalid Content-Length"))?;
            if length > limits.max_body {
                return Err(bad("body exceeds limit"));
            }
            body.resize(length, 0);
            reader.read_exact(&mut body).await?;
        }
        (None, Some(encoding)) if encoding.eq_ignore_ascii_case("chunked") => loop {
            let size = line(reader, 128).await?;
            let text = std::str::from_utf8(&size).map_err(|_| bad("invalid chunk size"))?;
            let size = text.split(';').next().unwrap_or("");
            if size.is_empty() || !size.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(bad("invalid chunk size"));
            }
            let size = usize::from_str_radix(size, 16).map_err(|_| bad("invalid chunk size"))?;
            if size == 0 {
                let mut trailers = 0usize;
                loop {
                    let trailer = line(reader, limits.max_headers.saturating_sub(trailers)).await?;
                    trailers += trailer.len() + 2;
                    if trailers > limits.max_headers {
                        return Err(bad("trailers exceed limit"));
                    }
                    if trailer.is_empty() {
                        break;
                    }
                    let text = std::str::from_utf8(&trailer).map_err(|_| bad("invalid trailer"))?;
                    let (key, value) =
                        text.split_once(':').ok_or_else(|| bad("invalid trailer"))?;
                    if !token(key)
                        || !header_value(value)
                        || matches!(
                            key.to_ascii_lowercase().as_str(),
                            "content-length" | "transfer-encoding" | "host"
                        )
                    {
                        return Err(bad("invalid trailer"));
                    }
                }
                break;
            }
            if body.len().saturating_add(size) > limits.max_body {
                return Err(bad("body exceeds limit"));
            }
            let start = body.len();
            body.resize(start + size, 0);
            reader.read_exact(&mut body[start..]).await?;
            let mut ending = [0; 2];
            reader.read_exact(&mut ending).await?;
            if &ending != b"\r\n" {
                return Err(bad("invalid chunk ending"));
            }
        },
        (None, Some(_)) => return Err(bad("unsupported Transfer-Encoding")),
        (None, None) => (),
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(HttpRequest {
        id,
        method: method.into(),
        path: path.into(),
        query: query.into(),
        headers,
        body,
    })
}
async fn reject(mut stream: tokio::net::TcpStream, status: u16) -> std::io::Result<()> {
    stream
        .write_all(
            format!("HTTP/1.1 {status} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    stream.shutdown().await
}
async fn exchange(
    stream: tokio::net::TcpStream,
    id: i64,
    limits: &HttpConfig,
    sender: mpsc::SyncSender<Incoming>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let request = match parse(&mut reader, id, limits).await {
        Ok(request) => request,
        Err(_) => return reject(reader.into_inner(), 400).await,
    };
    let method = request.method.clone();
    let (reply_tx, reply_rx) = oneshot::channel();
    if sender
        .try_send(Incoming {
            request,
            reply: reply_tx,
            deadline: Instant::now() + limits.timeout,
        })
        .is_err()
    {
        return reject(reader.into_inner(), 503).await;
    }
    let response = match reply_rx.await {
        Ok(response) => response,
        Err(_) => return reject(reader.into_inner(), 503).await,
    };
    let mut stream = reader.into_inner();
    let body_allowed =
        method != "HEAD" && response.status >= 200 && !matches!(response.status, 204 | 304);
    let length = if response.status == 204 || response.status < 200 {
        0
    } else {
        response.body.len()
    };
    let mut head = format!("HTTP/1.1 {} Response\r\n", response.status);
    for (key, value) in response.headers {
        if !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-length" | "transfer-encoding" | "connection"
        ) {
            head.push_str(&format!("{key}: {value}\r\n"));
        }
    }
    head.push_str(&format!(
        "Content-Length: {length}\r\nConnection: close\r\n\r\n"
    ));
    stream.write_all(head.as_bytes()).await?;
    if body_allowed {
        stream.write_all(&response.body).await?;
    }
    stream.shutdown().await
}

pub fn request(config: &Json, body: &[u8]) -> Result<HttpResponse, EvalError> {
    let limits = HttpConfig::from_json(config)?;
    if body.len() > limits.max_body {
        return Err(invalid("HTTP request body exceeds limit"));
    }
    let url = config
        .get("url")
        .and_then(Json::as_str)
        .ok_or_else(|| invalid("HTTP request requires url"))?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(invalid("HTTP URL scheme must be http or https"));
    }
    let method = config.get("method").and_then(Json::as_str).unwrap_or("GET");
    if !token(method) {
        return Err(invalid("invalid HTTP method"));
    }
    let headers = headers(config.get("headers"), limits.max_headers)?;
    if headers.keys().any(|key| {
        matches!(
            key.as_str(),
            "content-length" | "transfer-encoding" | "connection"
        )
    }) {
        return Err(invalid("HTTP body framing headers are transport-owned"));
    }
    // No ambient proxy authority, redirects, retries or application credential policy.
    let agent = ureq::AgentBuilder::new()
        .timeout(limits.timeout)
        .redirects(0)
        .try_proxy_from_env(false)
        .build();
    let mut request = agent.request(method, url);
    for (key, value) in headers {
        request = request.set(&key, &value);
    }
    let response = match request.send_bytes(body) {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error) => return Err(backend(error)),
    };
    let mut headers = BTreeMap::new();
    let mut size = 0usize;
    for name in response.headers_names() {
        if headers.len() >= 128 {
            return Err(EvalError::custom(
                "protocol-violation: response has too many headers",
            ));
        }
        let values = response.all(&name);
        let value = values.join(", ");
        size = size.saturating_add(name.len() + value.len() + 4);
        if size > limits.max_headers {
            return Err(EvalError::custom(
                "protocol-violation: response headers exceed byte limit",
            ));
        }
        headers.insert(name.to_ascii_lowercase(), value);
    }
    let status = response.status();
    let mut body = Vec::new();
    response
        .into_reader()
        .take(limits.max_body as u64 + 1)
        .read_to_end(&mut body)
        .map_err(backend)?;
    if body.len() > limits.max_body {
        return Err(EvalError::custom(
            "protocol-violation: response body exceeds byte limit",
        ));
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}
