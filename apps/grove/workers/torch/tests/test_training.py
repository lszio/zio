"""W04 worker training contract: real gradient descent, not a simulation.

These run on CPU and take a few seconds. They prove the properties the
acceptance task depends on:

  * a parameter update actually lowers the held-out loss and changes the
    predictions (training is real);
  * the linear baseline genuinely cannot fit the XOR, which is what makes
    the structural change measurable;
  * a missing modality is masked out of the loss, so it is never trained
    as class 0;
  * training is reproducible under a fixed seed within a declared tolerance;
  * a broken weights file is a visible error, not a confident guess.

    python -m unittest discover -s apps/grove/workers/torch/tests -p test_training.py
"""

from __future__ import annotations

import json
import math
import sys
import unittest
from pathlib import Path

import torch

ROOT = Path(__file__).resolve().parents[5]  # repo root
WORKER = ROOT / "apps" / "grove" / "workers" / "torch"
TASK = ROOT / "examples" / "self-learning"

sys.path.insert(0, str(WORKER))
from data import load_split  # noqa: E402
from train import (  # noqa: E402
    LinearFusion,
    NonlinearFusion,
    accuracy,
    build,
    features,
    train,
)


def _data():
    rows = load_split(TASK / "data" / "val.bin")
    return build(rows)


class TrainingTests(unittest.TestCase):
    def setUp(self):
        torch.use_deterministic_algorithms(True)
        torch.set_num_threads(1)
        self.xtr, self.ytr, self.dtr = build(load_split(TASK / "data" / "train.bin"))
        self.xva, self.yva, self.dva = _data()

    def test_parameters_actually_move_and_loss_falls(self):
        model = NonlinearFusion()
        before = model(self.xtr[:64]).detach().clone()
        losses = train(model, self.xtr, self.ytr, self.dtr, steps=60, lr=0.02, seed=1)
        after = model(self.xtr[:64]).detach()
        self.assertFalse(torch.allclose(before, after), "training changed nothing")
        self.assertLess(losses[-1], losses[0] * 0.6, "loss did not meaningfully fall")

    def test_linear_baseline_cannot_fit_the_xor(self):
        # The structural claim the acceptance gate rests on: even a
        # well-trained linear fusion plateaus on this task.
        model = LinearFusion()
        train(model, self.xtr, self.ytr, self.dtr, steps=200, lr=0.05, seed=1)
        acc, n = accuracy(model, self.xva, self.yva, self.dva)
        self.assertGreater(n, 0)
        self.assertLess(acc, 0.75, "a linear model should not solve an XOR")

    def test_nonlinear_structure_learns_the_task(self):
        model = NonlinearFusion()
        train(model, self.xtr, self.ytr, self.dtr, steps=300, lr=0.02, seed=1)
        acc, n = accuracy(model, self.xva, self.yva, self.dva)
        self.assertGreater(acc, 0.9, f"nonlinear model underfit: {acc}")

    def test_missing_modality_is_excluded_from_the_loss(self):
        # Build a batch that is entirely undecidable: loss must be 0 and
        # gradients must not be shaped by a fabricated class 0.
        rows = load_split(TASK / "data" / "val.bin")
        missing = [r for r in rows if not r.decidable]
        self.assertGreater(len(missing), 0)
        x, y, decidable = build(missing)
        self.assertTrue(all(not d for d in decidable.tolist()))
        model = NonlinearFusion()
        before = model(x).detach().clone()
        train(model, x, y, decidable, steps=5, lr=0.1, seed=1)
        after = model(x).detach()
        self.assertTrue(
            torch.allclose(before, after, atol=1e-6),
            "abstained rows must not be trained toward a class",
        )

    def test_training_is_reproducible_within_tolerance(self):
        def run():
            torch.manual_seed(7)  # covers the parameter init, not just the batches
            model = NonlinearFusion()
            train(model, self.xtr, self.ytr, self.dtr, steps=40, lr=0.02, seed=7)
            return model(self.xva).detach()

        a, b = run(), run()
        self.assertTrue(
            torch.allclose(a, b, atol=1e-5),
            "same seed must reproduce the same predictions on CPU",
        )

    def test_features_encode_presence_separately_from_content(self):
        rows = load_split(TASK / "data" / "val.bin")
        missing = next(r for r in rows if not r.image_present)
        present = next(r for r in rows if r.image_present and r.numeric_present)
        f_missing, f_present = features(missing), features(present)
        # exactly one modality is absent, and the flag says which
        self.assertNotEqual(f_missing[-2], f_missing[-1])
        self.assertEqual(f_present[-2], 1.0)
        self.assertEqual(f_present[-1], 1.0)

    def test_a_broken_weights_file_is_a_visible_error(self):
        import teacher
        from train import export_weights

        torch.manual_seed(5)
        trained = NonlinearFusion()
        train(trained, self.xtr, self.ytr, self.dtr, steps=10, lr=0.02, seed=5)
        good = export_weights(trained)  # already {"hidden": {...}}
        teacher.TeacherHandler.weights = good
        # A NaN in the frozen weights must not become a confident label.
        broken = json.loads(json.dumps(good))
        broken["hidden"]["w2"][0][0] = math.nan
        teacher.TeacherHandler.weights = broken
        try:
            p0, p1 = teacher.forward(broken, [0.5] * 260)
            self.assertTrue(math.isnan(p0) or math.isnan(p1),
                            "a NaN weight must not yield a confident label")
        finally:
            teacher.TeacherHandler.weights = good

    def test_frozen_weights_file_round_trips(self):
        model = NonlinearFusion()
        train(model, self.xtr, self.ytr, self.dtr, steps=20, lr=0.02, seed=3)
        payload = {
            "model_version": "test-1",
            "weights": __import__("train").export_weights(model),
        }
        import tempfile
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump(payload, handle)
            path = Path(handle.name)
        import teacher
        loaded = json.loads(path.read_text())
        x = [0.0] * 260
        p0, p1 = teacher.forward(loaded["weights"], x)
        self.assertAlmostEqual(p0 + p1, 1.0, places=5, msg="softmax must normalize")
        path.unlink()


if __name__ == "__main__":
    unittest.main()
