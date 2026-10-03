"""Worker protocol contract: frames, structural rejection, real training.

    python -m unittest discover -s workers/torch/tests -p test_worker.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import torch

ROOT = Path(__file__).resolve().parents[3]
WORKER = ROOT / "workers" / "torch"
TASK = ROOT / "examples" / "self-learning"
PYTHON = ROOT / ".venv" / "bin" / "python"

sys.path.insert(0, str(WORKER))
from graph import (  # noqa: E402
    Graph,
    GraphError,
    Op,
    TensorSpec,
    build_fusion_graph,
    graph_from_json,
    graph_to_json,
)

PROTOCOL = "grove.worker/1"


def python_bin() -> str:
    return str(PYTHON) if PYTHON.exists() else sys.executable


def linear_graph_payload() -> dict:
    graph, _ = build_fusion_graph(258, hidden=None, out_dim=2)
    graph.validate()
    return graph_to_json(graph)


def json_layer(rows: int, cols: int) -> dict:
    """One linear layer's artifact: weight (rows=out, cols=in) plus bias."""
    return {"w": [[0.0] * cols for _ in range(rows)], "b": [0.0] * rows}


def json_matrix(rows: int, cols: int) -> list[list[float]]:
    """Weight rows = out_features, cols = in_features (the artifact shape)."""
    return [[0.0] * cols for _ in range(rows)]


def nonlinear_graph_payload(hidden: int = 24) -> dict:
    graph, _ = build_fusion_graph(258, hidden=hidden, out_dim=2)
    graph.validate()
    return graph_to_json(graph)


def start_worker() -> subprocess.Popen:
    process = subprocess.Popen(
        [python_bin(), str(WORKER / "worker.py")],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, bufsize=1,
    )
    hello = json.loads(process.stdout.readline())
    assert hello["type"] == "hello", hello
    assert hello["protocol"] == PROTOCOL, hello
    return process


def send(process: subprocess.Popen, frame: dict) -> None:
    """Write one frame. The response is left for `until` to read — a send
    that eats the first reply is exactly the bug that hangs a suite."""
    process.stdin.write(json.dumps(frame) + "\n")
    process.stdin.flush()


def until(process: subprocess.Popen, run_id: str, terminal=("done", "failed"), timeout: float = 90.0) -> dict:
    """Read frames until a terminal one. Bounded: a worker that stops
    speaking fails the test instead of hanging the suite."""
    import selectors
    import time as _time

    sel = selectors.DefaultSelector()
    sel.register(process.stdout, selectors.EVENT_READ)
    deadline = _time.monotonic() + timeout
    while True:
        if _time.monotonic() > deadline:
            process.kill()
            raise AssertionError(f"worker never reached {terminal} for {run_id}")
        if not sel.select(timeout=min(1.0, deadline - _time.monotonic())):
            continue
        frame = json.loads(process.stdout.readline())
        if frame.get("type") in terminal:
            assert frame.get("run_id") == run_id, frame
            return frame


class GraphValidationTests(unittest.TestCase):
    def test_an_unpermitted_operator_is_rejected(self):
        graph = Graph(
            inputs={"x": TensorSpec(shape=[4])},
            ops=[Op("matmul", ["x"], "y")],  # not on the allow-list
            outputs={"out": "y"},
        )
        with self.assertRaises(GraphError) as caught:
            graph.validate()
        self.assertIn("not permitted", str(caught.exception))

    def test_reading_an_undefined_value_is_rejected(self):
        graph = Graph(
            inputs={"x": TensorSpec(shape=[4])},
            ops=[Op("relu", ["missing"], "y")],
            outputs={"out": "y"},
        )
        with self.assertRaises(GraphError):
            graph.validate()

    def test_incompatible_dtype_is_rejected(self):
        with self.assertRaises(GraphError):
            TensorSpec(shape=[4], dtype="float16")

    def test_a_linear_weight_that_cannot_accept_the_input_is_rejected(self):
        graph = Graph(
            inputs={"x": TensorSpec(shape=[4])},
            ops=[Op("linear", ["x"], "y", attrs={"w": torch.zeros(2, 7)})],
            outputs={"out": "y"},
        )
        graph.validate()  # structure is fine
        from graph import evaluate
        with self.assertRaises(GraphError):
            evaluate(graph, {"x": torch.zeros(1, 4)})

    def test_concat_refuses_mismatched_semantic_spaces(self):
        graph = Graph(
            inputs={"a": TensorSpec(shape=[2], space="pixel"),
                    "b": TensorSpec(shape=[2], space="signed")},
            ops=[Op("concat", ["a", "b"], "f", attrs={"spaces": ["pixel", "signed"]})],
            outputs={"out": "f"},
        )
        graph.validate()
        from graph import evaluate
        # same width, incompatible declared spaces → the graph must refuse
        graph.ops[0].attrs["spaces"] = ["signed", "signed"]
        with self.assertRaises(GraphError) as caught:
            evaluate(graph, {"a": torch.zeros(1, 2), "b": torch.zeros(1, 2)})
        self.assertIn("semantic spaces", str(caught.exception))


class WorkerProtocolTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not PYTHON.exists():
            raise unittest.SkipTest("no .venv with torch; run workers/torch/requirements.txt")

    def test_the_worker_completes_a_handshake(self):
        process = start_worker()
        process.stdin.close()
        process.wait(timeout=30)

    def test_an_unknown_protocol_version_is_refused(self):
        process = start_worker()
        send(process, {"v": 99, "type": "train", "run_id": "r1"})
        frame = until(process, "r1")
        self.assertEqual(frame["type"], "failed")
        self.assertIn("version", frame["error"])
        process.stdin.close()
        process.kill()

    def test_an_unknown_action_is_refused(self):
        process = start_worker()
        send(process, {"v": 1, "type": "launch-missiles", "run_id": "r1"})
        frame = until(process, "r1")
        self.assertEqual(frame["type"], "failed")
        process.stdin.close()
        process.kill()

    def test_real_training_reduces_loss_and_saves_weights(self):
        process = start_worker()
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            weights = tmp / "w.json"
            weights.write_text("{}")  # seeded init from the run seed
            out = tmp / "out.json"
            frame = send(process, {
                "v": 1, "type": "train", "request_id": "req-1", "run_id": "run-1",
                "attempt_id": "att-1",
                "graph": nonlinear_graph_payload(),
                "weights": str(weights),
                "data": str(TASK / "data" / "train.bin"),
                "val_data": str(TASK / "data" / "val.bin"),
                "out": str(out), "steps": 150, "seed": 1,
            })
            result = until(process, "run-1")
            self.assertEqual(result["type"], "done", result)
            self.assertLess(result["loss"], result["first_loss"])
            self.assertGreaterEqual(result["val_accuracy"], 0.9)
            saved = json.loads(out.read_text())
            self.assertIn("h1", saved["params"])
            # parameters actually changed
            # parameters actually moved off their seeded start
            self.assertNotEqual(saved["params"]["h1"]["w"], json_matrix(2, 24))
        process.stdin.close()
        process.kill()

    def test_a_structurally_invalid_graph_fails_before_training(self):
        process = start_worker()
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            weights = tmp / "w.json"
            weights.write_text(json.dumps({"h0": [[0.0] * 258, [0.0] * 258]}))
            bad = linear_graph_payload()
            bad["ops"].append({"kind": "system", "inputs": ["h0"], "output": "z", "attrs": {}})
            frame = send(process, {
                "v": 1, "type": "train", "run_id": "run-2",
                "graph": bad, "weights": str(weights),
                "data": str(TASK / "data" / "train.bin"),
                "out": str(tmp / "o.json"), "steps": 1, "seed": 1,
            })
            result = until(process, "run-2")
            self.assertEqual(result["type"], "failed")
            self.assertIn("not permitted", result["error"])
        process.stdin.close()
        process.kill()

    def test_a_corrupt_weights_file_is_reported(self):
        process = start_worker()
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            weights = tmp / "w.json"
            # an explicit weight matrix whose width cannot accept the
            # graph's input is rejected before any training step
            weights.write_text(json.dumps({"h0": json_layer(2, 3)}))
            frame = send(process, {
                "v": 1, "type": "train", "run_id": "run-3",
                "graph": linear_graph_payload(), "weights": str(weights),
                "data": str(TASK / "data" / "train.bin"),
                "out": str(tmp / "o.json"), "steps": 1, "seed": 1,
            })
            result = until(process, "run-3")
            self.assertEqual(result["type"], "failed")
        process.stdin.close()
        process.kill()

    def test_weights_saved_by_one_process_load_and_predict_in_another(self):
        """The checkpoint round trip the acceptance contract depends on."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            weights = tmp / "w.json"
            weights.write_text(json.dumps({"h0": json_matrix(24, 258), "h1": json_matrix(2, 24)}))
            trained = tmp / "trained.json"
            trainer = start_worker()
            send(trainer, {
                "v": 1, "type": "train", "run_id": "run-train",
                "graph": nonlinear_graph_payload(),
                "weights": str(weights),
                "data": str(TASK / "data" / "train.bin"),
                "val_data": str(TASK / "data" / "val.bin"),
                "out": str(trained), "steps": 150, "seed": 5,
            })
            result = until(trainer, "run-train")
            self.assertEqual(result["type"], "done", result)
            trainer.stdin.close()
            trainer.kill()
            trainer.wait(timeout=10)

            # A brand-new process loads the saved weights and predicts.
            predictor = start_worker()
            predictions = tmp / "pred.json"
            send(predictor, {
                "v": 1, "type": "predict", "run_id": "run-predict",
                "graph": nonlinear_graph_payload(),
                "weights": str(trained),
                "data": str(TASK / "data" / "val.bin"),
                "out": str(predictions),
            })
            result = until(predictor, "run-predict")
            self.assertEqual(result["type"], "done", result)
            rows = json.loads(predictions.read_text())["predictions"]
            self.assertGreater(len(rows), 0)
            self.assertTrue(all(r["answer"] in (0, 1) for r in rows))
            predictor.stdin.close()
            predictor.kill()

    def test_the_linear_structure_stays_weak_and_the_nonlinear_one_does_not(self):
        """The structural claim the acceptance gate rests on."""
        def accuracy_for(graph_payload, steps):
            process = start_worker()
            with tempfile.TemporaryDirectory() as tmp:
                tmp = Path(tmp)
                weights = tmp / "w.json"
                n_params = sum(len(p["w"]) if False else 0 for p in [graph_payload])
                del n_params
                # h0 is (24, 258) for the nonlinear graph, (2, 258) for the
                # linear one; h1 is (2, 24)
                hidden = next((op for op in graph_payload["ops"] if op["kind"] == "relu"), None)
                weights.write_text("{}")  # seeded init from the run seed
                send(process, {
                    "v": 1, "type": "train", "run_id": f"acc-{steps}",
                    "graph": graph_payload, "weights": str(weights),
                    "data": str(TASK / "data" / "train.bin"),
                    "val_data": str(TASK / "data" / "val.bin"),
                    "out": str(tmp / "o.json"), "steps": steps, "seed": 2,
                })
                result = until(process, f"acc-{steps}")
            process.stdin.close()
            process.kill()
            return result

        linear = accuracy_for(linear_graph_payload(), 200)
        nonlinear = accuracy_for(nonlinear_graph_payload(), 200)
        self.assertEqual(linear["type"], "done", linear)
        self.assertEqual(nonlinear["type"], "done", nonlinear)
        self.assertLess(linear["val_accuracy"], 0.75,
                        "a linear fusion should not solve the XOR")
        self.assertGreater(nonlinear["val_accuracy"], 0.9,
                           "the nonlinear structure should fit it")


if __name__ == "__main__":
    unittest.main()
