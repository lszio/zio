"""CPU contracts: run python -m unittest discover -s contribs/tensor/tests."""
import copy
import importlib.util
import json
import pathlib
import subprocess
import sys
import unittest

SPEC = importlib.util.spec_from_file_location("tensor_backend", pathlib.Path(__file__).parents[1] / "backend.py")
backend = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(backend)


class TensorContracts(unittest.TestCase):
    def create(self, b, name, data, shape, grad=False, dtype="float64"):
        return b.call({"op": "create", "out": name, "data": data, "shape": shape,
                       "dtype": dtype, "requires_grad": grad})

    def model(self):
        b = backend.Backend()
        self.create(b, "x", [1, 2, -1, -2], [2, 2])
        self.create(b, "w", [.1, -.2, -.1, .2], [2, 2], True)
        self.create(b, "bias", [.2, -.2], [2], True)
        self.create(b, "frozen", [7], [1], False)
        self.create(b, "y", [0, 1], [2], dtype="int64")
        b.call({"op": "adam-create", "name": "opt", "parameters": ["w", "bias", "frozen"], "lr": .05})
        return b

    def step(self, b):
        b.call({"op": "zero-grad", "inputs": ["w", "bias", "frozen"]})
        b.call({"op": "linear", "inputs": ["x", "w", "bias"], "out": "logits"})
        b.call({"op": "cross-entropy", "inputs": ["logits", "y"], "reduction": "mean", "out": "loss"})
        loss = b.call({"op": "value", "input": "loss"})["data"][0]
        b.call({"op": "backward", "input": "loss"})
        b.call({"op": "adam-step", "optimizer": "opt"})
        b.call({"op": "drop", "inputs": ["logits", "loss"]})
        return loss

    def test_weight_bias_gradients_and_frozen_state(self):
        b = self.model()
        w = b.call({"op": "value", "input": "w"})
        bias = b.call({"op": "value", "input": "bias"})
        first = self.step(b)
        self.assertNotEqual(w, b.call({"op": "value", "input": "w"}))
        self.assertNotEqual(bias, b.call({"op": "value", "input": "bias"}))
        state = b.call({"op": "state-export"})
        self.assertEqual(state["tensors"]["frozen"]["data"], [7.])
        self.assertNotIn("frozen", state["optimizers"]["opt"]["state"])
        for _ in range(15):
            last = self.step(b)
        self.assertLess(last, first)
        b.call({"op": "freeze", "input": "bias", "frozen": True})
        frozen = b.call({"op": "state-export"})
        self.step(b)
        after = b.call({"op": "state-export"})
        self.assertEqual(frozen["tensors"]["bias"], after["tensors"]["bias"])
        self.assertEqual(frozen["optimizers"]["opt"]["state"]["bias"], after["optimizers"]["opt"]["state"]["bias"])

    def test_json_state_adam_and_rng_continue_exactly(self):
        b = self.model()
        b.call({"op": "seed", "seed": 417})
        self.step(b)
        state = json.loads(json.dumps(b.call({"op": "state-export"}), allow_nan=False))
        restored = backend.Backend()
        restored.call({"op": "state-import", "state": state})
        for item in (b, restored):
            item.call({"op": "randperm", "n": 17, "out": "order"})
            self.step(item)
        self.assertEqual(b.call({"op": "value", "input": "order"}), restored.call({"op": "value", "input": "order"}))
        self.assertEqual(b.call({"op": "state-export"}), restored.call({"op": "state-export"}))

    def test_bad_state_is_atomic_and_shapes_are_checked(self):
        b = self.model()
        self.step(b)
        before = b.call({"op": "state-export"})
        for mutate in (
            lambda s: s["tensors"]["bias"].update(shape=[3]),
            lambda s: s["tensors"]["w"]["data"].__setitem__(0, float("nan")),
            lambda s: s["optimizers"]["opt"]["state"]["w"]["exp_avg_sq"]["data"].__setitem__(0, -1),
            lambda s: s.update(rng=[1, 2]),
        ):
            bad = copy.deepcopy(before)
            mutate(bad)
            with self.assertRaises(backend.TensorError):
                b.call({"op": "state-import", "state": bad})
            self.assertEqual(before, b.call({"op": "state-export"}))
        with self.assertRaises(backend.TensorError):
            self.create(b, "bad", [1, 2, 3], [2, 2])
        with self.assertRaises(backend.TensorError):
            b.call({"op": "linear", "inputs": ["x", "bias"], "out": "bad"})
        self.assertNotIn("bad", b.tensors)

    def test_preference_and_policy_kernels_have_real_gradients(self):
        b = backend.Backend()
        self.create(b, "scores", [.3, -.4], [1, 2], True)
        self.create(b, "pick", [0], [1, 1], dtype="int64")
        b.call({"op": "log-softmax", "inputs": ["scores"], "out": "lp"})
        b.call({"op": "gather", "inputs": ["lp", "pick"], "out": "chosen"})
        b.call({"op": "mean", "inputs": ["chosen"], "out": "loss"})
        b.call({"op": "backward", "input": "loss"})
        b.call({"op": "grad", "input": "scores", "out": "gradient"})
        gradient = b.call({"op": "value", "input": "gradient"})["data"]
        self.assertGreater(gradient[0], 0)
        self.assertLess(gradient[1], 0)
        self.assertAlmostEqual(sum(gradient), 0)

    def test_protocol_correlates_and_survives_math_rejection(self):
        frames = [{"id": 1, "operation": {"op": "create", "out": "a", "data": [1], "shape": []}},
                  {"id": 2, "operation": {"op": "log", "inputs": ["a"], "out": "l"}},
                  {"id": 3, "operation": {"op": "create", "out": "a", "data": [2]}},
                  {"id": 4, "operation": {"op": "value", "input": "a"}}]
        result = subprocess.run([sys.executable, str(pathlib.Path(__file__).parents[1] / "backend.py")],
                                input="".join(json.dumps(f) + "\n" for f in frames), text=True,
                                capture_output=True, timeout=30, check=True)
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual([r["id"] for r in replies], [1, 2, 3, 4])
        self.assertFalse(replies[2]["ok"])
        self.assertEqual(replies[3]["result"]["data"], [1.])


if __name__ == "__main__":
    unittest.main()
