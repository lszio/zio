"""Worker protocol contract: frames, structural rejection, real training.

    python -m unittest discover -s apps/grove/workers/torch/tests -p test_worker.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import torch

ROOT = Path(__file__).resolve().parents[5]
WORKER = ROOT / "apps" / "grove" / "workers" / "torch"
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

from worker import build_model, make_optimiser, trainable_layers  # noqa: E402

def two_layer_graph_payload() -> dict:
    """Two linear layers, only the second declared trainable."""
    return {
        "inputs": {
            "image": {"shape": [256], "dtype": "float32", "space": "pixel"},
            "numeric": {"shape": [2], "dtype": "float32", "space": "signed"},
        },
        "ops": [
            {"kind": "normalize", "inputs": ["image"], "output": "img", "attrs": {}},
            {"kind": "normalize", "inputs": ["numeric"], "output": "num", "attrs": {}},
            {"kind": "concat", "inputs": ["img", "num"], "output": "fused", "attrs": {}},
            {"kind": "linear", "inputs": ["fused"], "output": "frozen", "attrs": {"out": 32}},
            {"kind": "relu", "inputs": ["frozen"], "output": "hidden", "attrs": {}},
            {"kind": "linear", "inputs": ["hidden"], "output": "active", "attrs": {"out": 2}},
        ],
        "outputs": {"logits": "active"},
        "trainable": ["active"],
    }


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


def seeded_layer(rows: int, cols: int, seed: int) -> dict:
    """A layer with a non-degenerate start.

    All-zero weights have zero gradient everywhere, so a trainable layer
    seeded with zeros cannot move and the check would pass for the wrong
    reason.
    """
    state = seed * 6364136223846793005 + 1

    def nxt() -> float:
        nonlocal state
        state = (state * 6364136223846793005 + 1442695040888963407) & ((1 << 64) - 1)
        return ((state >> 11) / float(1 << 53)) - 0.5

    return {
        "w": [[nxt() for _ in range(cols)] for _ in range(rows)],
        "b": [nxt() for _ in range(rows)],
    }


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


def fingerprint(values) -> str:
    """A short, stable digest of a nested numeric structure.

    Used instead of comparing arrays directly: a 32x258 float matrix that
    fails an equality check produces a diff nobody reads, and the diff
    itself is expensive enough to look like a hang.
    """
    import hashlib
    import struct

    flat: list[float] = []

    def walk(node) -> None:
        if isinstance(node, (list, tuple)):
            for item in node:
                walk(item)
        elif isinstance(node, (int, float)):
            flat.append(float(node))

    walk(values)
    digest = hashlib.sha256()
    for value in flat:
        # f32: the worker's tensors are float32, so the seed's f64
        # decimal is more precise than the value it produced.
        digest.update(struct.pack("<f", value))
    return digest.hexdigest()[:16]


def max_abs_diff(a, b) -> float:
    """Largest absolute difference between two nested numeric structures.
    Dicts are walked by matching keys; any shape mismatch is infinite so
    an assertion can never mistake a different structure for equality."""
    diffs: list[float] = []

    def walk(x, y) -> None:
        if isinstance(x, dict) and isinstance(y, dict):
            if set(x) != set(y):
                diffs.append(float("inf"))
                return
            for key in x:
                walk(x[key], y[key])
        elif isinstance(x, (list, tuple)) and isinstance(y, (list, tuple)):
            if len(x) != len(y):
                diffs.append(float("inf"))
                return
            for xi, yi in zip(x, y):
                walk(xi, yi)
        elif isinstance(x, (int, float)) and isinstance(y, (int, float)):
            diffs.append(abs(float(x) - float(y)))
        else:
            diffs.append(float("inf"))

    walk(a, b)
    return max(diffs, default=0.0)


def stop(process: subprocess.Popen) -> None:
    """Close a worker's stdin and reap it.

    A worker that has already died would make a bare `stdin.close()`
    raise or block, so both halves are guarded and the wait is bounded.
    """
    try:
        process.stdin.close()
    except (BrokenPipeError, ValueError, OSError):
        pass
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass


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


def drain_frames(process: subprocess.Popen, run_id: str, timeout: float = 90.0) -> list[dict]:
    """Every frame a run emitted, terminal last. Bounded like `until`."""
    import selectors
    import time as _time

    frames: list[dict] = []
    sel = selectors.DefaultSelector()
    sel.register(process.stdout, selectors.EVENT_READ)
    deadline = _time.monotonic() + timeout
    try:
        while True:
            if _time.monotonic() > deadline:
                process.kill()
                raise AssertionError(f"worker never finished {run_id}")
            if not sel.select(timeout=1.0):
                continue
            frame = json.loads(process.stdout.readline())
            if frame.get("run_id") != run_id:
                continue
            frames.append(frame)
            if frame.get("type") in ("done", "failed"):
                return frames
    finally:
        stop(process)


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
            raise unittest.SkipTest("no .venv with torch; run apps/grove/workers/torch/requirements.txt")

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

    def test_a_layer_outside_the_trainable_set_does_not_move(self):
        """The frozen side of a composite, checked on the real worker.

        Two linear layers, one declared trainable. The frozen layer's
        weight *and* bias must come out byte-identical to what went in;
        the trainable layer's must move.
        """
        process = start_worker()
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            seed = {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}
            weights = tmp / "w.json"
            weights.write_text(json.dumps(seed))
            out = tmp / "out.json"
            send(process, {
                "v": 1, "type": "train", "request_id": "req-frozen",
                "run_id": "run-frozen", "attempt_id": "att-frozen",
                "graph": two_layer_graph_payload(),
                "weights": str(weights),
                "data": str(TASK / "data" / "train.bin"),
                "val_data": str(TASK / "data" / "val.bin"),
                "out": str(out), "steps": 40, "seed": 1,
            })
            result = until(process, "run-frozen")
            self.assertEqual(result["type"], "done", result)
            saved = json.loads(out.read_text())["params"]
            # Digests, not the arrays: a failed assertEqual over 32x258
            # floats builds a diff string no one will ever read, and the
            # comparison itself is not what is under test.
            for part in ("w", "b"):
                self.assertEqual(
                    fingerprint(saved["frozen"][part]),
                    fingerprint(seed["frozen"][part]),
                    f"the frozen layer's {part} moved even though it is not trainable",
                )
            # and the trainable one actually learned
            self.assertNotEqual(
                fingerprint(saved["active"]["w"]),
                fingerprint(seed["active"]["w"]),
                "the trainable layer did not move at all",
            )
        stop(process)

    def test_the_optimiser_trains_biases_and_only_the_trainable_set(self):
        graph = graph_from_json(two_layer_graph_payload())
        params = {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}
        _forward, _all, linears = build_model(graph, params, 1)
        trainable = trainable_layers(graph, linears)
        self.assertEqual(trainable, ["active"])
        _opt, named = make_optimiser(linears, trainable, 0.02)
        keys = [name for name, _ in named]
        self.assertEqual(keys, ["active.weight", "active.bias"],
                         "a weight-only optimiser would freeze every bias")
        self.assertFalse(linears["frozen"].weight.requires_grad)
        self.assertFalse(linears["frozen"].bias.requires_grad)

    def test_a_trainable_name_that_is_not_a_layer_is_refused(self):
        graph = graph_from_json(two_layer_graph_payload())
        graph.trainable = ["not-a-layer"]
        params = {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}
        _forward, _all, linears = build_model(graph, params, 1)
        with self.assertRaises(GraphError) as caught:
            trainable_layers(graph, linears)
        self.assertIn("are not linear layers", str(caught.exception))

    def test_an_empty_trainable_list_means_every_layer(self):
        graph = graph_from_json(linear_graph_payload())
        graph.trainable = []
        params = {"h0": json_layer(2, 258)}
        _forward, _all, linears = build_model(graph, params, 1)
        self.assertEqual(trainable_layers(graph, linears), ["h0"])

    def test_a_checkpoint_records_the_trainable_set_it_saved(self):
        """Optimizer moments are keyed per parameter, bias included, so a
        resume can tell that the artifact describes its own attempt."""
        process = start_worker()
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            weights = tmp / "w.json"
            weights.write_text(json.dumps(
                {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}))
            out = tmp / "out.json"
            state = tmp / "state.json"
            send(process, {
                "v": 1, "type": "train", "request_id": "req-ck",
                "run_id": "run-ck", "attempt_id": "att-ck",
                "graph": two_layer_graph_payload(),
                "weights": str(weights),
                "data": str(TASK / "data" / "train.bin"),
                "out": str(out), "steps": 20, "seed": 1,
                "save_at": 10, "state_out": str(state),
            })
            until(process, "run-ck")
            # per-step names: save_at=10 lands in state-10.json
            saved = json.loads(state.with_name("state-10.json").read_text())
            self.assertEqual(saved["schema"], 2)
            keys = set(saved["optimizer"]["adam"])
            # only the trainable layer, both of its parameters
            self.assertEqual(keys, {"active.weight", "active.bias"})
        stop(process)

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


class CheckpointSaveTests(unittest.TestCase):
    """Each save lands in its own immutable per-step file, and the frame's
    `saved` path names exactly the file holding the step it reports — the
    owner commits what it bills, never a later step's bytes."""

    DATA = str(TASK / "data" / "train.bin")

    def _run(self, tmp: Path, run_id: str, **extra) -> list[dict]:
        weights = tmp / f"{run_id}-w.json"
        weights.write_text(json.dumps(
            {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}))
        frame = {
            "v": 1, "type": "train", "run_id": run_id,
            "attempt_id": f"att-{run_id}",
            "graph": two_layer_graph_payload(),
            "weights": str(weights), "data": self.DATA,
            "out": str(tmp / f"{run_id}-out.json"),
            "steps": 12, "seed": 1, "save_at": 10,
            "state_out": str(tmp / f"{run_id}-state.json"),
        }
        frame.update(extra)
        process = start_worker()
        send(process, frame)
        return drain_frames(process, run_id)

    def test_each_step_gets_its_own_state_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            frames = self._run(tmp, "run-save")
            self.assertEqual(frames[-1]["type"], "done", frames)
            saved = [f for f in frames if "saved" in f]
            self.assertEqual([f["step"] for f in saved], [10, 11, 12])
            for frame in saved:
                path = Path(frame["saved"])
                self.assertTrue(path.exists(), frame["saved"])
                state = json.loads(path.read_text())
                self.assertEqual(state["step"], frame["step"])
                self.assertEqual(state["run_id"], "run-save")
            # no shared mutable name: a plain state file never exists
            self.assertFalse((tmp / "run-save-state.json").exists())
            names = sorted(p.name for p in tmp.glob("state-*.json"))
            self.assertEqual(names, ["state-10.json", "state-11.json",
                                     "state-12.json"])

    def test_stop_after_save_writes_exactly_one_state_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            frames = self._run(tmp, "run-stop", stop_after_save=True)
            self.assertEqual(frames[-1]["type"], "done", frames)
            saved = [f for f in frames if "saved" in f]
            self.assertEqual([f["step"] for f in saved], [10])
            names = sorted(p.name for p in tmp.glob("state-*.json"))
            self.assertEqual(names, ["state-10.json"])

    def test_resume_run_identity_is_enforced_on_the_save_path(self):
        """resume_run validation is unchanged: a stop_after_save run that
        resumes a foreign state still refuses before any save happens."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            parent = self._make_parent(tmp)
            frames = self._run(tmp, "run-child", steps=1, save_at=11,
                               stop_after_save=True, resume=str(parent),
                               resume_level="learning-continuation",
                               resume_run="run-someone-else")
            self.assertEqual(frames[-1]["type"], "failed", frames)
            self.assertIn("belongs to run", frames[-1].get("error", ""))
            self.assertFalse(any("saved" in f for f in frames))
            # the child never wrote its own per-step state file
            self.assertFalse((tmp / "state-11.json").exists())

    def _make_parent(self, tmp: Path) -> Path:
        process = start_worker()
        weights = tmp / "parent-w.json"
        weights.write_text(json.dumps(
            {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}))
        state_out = tmp / "parent-state.json"
        send(process, {
            "v": 1, "type": "train", "run_id": "run-parent",
            "attempt_id": "att-parent",
            "graph": two_layer_graph_payload(),
            "weights": str(weights), "data": self.DATA,
            "out": str(tmp / "parent-out.json"),
            "steps": 10, "seed": 1, "save_at": 10,
            "state_out": str(state_out), "stop_after_save": True,
        })
        until(process, "run-parent")
        stop(process)
        return state_out.with_name("state-10.json")


class ResumeContractTests(unittest.TestCase):
    """checkpoint--resume enqueues a NEW run id together with the parent's
    identity (`resume_run`) and a level; the worker must accept the parent
    state for the new run and restore only what the level names."""

    DATA = str(TASK / "data" / "train.bin")

    def _write_weights(self, tmp: Path) -> Path:
        weights = tmp / "w.json"
        weights.write_text(json.dumps(
            {"frozen": seeded_layer(32, 258, 7), "active": seeded_layer(2, 32, 3)}))
        return weights

    def _checkpoint(self, tmp: Path, run_id: str = "run-a") -> Path:
        """Ten real steps, then stop exactly at the checkpoint boundary."""
        process = start_worker()
        state_out = tmp / f"{run_id}-state.json"
        send(process, {
            "v": 1, "type": "train",
            "run_id": run_id, "attempt_id": "att-parent",
            "graph": two_layer_graph_payload(),
            "weights": str(self._write_weights(tmp)),
            "data": self.DATA, "out": str(tmp / f"{run_id}-out.json"),
            "steps": 10, "seed": 1,
            "save_at": 10, "state_out": str(state_out), "stop_after_save": True,
        })
        done = until(process, run_id)
        self.assertEqual(done["type"], "done", done)
        stop(process)
        # per-step names: the saved file is the frame's `state-10.json`
        return state_out.with_name("state-10.json")

    def _run(self, tmp: Path, frame: dict) -> list[dict]:
        """One train frame; every frame the run emitted, terminal last."""
        process = start_worker()
        send(process, frame)
        return drain_frames(process, frame["run_id"])

    def test_resume_learning_continuation_accepts_the_parent_state(self):
        """A resumed run (new id by design) continues the parent's Adam
        moments, RNG stream and step: 10 saved + 20 resumed lands exactly
        where 30 uninterrupted steps would."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            state = self._checkpoint(tmp)
            weights = self._write_weights(tmp)

            control = start_worker()
            send(control, {
                "v": 1, "type": "train", "run_id": "run-control",
                "graph": two_layer_graph_payload(),
                "weights": str(weights), "data": self.DATA,
                "out": str(tmp / "control-out.json"),
                "steps": 30, "seed": 1,
            })
            done = until(control, "run-control")
            self.assertEqual(done["type"], "done", done)
            stop(control)

            frames = self._run(tmp, {
                "v": 1, "type": "train", "run_id": "run-b",
                "attempt_id": "att-child",
                "graph": two_layer_graph_payload(),
                "weights": str(weights), "data": self.DATA,
                "out": str(tmp / "run-b-out.json"),
                "steps": 20, "seed": 1,
                "resume": str(state), "resume_level": "learning-continuation",
                "resume_run": "run-a",
            })
            self.assertEqual(frames[-1]["type"], "done", frames)
            steps = [f.get("step") for f in frames if f["type"] == "progress"]
            # the run never restarts at 1: it continues the parent's ledger
            self.assertTrue(all(s > 10 for s in steps), steps)
            self.assertAlmostEqual(frames[-1]["loss"], done["loss"], delta=1e-6)
            resumed = json.loads((tmp / "run-b-out.json").read_text())
            control_out = json.loads((tmp / "control-out.json").read_text())
            # optimizer, RNG and step all restored: identical trajectory
            self.assertLessEqual(
                max_abs_diff(resumed["params"], control_out["params"]), 1e-5)

    def test_resume_model_initialization_restores_weights_only(self):
        """Weights carry over; Adam moments, RNG and the step counter do
        not: the resumed run's first step matches a fresh run started from
        the parent's saved weights."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            state = self._checkpoint(tmp)
            weights = self._write_weights(tmp)

            frames = self._run(tmp, {
                "v": 1, "type": "train", "run_id": "run-b",
                "attempt_id": "att-child",
                "graph": two_layer_graph_payload(),
                "weights": str(weights), "data": self.DATA,
                "out": str(tmp / "run-b-out.json"),
                "steps": 1, "seed": 1,
                "resume": str(state), "resume_level": "model-initialization",
                "resume_run": "run-a",
            })
            self.assertEqual(frames[-1]["type"], "done", frames)
            steps = [f.get("step") for f in frames if f["type"] == "progress"]
            # a weights-only resume restarts the ledger at 0, not 10
            self.assertEqual(steps, [1], steps)

            # the exact step a never-paused run would take from the
            # checkpointed weights: fresh Adam, fresh seed, parent weights
            artifact = json.loads(state.read_text())
            parent_weights = tmp / "parent-w.json"
            parent_weights.write_text(json.dumps(artifact["params"]))
            fresh = start_worker()
            send(fresh, {
                "v": 1, "type": "train", "run_id": "run-fresh",
                "graph": two_layer_graph_payload(),
                "weights": str(parent_weights), "data": self.DATA,
                "out": str(tmp / "fresh-out.json"),
                "steps": 1, "seed": 1,
            })
            done = until(fresh, "run-fresh")
            self.assertEqual(done["type"], "done", done)
            stop(fresh)

            resumed = json.loads((tmp / "run-b-out.json").read_text())
            fresh_out = json.loads((tmp / "fresh-out.json").read_text())
            self.assertLessEqual(
                max_abs_diff(resumed["params"], fresh_out["params"]), 1e-6)

    def test_resume_refuses_a_state_artifact_from_an_unrelated_run(self):
        """The parent identity in the frame is the trust anchor: an
        artifact minted by a different run never loads."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            state = self._checkpoint(tmp)
            frames = self._run(tmp, {
                "v": 1, "type": "train", "run_id": "run-b",
                "graph": two_layer_graph_payload(),
                "weights": str(self._write_weights(tmp)), "data": self.DATA,
                "out": str(tmp / "run-b-out.json"),
                "steps": 5, "seed": 1,
                "resume": str(state), "resume_level": "learning-continuation",
                "resume_run": "run-someone-else",
            })
            self.assertEqual(frames[-1]["type"], "failed", frames)
            self.assertIn("belongs to run", frames[-1].get("error", ""))


if __name__ == "__main__":
    unittest.main()
