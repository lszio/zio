//! HTTP-feature contract tests. All of them are offline: the timeout test
//! uses a local TCP listener that accepts but never answers, which is a
//! deterministic read timeout with no external network involved.

#![cfg(feature = "http")]

use std::net::TcpListener;
use std::time::Duration;

use zio_ai::http::HttpAiHost;
use zio_ai::{HostErrorKind, LlmHost};

#[test]
fn read_timeout_maps_to_timeout_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept and stay silent — the client must hit its read timeout.
    std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_secs(5));
    });

    let host = HttpAiHost::builder()
        .base_url(format!("http://127.0.0.1:{port}"))
        .timeout(Duration::from_millis(300))
        .build();
    let err = host.complete("hi", &Default::default()).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Timeout, "got: {err}");
}

#[test]
fn transport_failure_maps_to_transport_error() {
    // Port 1 on loopback: nothing listens there, the connection is refused.
    let host = HttpAiHost::builder()
        .base_url("http://127.0.0.1:1")
        .timeout(Duration::from_millis(500))
        .build();
    let err = host.complete("hi", &Default::default()).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Transport, "got: {err}");
}

#[test]
fn response_cap_rejects_oversized_bodies() {
    // A listener that answers with more bytes than the cap allows.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let (mut socket, _) = listener.accept().unwrap();
        // Drain the request before replying: closing with unread request
        // bytes in the receive queue sends RST instead of FIN, and on
        // macOS a read() after RST on a timeout-armed socket yields EINVAL.
        socket
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        let mut buf = [0u8; 4096];
        loop {
            match socket.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => continue,
            }
        }
        let body = "x".repeat(4096);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes());
        let _ = socket.flush();
        // Keep the socket open long enough for the client to finish
        // reading, so the close is a clean FIN.
        std::thread::sleep(Duration::from_millis(300));
    });

    let host = HttpAiHost::builder()
        .base_url(format!("http://127.0.0.1:{port}"))
        .timeout(Duration::from_secs(2))
        .max_response_bytes(1024)
        .build();
    let err = host.complete("hi", &Default::default()).unwrap_err();
    assert_eq!(err.kind, HostErrorKind::Protocol, "got: {err}");
    assert!(err.to_string().contains("cap"), "{err}");
}
