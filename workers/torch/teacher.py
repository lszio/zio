#!/usr/bin/env python3
"""W03/W08 local teacher service — the `local-teacher` source.

A real, small, CPU-trained model served over the same HTTP contract the
`grove`/`zio-ai` teacher host speaks. It is NOT a constant-label echo:
`train.py` fits a small MLP on an independent split of the W00 data and
freezes it; this service only does inference on those frozen weights.

The point is provenance, not accuracy: a teacher's answers carry
`model_version` and a real prediction, and a broken weights file is a
visible error rather than a confident wrong answer.

    python workers/torch/train.py --task ../../examples/self-learning \
        --steps 400 --out teacher.pt
    python workers/torch/teacher.py --weights teacher.pt --port 8088
"""

from __future__ import annotations

import argparse
import json
import math
import struct
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from data import Observation, load_split  # noqa: E402


def features(obs: Observation) -> list[float]:
    """256 image pixels + 2 sign-preserving numbers + 2 presence flags."""
    values = [p / 255.0 for p in obs.pixels]
    for r in obs.readings:
        values.append(r / (abs(r) + 1.0))
    values.append(1.0 if obs.image_present else 0.0)
    values.append(1.0 if obs.numeric_present else 0.0)
    return values


def forward(weights: dict, x: list[float]) -> tuple[float, float]:
    """Two-layer MLP: Linear → ReLU → Linear → softmax."""
    h = weights["hidden"]
    w1, b1, w2, b2 = h["w1"], h["b1"], h["w2"], h["b2"]
    hidden = [
        max(0.0, sum(w1[i][j] * x[j] for j in range(len(x))) + b1[i])
        for i in range(len(b1))
    ]
    logits = [
        sum(w2[i][j] * hidden[j] for j in range(len(hidden))) + b2[i]
        for i in range(len(b2))
    ]
    top = max(logits)
    exps = [math.exp(v - top) for v in logits]
    total = sum(exps)
    return exps[0] / total, exps[1] / total


class TeacherHandler(BaseHTTPRequestHandler):
    weights: dict = {}
    model_version: str = "local-teacher-unknown"
    wants_soft: bool = False

    def log_message(self, *args):  # keep stdout for the protocol only
        pass

    def _send(self, code: int, payload: dict):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        raw = self.rfile.read(length)
        try:
            body = json.loads(raw.decode("utf-8"))
        except Exception as exc:  # malformed request is a visible error
            self._send(400, {"error": f"invalid JSON: {exc}"})
            return

        request_id = body.get("request_id", "")
        try:
            obs = Observation.from_parts(body["parts"])
        except Exception as exc:
            self._send(400, {"request_id": request_id, "teacher_id": "local-teacher",
                             "error": f"undecodable content: {exc}"})
            return

        if not obs.image_present and not obs.numeric_present:
            p0, _p1 = forward(self.weights, features(obs))
        else:
            p0, _p1 = forward(self.weights, features(obs))

        hard = None if not (obs.image_present or obs.numeric_present) else (
            "clear" if p0 >= 0.5 else "fault"
        )
        payload = {
            "request_id": request_id,
            "teacher_id": "local-teacher",
            "model_version": self.model_version,
            "hard_label": hard,
            "soft_scores": [p0, 1.0 - p0] if body.get("want_soft") else None,
            "program": None,
            "error": None,
        }
        self._send(200, payload)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--weights", required=True, help="frozen weights from train.py")
    parser.add_argument("--port", type=int, default=8088)
    parser.add_argument("--host", default="127.0.0.1")
    args = parser.parse_args()

    with open(args.weights, "r", encoding="utf-8") as handle:
        blob = json.load(handle)
    TeacherHandler.weights = blob["weights"]
    TeacherHandler.model_version = blob["model_version"]

    server = HTTPServer((args.host, args.port), TeacherHandler)
    print(f"local-teacher {TeacherHandler.model_version} listening on "
          f"http://{args.host}:{args.port}/v1/teacher", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
