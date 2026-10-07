#!/usr/bin/env python3
"""Drive a real stdio LSP session against the installed zio-lsp.

Kept as a script rather than a Rust test because the thing under test is
the wire: a real initialize handshake, a real document with CJK, emoji
and CRLF, a real edit, and a real definition/references answer.

The document contains `(eval '(do (spit "<canary>" ...)))`. The server
must analyze it as text. If it executed the document, the canary file
would exist — that is the check that matters most here, because an
editor that runs what you are typing is not an editor.

Usage: tools/lsp_probe.py [path-to-zio-lsp]
"""
import json
import os
import select
import subprocess
import sys
import tempfile

LSP = sys.argv[1] if len(sys.argv) > 1 else "target/debug/zio-lsp"

canary = os.path.join(tempfile.gettempdir(), "zio-lsp-canary.txt")
if os.path.exists(canary):
    os.remove(canary)

proc = subprocess.Popen(
    [LSP],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    # Unbuffered. A `select`-able stream needs its own buffer, and the
    # default BufferedReader would hold bytes the select is waiting on —
    # which presents as a server that never answers.
    bufsize=0,
)

failures = []


def check(label, condition, detail=""):
    print(f"{label}: {condition}{(' — ' + detail) if detail else ''}")
    if not condition:
        failures.append(label)


def send(payload):
    body = json.dumps(payload).encode("utf-8")
    proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    proc.stdin.flush()


def read(timeout=8.0):
    """One framed message, or None when the server says nothing.

    A blocking read is what made an earlier probe look like a hang: the
    server is right to stay quiet after the last answer, and a reader
    waiting for bytes that will never come cannot tell that apart.
    """
    length = None
    while True:
        ready, _, _ = select.select([proc.stdout], [], [], timeout)
        if not ready:
            return None
        line = proc.stdout.readline()
        if not line:
            return None
        text = line.decode("utf-8").strip()
        if text.lower().startswith("content-length:"):
            length = int(text.split(":")[1])
        elif text == "":
            break
    if length is None:
        return None
    body = b""
    while len(body) < length:
        ready, _, _ = select.select([proc.stdout], [], [], timeout)
        if not ready:
            return None
        body += proc.stdout.read(length - len(body))
    return json.loads(body.decode("utf-8"))


source = (
    "(defn 计算 [x] (* x 2))\r\n"
    "(def helper 1)\r\n"
    "(println \"🌍 emoji\")\r\n"
    "(println (计算 helper))\r\n"
    "(eval '(do (spit \"" + canary + "\" \"owned\") 1))\r\n"
)
uri = "file:///tmp/probe.zio"

send({
    "jsonrpc": "2.0", "id": 1, "method": "initialize",
    "params": {"processId": None, "rootUri": None, "capabilities": {}},
})
init = read()
check("initialize answered", init is not None and "result" in init)
caps = (init or {}).get("result", {}).get("capabilities", {})
check("positionEncoding is utf-16", caps.get("positionEncoding") == "utf-16",
      str(caps.get("positionEncoding")))

send({"jsonrpc": "2.0", "method": "initialized", "params": {}})
send({
    "jsonrpc": "2.0", "method": "textDocument/didOpen",
    "params": {"textDocument": {"uri": uri, "languageId": "zio", "version": 1, "text": source}},
})

send({"jsonrpc": "2.0", "id": 2, "method": "textDocument/documentSymbol",
      "params": {"textDocument": {"uri": uri}}})
# Character 1 of `(println (计算 helper))` is inside the `计算` token.
# Asking past the end of the line answers with an empty list, which is
# correct behaviour and a useless check.
send({"jsonrpc": "2.0", "id": 3, "method": "textDocument/definition",
      "params": {"textDocument": {"uri": uri}, "position": {"line": 3, "character": 11}}})
send({"jsonrpc": "2.0", "id": 4, "method": "textDocument/references",
      "params": {"textDocument": {"uri": uri}, "position": {"line": 1, "character": 6},
                 "context": {"includeDeclaration": True}}})

answers = {}
diagnostics_seen = False
for _ in range(200):
    msg = read()
    if msg is None:
        break
    if "id" in msg:
        answers[msg["id"]] = msg.get("result")
    elif msg.get("method") == "textDocument/publishDiagnostics":
        diagnostics_seen = True

check("documentSymbol returned symbols", len(answers.get(2) or []) > 0,
      f"{len(answers.get(2) or [])} symbols")
check("definition resolved", bool(answers.get(3)))
check("references resolved", len(answers.get(4) or []) > 0,
      f"{len(answers.get(4) or [])} references")
check("diagnostics published", diagnostics_seen)
check("document was not executed", not os.path.exists(canary),
      "canary file exists" if os.path.exists(canary) else "")

send({"jsonrpc": "2.0", "id": 5, "method": "shutdown", "params": None})
shutdown_ok = False
for _ in range(40):
    msg = read()
    if msg is None:
        break
    if msg.get("id") == 5:
        shutdown_ok = "result" in msg
        break
check("shutdown answered", shutdown_ok)

send({"jsonrpc": "2.0", "method": "exit", "params": None})
proc.wait(timeout=10)
check("exit code is zero", proc.returncode == 0, str(proc.returncode))

print()
if failures:
    print("FAILED: " + ", ".join(failures))
    sys.exit(1)
print("all language-service checks passed")
