use serde_json::json;
use std::io::Cursor;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zio_host::HostPolicy;
use zio_host::transport::{Process, ProcessConfig, ProcessRead, framing};

#[test]
fn content_length_distinguishes_eof_from_truncation_and_refuses_ambiguous_lengths() {
    assert!(
        framing::read_content_length(&mut Cursor::new(b""), 1024, 1024)
            .unwrap()
            .is_none()
    );
    for bytes in [
        b"Content-Length: 7\r\n\r\n{}".as_slice(),
        b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
        b"Content-Length: 9999\r\n\r\n",
    ] {
        assert!(framing::read_content_length(&mut Cursor::new(bytes), 1024, 1024).is_err());
    }
    let message = json!({"jsonrpc":"2.0", "id":1, "method":"λ"});
    let mut wire = Vec::new();
    framing::write_content_length(&mut wire, &message, 1024).unwrap();
    assert_eq!(
        framing::read_content_length(&mut Cursor::new(wire), 1024, 1024).unwrap(),
        Some(message)
    );
}

fn policy() -> HostPolicy {
    HostPolicy {
        process: true,
        read_roots: vec![
            PathBuf::from("/usr"),
            PathBuf::from("/bin"),
            std::env::temp_dir(),
        ],
        write_roots: vec![std::env::temp_dir()],
        ..HostPolicy::default()
    }
    .canonicalized()
    .unwrap()
}

#[test]
fn process_start_never_falls_back_when_a_jail_is_not_declared() {
    let config = ProcessConfig::from_json(
        json!({"executable":"/bin/sh", "argv":["-c","echo should-not-run"]}),
        &policy(),
    )
    .unwrap();
    assert!(Process::start(&config, &policy()).is_err());
}

#[test]
fn jail_canaries_symlink_escape_and_network_are_absent_or_namespace_is_refused() {
    let root = std::env::temp_dir().join(format!("zio-host-jail-contract-{}", std::process::id()));
    std::fs::create_dir_all(root.join("scratch")).unwrap();
    std::fs::write(root.join("credential"), b"secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("credential"), root.join("scratch/escape")).unwrap();
    let probe = format!(
        "test ! -e {0}/credential && test ! -e /scratch/escape && test ! -e /home && test ! -e /etc/resolv.conf && echo '{{\"isolated\":true}}' && echo allowed > /scratch/ok",
        root.display()
    );
    let config = ProcessConfig::from_json(json!({"executable":"/usr/bin/sh", "argv":["-c",probe], "scratch":root.join("scratch"), "read-only-mounts":[{"source":"/usr", "target":"/usr"}], "timeout-ms":3000}), &policy()).unwrap();
    let started = Instant::now();
    match Process::start(&config, &policy()) {
        Err(error) => assert!(error.to_string().contains("capability-denied")),
        Ok(mut process) => match process.read(Duration::from_secs(3)) {
            Ok(ProcessRead::Frame(frame)) => {
                assert_eq!(frame, json!({"isolated":true}));
                assert_eq!(
                    std::fs::read(root.join("scratch/ok")).unwrap(),
                    b"allowed\n"
                );
                assert!(
                    process
                        .wait(Duration::from_secs(1))
                        .unwrap()
                        .unwrap()
                        .success
                );
            }
            Err(error) => assert!(error.to_string().contains("capability-denied")),
            other => panic!("unexpected jail result: {other:?}"),
        },
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(std::fs::read(root.join("credential")).unwrap(), b"secret");
    std::fs::remove_dir_all(root).unwrap();
}
