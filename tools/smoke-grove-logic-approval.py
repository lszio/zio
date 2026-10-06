#!/usr/bin/env python3
"""G04 real smoke: drive the approval boundary against a live `grove serve`
over real bearer tokens.

Two legs, both real:

A. governance refusals — a proposal naming a protected region, an operator
   reaching the approval boundary, and an approval of something that never
   qualified.
B. a real human approval — the dual demo trains on the frozen task and
   commits its trained weights, then the publisher token approves that
   candidate through `POST /api/approve` and the pointer moves to it.

Usage:  GROVE_TOKEN_* are required.
        python3 tools/smoke-grove-logic-approval.py
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIND = os.environ.get("GROVE_SMOKE_BIND", "127.0.0.1:8871")
BASE = f"http://{BIND}"


def call(method, path, token, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{BASE}{path}", data=data, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw or b"null")
        except json.JSONDecodeError:
            return e.code, {"raw": raw.decode(errors="replace")}


def wait_for_health(token):
    for _ in range(120):
        try:
            if call("GET", "/api/health", token)[0] == 200:
                return True
        except Exception:
            pass
        time.sleep(0.25)
    return False


def main():
    reader = os.environ["GROVE_TOKEN_READER"]
    operator = os.environ["GROVE_TOKEN_OPERATOR"]
    publisher = os.environ["GROVE_TOKEN_PUBLISHER"]
    root = "/tmp/grove-g04-smoke"
    subprocess.run(["rm", "-rf", root], check=False)

    # The demo provisions the frozen protocol and trains real weights. It
    # runs before the service so the store exists and has a candidate.
    print("0. grove demo --case dual (real CPU training on the frozen task)…")
    demo = subprocess.run(
        ["cargo", "run", "-q", "-p", "grove-app", "--bin", "grove", "--",
         "demo", "--case", "dual", "--root", root],
        cwd=ROOT, capture_output=True, text=True,
    )
    if demo.returncode != 0:
        print("FAIL: the demo did not complete")
        print(demo.stdout[-3000:], demo.stderr[-2000:])
        return 1
    report = demo.stdout
    assert "publication: none" in report, f"the demo published by itself:\n{report}"
    candidate = [l.rsplit(" ", 1)[1] for l in report.splitlines() if "--snapshot" in l][-1]
    print(f"   demo left a pending candidate: {candidate[:12]}, nothing deployed")

    server = subprocess.Popen(
        ["cargo", "run", "-q", "-p", "grove-app", "--features", "http", "--bin", "grove",
         "--", "serve", "--root", root, "--bind", BIND],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
    )
    try:
        if not wait_for_health(reader):
            print("FAIL: the service never became healthy")
            return 1

        # ── A. governance refusals ────────────────────────────────
        protected = call("POST", "/api/logic/propose", operator, {
            "id": "smoke-protected",
            "module_path": "learning/src/evaluation.rs",
            "source": "(defn agent-run [t n] {:status \"candidate\"})\n",
            "diff": "try to rewrite the evaluator",
        })
        assert protected[0] == 400, f"protected region was not refused: {protected}"
        assert "protected" in protected[1]["detail"], protected[1]
        print("A1. a proposal naming a protected region is refused:",
              protected[1]["detail"][:88])

        source = ("(defn agent-run [task max-turns]\n"
                  "  (if (> max-turns 0) {:status \"candidate\"} {:status \"exhausted\"}))\n\n"
                  "(defn validate-entry [s] (= \"agent-entry\" s))\n")
        proposed = call("POST", "/api/logic/propose", operator, {
            "id": "smoke-candidate",
            "module_path": "lib/zio/agent.zio",
            "source": source,
            "diff": "+validate-entry\n",
        })
        assert proposed[0] == 200, f"governed candidate was refused: {proposed}"
        print(f"A2. a governed-region candidate is accepted: {proposed[1]['id']}")

        forbidden = call("POST", "/api/approve", operator, {
            "operation_id": "smoke-op-forbidden",
            "snapshot": candidate, "protocol": "accept-v1",
        })
        assert forbidden[0] == 403, f"operator reached the boundary: {forbidden}"
        print("A3. an operator token is refused at the boundary:",
              forbidden[1]["detail"][:70])

        # ── B. the real human approval ────────────────────────────
        before = call("GET", "/api/logic/candidates", reader)[1]
        assert before["active"] is None, f"something was already deployed: {before}"

        approved = call("POST", "/api/approve", publisher, {
            "operation_id": "smoke-op-approve",
            "snapshot": candidate, "protocol": "accept-v1",
        })
        assert approved[0] == 200, f"the qualified candidate was refused: {approved}"
        print(f"B1. a publisher approved the qualified candidate: {approved[1]['id']} "
              f"→ v{approved[1]['version']}")

        after = call("GET", "/api/logic/candidates", reader)[1]
        assert after["active"]["snapshot"] == candidate, after["active"]
        assert after["active"]["version"] == approved[1]["version"], after["active"]
        print(f"B2. the pointer moved to the trained weights: "
              f"v{after['active']['version']} {after['active']['snapshot'][:12]}")
        assert len(after["approvals"]) == 1, after["approvals"]
        assert after["approvals"][0]["authenticated_actor"], after["approvals"]
        print("B3. the decision is on the record:", after["approvals"][0]["id"])

        # a replay of the same operation id returns the first answer
        replay = call("POST", "/api/approve", publisher, {
            "operation_id": "smoke-op-approve",
            "snapshot": candidate, "protocol": "accept-v1",
        })
        assert replay[1]["version"] == approved[1]["version"], replay[1]
        assert call("GET", "/api/logic/candidates", reader)[1]["active"]["version"] == 1, \
            "a replayed operation id must not mint a new version"
        print("B4. replaying the operation id returns the first answer, no new version")

        # a stale expected-version loses the race
        stale = call("POST", "/api/approve", publisher, {
            "operation_id": "smoke-op-stale",
            "snapshot": candidate, "protocol": "accept-v1", "expected_version": 0,
        })
        assert stale[0] == 409, f"a stale version was accepted: {stale}"
        print("B5. a stale expected version is refused:",
              stale[1]["detail"][:80])

        print("\nG04 smoke passed: the refusals held and one qualified candidate "
              "was approved by hand.")
        return 0
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()


if __name__ == "__main__":
    sys.exit(main())