//! ZIO-009 stdio contract tests: spawn the real `zio-mcp` binary and
//! exchange JSON-RPC 2.0 lines over its stdin/stdout. These cover the
//! contract §13 verification matrix row 4 (RunId round-trip), the typed
//! capability denial (matrix row 3 through the wire), session scoping,
//! and session release.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

const BIG_INTEGER: &str = "1234567890123456789";

/// A live `zio-mcp` process whose protocol pipe is line-delimited JSON.
struct ServerProcess {
    child: Child,
    stdout: BufReader<ChildStdout>,
    stdin: ChildStdin,
}

impl ServerProcess {
    fn start() -> ServerProcess {
        let mut child = Command::new(env!("CARGO_BIN_EXE_zio-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn zio-mcp");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        let stdin = child.stdin.take().expect("child stdin");
        ServerProcess {
            child,
            stdout,
            stdin,
        }
    }

    /// Send one request line, read exactly one response line.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        writeln!(self.stdin, "{request}").expect("write request");
        self.stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read response line");
        assert!(
            !line.is_empty(),
            "server exited without responding to {method}"
        );
        serde_json::from_str(&line).expect("response line is valid JSON")
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdio_round_trip_run_id_preserved() {
    let mut server = ServerProcess::start();
    let created = server.call("create_session", json!({}));
    let session_id = created["result"]["session_id"]
        .as_u64()
        .expect("create_session returns session_id");

    let response = server.call(
        "eval",
        json!({ "session_id": session_id, "name": "bigint", "source": BIG_INTEGER }),
    );
    let result = &response["result"];

    // The harness generates the RunId and the response returns it
    // unchanged (contract §3/§13, matrix row 4).
    assert!(result["run_id"].is_u64(), "run_id present in envelope");

    // Bigint precision: integers travel as decimal strings, so the
    // value survives the wire exactly (contract §6 row 6).
    assert_eq!(result["value"]["kind"], "integer");
    assert_eq!(result["value"]["value"], BIG_INTEGER);
    assert!(result["error"].is_null(), "eval succeeded, error is null");
}

#[test]
fn capability_denied_maps_to_typed_error() {
    let mut server = ServerProcess::start();
    let created = server.call("create_session", json!({ "capabilities": {} }));
    let session_id = created["result"]["session_id"]
        .as_u64()
        .expect("create_session returns session_id");

    let response = server.call(
        "eval",
        json!({ "session_id": session_id, "name": "slurp", "source": r#"(slurp "/etc/hostname")"# }),
    );

    // A denied capability is a typed envelope error, not a JSON-RPC
    // failure (contract §10/§12; typed, never generic).
    assert!(response.get("error").is_none(), "response carries a result");
    let error = &response["result"]["error"];
    assert_eq!(error["kind"], "CapabilityDenied");
    assert_eq!(error["capability"], "FileRead");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("capability denied"),
        "message: {}",
        error["message"]
    );
}

#[test]
fn missing_session_returns_error() {
    let mut server = ServerProcess::start();
    let response = server.call(
        "eval",
        json!({ "session_id": 99999, "name": "x", "source": "1" }),
    );
    assert_eq!(response["error"]["code"], -32000);
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session not found"),
        "message: {}",
        response["error"]["message"]
    );
}

#[test]
fn close_session_releases() {
    let mut server = ServerProcess::start();
    let created = server.call("create_session", json!({}));
    let session_id = created["result"]["session_id"]
        .as_u64()
        .expect("create_session returns session_id");

    let closed = server.call("close_session", json!({ "session_id": session_id }));
    assert!(
        closed["result"]["ok"].as_bool().unwrap(),
        "close_session acknowledges"
    );

    let again = server.call("close_session", json!({ "session_id": session_id }));
    assert_eq!(again["error"]["code"], -32000);
    assert!(
        again["error"]["message"]
            .as_str()
            .unwrap()
            .contains("session not found"),
        "message: {}",
        again["error"]["message"]
    );
}
