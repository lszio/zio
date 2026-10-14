use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn send(w: &mut impl Write, v: Value) {
    let body = serde_json::to_vec(&v).unwrap();
    write!(w, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    w.write_all(&body).unwrap();
    w.flush().unwrap();
}
fn recv(r: &mut impl BufRead) -> Value {
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert!(r.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some(n) = line.strip_prefix("Content-Length: ") {
            length = n.trim().parse().unwrap();
        }
    }
    let mut bytes = vec![0; length];
    r.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[test]
fn real_stdio_edits_shadowing_unicode_modules_and_no_effects() {
    let root = std::env::temp_dir().join(format!("zio-lsp-contract-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let canary = root.join("should-not-exist");
    std::fs::write(
        root.join("math.zio"),
        "(module :math :export [twice] (defn twice [x] (+ x x)) (def private 1))",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_zio-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    let mut r = BufReader::new(child.stdout.take().unwrap());
    send(
        &mut w,
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":format!("file://{}",root.display())}}),
    );
    assert_eq!(
        recv(&mut r)["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    send(
        &mut w,
        json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
    );
    let uri = format!("file://{}/main.zio", root.display());
    let text = format!(
        "(require :math :refer [twice])\r\n(def x 1)\r\n(let* [x 2] (+ x (twice x)))\r\n; 中文😀\r\n(spit \"{}\" \"bad\")\r\n(defmacro evil [] (spit \"{}\" \"bad\"))\r\n(evil)\r\n",
        canary.display(),
        canary.display()
    );
    send(
        &mut w,
        json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":uri,"languageId":"zio","version":1,"text":text}}}),
    );
    assert_eq!(recv(&mut r)["params"]["diagnostics"], json!([]));
    send(
        &mut w,
        json!({"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{"textDocument":{"uri":uri},"position":{"line":2,"character":15}}}),
    );
    let local = recv(&mut r);
    assert_eq!(
        local["result"][0]["range"]["start"],
        json!({"line":2,"character":7})
    );
    send(
        &mut w,
        json!({"jsonrpc":"2.0","id":3,"method":"textDocument/references","params":{"textDocument":{"uri":uri},"position":{"line":2,"character":15},"context":{"includeDeclaration":true}}}),
    );
    let refs = recv(&mut r);
    assert_eq!(refs["result"].as_array().unwrap().len(), 3);
    send(
        &mut w,
        json!({"jsonrpc":"2.0","id":4,"method":"textDocument/definition","params":{"textDocument":{"uri":uri},"position":{"line":2,"character":18}}}),
    );
    assert_eq!(
        recv(&mut r)["result"][0]["uri"],
        format!("file://{}/math.zio", root.display())
    );
    send(
        &mut w,
        json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":"; 中文😀\r\n(def x 1)\r\n(let* [x 3] x)\r\n\"中😀\" ]"}]}}),
    );
    let bad = recv(&mut r);
    assert_eq!(bad["params"]["version"], 2);
    assert_eq!(
        bad["params"]["diagnostics"][0]["range"]["start"],
        json!({"line":3,"character":6})
    );
    send(
        &mut w,
        json!({"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":uri,"version":3},"contentChanges":[{"range":{"start":{"line":3,"character":6},"end":{"line":3,"character":7}},"text":""}]}}),
    );
    assert_eq!(recv(&mut r)["params"]["diagnostics"], json!([]));
    send(
        &mut w,
        json!({"jsonrpc":"2.0","id":5,"method":"textDocument/definition","params":{"textDocument":{"uri":uri},"position":{"line":2,"character":12}}}),
    );
    assert_eq!(
        recv(&mut r)["result"][0]["range"]["start"],
        json!({"line":2,"character":7})
    );
    send(
        &mut w,
        json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":uri}}}),
    );
    assert_eq!(recv(&mut r)["params"]["diagnostics"], json!([]));
    send(
        &mut w,
        json!({"jsonrpc":"2.0","id":6,"method":"shutdown","params":null}),
    );
    assert_eq!(recv(&mut r)["result"], Value::Null);
    send(&mut w, json!({"jsonrpc":"2.0","method":"exit"}));
    assert!(child.wait().unwrap().success());
    assert!(!canary.exists());
    std::fs::remove_dir_all(root).unwrap();
}
