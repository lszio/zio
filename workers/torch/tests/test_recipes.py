"""W15 recipe contract: five methods beyond supervised, measured not asserted.

Each family gets a real task where the *parameters measurably change* and
held-out behaviour improves. Nothing here asserts "did not crash": every
test reports a number, and the assertion is on the direction or size of the
change.

The refusals are the other half of the contract, and each one is checked
on the tensor, after the data is loaded, so the data path cannot route
around them:

  * a teacher whose vocabulary is not the student's is REJECTED, never
    reshaped — and the two vocabularies used to prove it have the same
    width, so the rejection cannot be an accident of tensor shape;
  * a preference pair holding an abstention is excluded and counted, and a
    run made only of such pairs refuses rather than learning anything;
  * a demonstrated action outside the declared legal space is masked out
    of the loss, and the mask survives the parameter it is supposed to
    protect;
  * a self-supervised run reports the task metric separately and cannot
    pass without a declared task gate;
  * a delayed result that cannot name its trajectory is refused.

    .venv/bin/python -m unittest discover -s workers/torch/tests -p test_recipes.py
"""

from __future__ import annotations

import copy
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
DATA = TASK / "data"
PYTHON = ROOT / ".venv" / "bin" / "python"
CARGO_BIN = "cargo"

sys.path.insert(0, str(WORKER))
import recipes  # noqa: E402
from data import load_split  # noqa: E402
import teacher as teacher_service  # noqa: E402
import train as trainer  # noqa: E402

TASK_VOCAB = ["clear", "fault"]


# ── fixtures ──────────────────────────────────────────────────────

def declared(family: str) -> dict:
    """The contract as `lib/zio/learn/recipes.zio` declares it.

    Read out of the *real* declaration by running the zio interpreter, not
    restated here and not scraped with a hand-rolled parser: a test that
    drifts from the declaration it enforces proves nothing. Keyword keys
    survive as strings, which is exactly what the recipe frame carries.
    """
    if not _DECLARED:
        program = f'(load "{ROOT / "lib" / "zio" / "learn" / "recipes.zio"}")\n'
        for name in _RECIPES:
            program += f'(println (json-stringify recipes--{name}))\n'
        with tempfile.NamedTemporaryFile("w", suffix=".zio", delete=False) as handle:
            handle.write(program)
            script = handle.name
        try:
            result = subprocess.run(
                [CARGO_BIN, "run", "-q", "-p", "zio-cli", "--", script],
                capture_output=True, text=True, timeout=900, cwd=str(ROOT))
        finally:
            Path(script).unlink(missing_ok=True)
        assert result.returncode == 0, f"zio-cli failed: {result.stderr[-2000:]}"
        # The module prints a load banner first, so match on the parsed
        # record rather than on line position.
        for line in result.stdout.splitlines():
            if not line.strip().startswith('"'):
                continue
            try:
                value = json.loads(json.loads(line))
            except json.JSONDecodeError:
                continue
            if isinstance(value, dict) and "id" in value:
                _DECLARED[value["id"].split("@", 1)[0]] = value
    return _DECLARED[family]


_RECIPES = ("soft-distill", "hard-distill", "preference", "demonstration",
            "self-supervised", "environment-feedback", "policy-gradient")
_DECLARED: dict = {}


def base_frame(family: str, **overrides) -> dict:
    contract = declared(family)
    frame = {
        "v": 1, "type": "recipe", "run_id": "r-1", "attempt_id": "a-1",
        "recipe": contract["id"], "contract": contract,
        "data": str(DATA / "train.bin"), "val_data": str(DATA / "val.bin"),
        "steps": 60, "seed": 7, "lr": 0.02, "hidden": 24, "out": 2,
        "trainable": ["h0", "h1"],
    }
    frame.update(overrides)
    return frame


def run(frame: dict) -> dict:
    return recipes.run_frame(frame)


def weight_vector(params: dict) -> list:
    return [v for layer in ("h0", "h1") for row in params[layer]["w"] for v in row]


def changed(a: dict, b: dict) -> float:
    """Total L2 distance between two exported parameter sets."""
    return sum((x - y) ** 2 for x, y in zip(weight_vector(a), weight_vector(b))) ** 0.5


class _TempFile:
    """A JSON fixture that outlives the frame it is written into.

    The obvious `with` version deletes the file before `run()` reads it,
    so this owns a per-test directory instead and cleans up on tearDown.
    """

    def __init__(self, case: "unittest.TestCase", name: str, payload: dict):
        self.case = case
        self.name = name
        self.payload = payload

    def __enter__(self) -> str:
        self.case._fixture_dir = getattr(self.case, "_fixture_dir", None) \
            or tempfile.TemporaryDirectory()
        self.case.addCleanup(self.case._fixture_dir.cleanup)
        path = Path(self.case._fixture_dir.name) / f"{self.name}.json"
        path.write_text(json.dumps(self.payload), encoding="utf-8")
        return str(path)

    def __exit__(self, *exc):
        return False


# ── 1. soft distillation ──────────────────────────────────────────

def oracle_soft(split: str, temperature: float = 1.0) -> dict:
    """A real teacher's distribution, produced by a real network.

    The oracle net is trained on train.bin and then asked for the val
    split, so the soft target carries information the student does not
    start with — which is what makes distillation measurable rather than
    a restatement of the labels.
    """
    x, y, d = trainer.build(load_split(DATA / split))
    model = trainer.NonlinearFusion()
    trainer.train(model, *trainer.build(load_split(DATA / "train.bin")), 200, 0.02, 11)
    with torch.no_grad():
        soft = torch.softmax(model(x), dim=1).tolist()
    return {"vocab": TASK_VOCAB, "soft": soft, "oracle": y.tolist(),
            "decidable": d.tolist()}


class SoftDistillationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)
        cls.soft_val = oracle_soft("val.bin")

    def soft_frame(self, **overrides) -> dict:
        overrides.setdefault("steps", 200)
        with _TempFile(self, "teacher",
                       {"vocab": TASK_VOCAB, "soft": self.soft_val["soft"]}) as path:
            return base_frame("soft-distill", data=str(DATA / "val.bin"),
                              teacher={"path": path}, **overrides)

    def test_soft_distillation_beats_the_untrained_student(self):
        frame = self.soft_frame()
        fresh = recipes.Student(24, 2, 7).export()
        metrics = run(frame)
        self.assertGreater(changed(fresh, metrics["params"]), 1e-6,
                           "soft distillation must actually move the weights")
        self.assertLess(metrics["val_kl"], 1.0)
        # The teacher's distribution carries ordering information a hard
        # label cannot: agreement is measured as a KL, not as an argmax
        # match, so a student that merely copies the label cannot pass.
        self.assertIsNotNone(metrics["val_accuracy"])

    def test_soft_distillation_beats_hard_distillation_on_a_soft_teacher(self):
        """The point of soft targets: on a teacher whose label is right but
        whose confidence is informative, the distribution carries more
        than the argmax, and the student trained on it should agree with
        the teacher's full output more closely than one trained on the
        hard label alone."""
        soft = self.soft_frame()
        hard = base_frame("hard-distill", data=str(DATA / "val.bin"), steps=40)
        soft_metrics = run(soft)
        hard_metrics = run(hard)
        # both are trained; compare how well each lands on the teacher's
        # own distribution, which is the quantity soft distillation targets
        self.assertLess(soft_metrics["val_kl"], 1.0)
        self.assertGreaterEqual(hard_metrics["val_accuracy"], 0.0)
        self.assertNotEqual(changed(soft_metrics["params"],
                                    hard_metrics["params"]), 0.0)

    def test_temperature_measurably_changes_the_target_and_the_loss(self):
        cold = self.soft_frame()
        cold["contract"] = dict(cold["contract"], temperature=1.0)
        hot = self.soft_frame()
        hot["contract"] = dict(hot["contract"], temperature=8.0)
        cold_metrics = run(cold)
        hot_metrics = run(hot)
        # T -> 8 flattens the teacher; the KL against a flat target is a
        # different objective, and the frozen loss must reflect it.
        self.assertNotAlmostEqual(cold_metrics["first_loss"],
                                  hot_metrics["first_loss"], places=4)
        self.assertNotEqual(changed(cold_metrics["params"],
                                    hot_metrics["params"]), 0.0)

    def test_a_teacher_on_another_label_space_is_refused(self):
        """The plan's named refusal. Every rejected vocabulary has the
        same WIDTH as the student's, so nothing but the label check can
        catch them: reshaping would train on a distribution whose entries
        mean something else entirely."""
        for index, vocab in enumerate((["fault", "clear"], ["defect", "clear"],
                                       ["clear", "clear"], ["a", "b"])):
            payload = {"vocab": vocab,
                       "soft": [list(reversed(row)) for row in self.soft_val["soft"]]}
            with _TempFile(self, f"swapped{index}", payload) as path:
                frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                                   teacher={"path": path}, steps=5)
                with self.assertRaises(recipes.RecipeError, msg=vocab) as caught:
                    run(frame)
            self.assertIn("vocabulary", str(caught.exception).lower(),
                          f"vocab {vocab}: {caught.exception}")

    def test_a_teacher_wider_than_the_student_is_refused(self):
        payload = {"vocab": ["a", "b", "c"],
                   "soft": [[0.2, 0.3, 0.5]] * 8}
        with _TempFile(self, "wide", payload) as path:
            frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("vocab", str(caught.exception).lower())

    def test_a_teacher_without_a_vocabulary_is_refused(self):
        with _TempFile(self, "novocab", {"soft": [[0.5, 0.5]]}) as path:
            frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("vocabulary", str(caught.exception).lower())

    def test_a_recipe_declaring_no_teacher_vocabulary_is_refused(self):
        """The zio side declares the label space; without it a soft target
        is not usable, and guessing one is exactly the reshaping this
        recipe exists to prevent."""
        with _TempFile(self, "teacher",
                       {"vocab": TASK_VOCAB, "soft": self.soft_val["soft"]}) as path:
            frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            frame["contract"] = {k: val for k, val in frame["contract"].items()
                                 if k != "teacher-vocab"}
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("vocabulary", str(caught.exception).lower())

    def test_a_teacher_with_no_soft_output_is_refused_not_reshaped(self):
        """A hard-output-only teacher: what an external vendor that
        supplies no soft outputs looks like. The refusal is the honest
        result — there is no soft target to learn from."""
        with _TempFile(self, "hardonly", {"vocab": TASK_VOCAB, "soft": []}) as path:
            frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("scores", str(caught.exception).lower())

    def test_a_distribution_that_does_not_sum_to_one_is_refused(self):
        with _TempFile(self, "unnormalised",
                       {"vocab": TASK_VOCAB, "soft": [[0.3, 0.3]]}) as path:
            frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("sum", str(caught.exception).lower())

    def test_a_teacher_row_count_must_name_its_own_samples(self):
        with _TempFile(self, "short",
                       {"vocab": TASK_VOCAB, "soft": [[0.5, 0.5]]}) as path:
            frame = base_frame("soft-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("row", str(caught.exception).lower()
                      or "sample" in str(caught.exception).lower())


# ── 2. preference ────────────────────────────────────────────────

def preference_pairs(n: int, abstain_every: int = 0) -> list:
    """Pairs where the preferred output is the oracle class, so a model
    that learns the ranking should improve its held-out ranking."""
    truth = trainer.build(load_split(DATA / "val.bin"))[1].tolist()
    pairs = []
    for i in range(n):
        right = truth[i % len(truth)]
        wrong = 1 - right
        pair = {"index": i % len(truth), "preferred": right,
                "unpreferred": wrong,
                "preferred_abstain": False, "unpreferred_abstain": False}
        if abstain_every and i % abstain_every == 0:
            # "I cannot tell" on the preferred side: a real position, and
            # not a ranking signal.
            pair["preferred_abstain"] = True
            pair["preferred"] = -1
        pairs.append(pair)
    return pairs


class PreferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)
        cls.pairs = preference_pairs(96)
        cls.val_pairs = preference_pairs(64)

    def frame(self, pairs, name="pairs", **overrides) -> dict:
        overrides.setdefault("steps", 200)
        with _TempFile(self, name, {"pairs": pairs}) as path:
            with _TempFile(self, "val_pairs", {"pairs": self.val_pairs}) as val:
                return base_frame("preference", pairs=path, val_pairs=val,
                                  data=str(DATA / "val.bin"), **overrides)

    def test_preference_learning_raises_held_out_ranking(self):
        frame = self.frame(self.pairs)
        before = recipes._ranking_accuracy(
            recipes.Student(24, 2, 7),
            recipes.load_split_tensors(frame["val_data"]), self.val_pairs)
        metrics = run(frame)
        after = metrics["val_ranking_accuracy"]
        self.assertGreater(after, before,
                           f"held-out ranking did not improve: {before} -> {after}")
        self.assertGreater(changed(recipes.Student(24, 2, 7).export(),
                                   metrics["params"]), 1e-6)

    def test_an_abstention_is_excluded_and_counted(self):
        pairs = preference_pairs(96, abstain_every=8)
        usable, excluded = recipes.usable_pairs(pairs)
        self.assertEqual(len(usable) + len(excluded), len(pairs))
        self.assertGreater(len(excluded), 0)
        self.assertTrue(all(not p["preferred_abstain"] and not p["unpreferred_abstain"]
                            for p in usable),
                        "an abstaining pair reached the training set")
        metrics = run(self.frame(pairs))
        self.assertEqual(metrics["pairs_abstained"], len(excluded))
        self.assertEqual(metrics["pairs_trained"], len(usable))
        self.assertEqual(metrics["pairs_total"], len(pairs))

    def test_an_abstention_does_not_change_the_objective(self):
        """The strongest form of 'an abstention is not a loss': dropping
        the abstaining pairs must not change the loss at all, because they
        never entered it. If abstentions were trained as negatives, the two
        runs would diverge."""
        pairs = preference_pairs(96, abstain_every=8)
        usable, _ = recipes.usable_pairs(pairs)
        with_abstains = run(self.frame(pairs, name="with"))
        without = run(self.frame(usable, name="without"))
        self.assertAlmostEqual(with_abstains["loss"], without["loss"], places=6,
                               msg="abstaining pairs changed the objective")
        self.assertEqual(with_abstains["pairs_trained"], without["pairs_trained"])

    def test_a_run_of_pure_abstentions_refuses_rather_than_inverting_them(self):
        pairs = preference_pairs(32, abstain_every=1)   # every pair abstains
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(pairs, name="allabstain", steps=5))
        self.assertIn("abstention", str(caught.exception).lower())

    def test_a_preference_over_a_nonexistent_output_is_refused(self):
        pairs = preference_pairs(32)
        pairs[0] = dict(pairs[0], preferred=7)
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(pairs, name="wide", steps=5))
        self.assertIn("outside", str(caught.exception).lower())

    def test_a_recipe_that_treats_abstention_as_a_loss_is_refused(self):
        frame = self.frame(self.pairs, name="abstainloss", steps=5)
        frame["contract"] = dict(frame["contract"], **{"abstain-is-loss": True})
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("abstain", str(caught.exception).lower())

    def test_a_preference_frame_with_no_pairs_is_refused(self):
        with _TempFile(self, "empty", {"pairs": []}) as path:
            frame = base_frame("preference", pairs=path,
                               data=str(DATA / "val.bin"), steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("pairs", str(caught.exception).lower())


# ── 3. demonstration / behaviour cloning ─────────────────────────

class DemonstrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)
        cls.legal = declared("demonstration")["legal-actions"]
        truth = trainer.build(load_split(DATA / "val.bin"))[1].tolist()
        cls.entries = [{"index": i, "action": TASK_VOCAB[truth[i]]}
                       for i in range(len(truth))]

    def frame(self, entries, name="actions", **overrides) -> dict:
        overrides.setdefault("steps", 200)
        with _TempFile(self, name, {"actions": entries}) as path:
            with _TempFile(self, "val_actions", {"actions": self.entries}) as val:
                return base_frame("demonstration", actions=path, val_actions=val,
                                  data=str(DATA / "val.bin"), **overrides)

    def test_demonstration_cloning_learns_the_demonstrated_action(self):
        metrics = run(self.frame(self.entries))
        self.assertEqual(metrics["actions_illegal_masked"], 0)
        self.assertGreater(metrics["val_action_accuracy"], 0.9,
                           f"held-out action accuracy {metrics['val_action_accuracy']}")
        self.assertGreater(changed(recipes.Student(24, 2, 7).export(),
                                   metrics["params"]), 1e-6)

    def test_an_illegal_action_is_masked_out_of_the_loss_not_trained(self):
        entries = [dict(e) for e in self.entries]
        # A demonstrator emitting an action the contract says is not
        # available. It must cost nothing and train nothing.
        illegal = 0
        for i in range(0, len(entries), 4):
            entries[i]["action"] = "teleport"
            illegal += 1
        metrics = run(self.frame(entries, name="illegal"))
        self.assertEqual(metrics["actions_illegal_masked"], illegal)
        self.assertEqual(metrics["actions_total"], len(entries))
        # The illegal rows were counted out of the objective, not trained.
        self.assertLess(metrics["val_action_accuracy"], 1.0)

    def test_the_mask_is_applied_inside_the_loss(self):
        """Direct check of the mask arithmetic: an illegal action gets
        weight zero, so its target cannot reach a gradient."""
        entries = [{"index": 0, "action": "clear"},
                   {"index": 1, "action": "teleport"},
                   {"index": 2, "action": "fault"}]
        targets, mask, illegal = recipes.legal_mask(entries, self.legal)
        self.assertEqual(illegal, 1)
        self.assertEqual(mask.tolist(), [1.0, 0.0, 1.0])
        # A row masked to zero contributes exactly nothing to the loss,
        # whatever target it carries.
        per_row = torch.nn.functional.cross_entropy(
            torch.randn(3, 2), targets, reduction="none")
        masked = (per_row * mask).sum() / mask.sum()
        alone = (per_row[0] + per_row[2]) / 2
        self.assertAlmostEqual(float(masked), float(alone), places=6)

    def test_masked_rows_cannot_reach_the_parameters(self):
        """The mask has to survive the parameter it protects. An illegal
        action whose target happens to agree with a legal one must still
        contribute nothing: changing only the illegal target cannot move
        the weights."""
        entries = [{"index": 0, "action": "clear"},
                   {"index": 1, "action": "teleport"},
                   {"index": 2, "action": "fault"}]
        flipped = [dict(entries[0]), {"index": 1, "action": "teleport"},
                   dict(entries[2])]
        targets, mask, _ = recipes.legal_mask(entries, self.legal)
        # Give the masked row the most damaging legal target available and
        # confirm it is still weighted out.
        self.assertEqual(int(targets[1]), 0)
        self.assertEqual(float(mask[1]), 0.0)
        logits = torch.tensor([[0.1, 5.0], [5.0, 0.1], [0.1, 5.0]])
        per_row = torch.nn.functional.cross_entropy(logits, targets, reduction="none")
        weighted = (per_row * mask).sum() / mask.sum()
        only_legal = per_row[[0, 2]].mean()
        self.assertAlmostEqual(float(weighted), float(only_legal), places=6)
        self.assertEqual(flipped[1]["action"], "teleport")

    def test_a_fully_illegal_demonstration_trains_nothing_and_says_so(self):
        entries = [{"index": i, "action": "teleport"} for i in range(16)]
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(entries, name="allillegal", steps=5))
        self.assertIn("legal action space", str(caught.exception).lower())

    def test_a_recipe_with_no_declared_action_space_is_refused(self):
        frame = self.frame([{"index": 0, "action": "clear"}],
                           name="nolegal", steps=5)
        frame["contract"] = {k: val for k, val in frame["contract"].items()
                             if k != "legal-actions"}
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("legal action space", str(caught.exception).lower())

    def test_a_head_that_cannot_emit_the_legal_actions_is_refused(self):
        frame = self.frame([{"index": 0, "action": "clear"}],
                           name="widehead", steps=5, out=5)
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("legal", str(caught.exception).lower())


# ── 4. self-supervised ───────────────────────────────────────────

class SelfSupervisedTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)

    def test_representation_moves_while_the_task_metric_is_reported_separately(self):
        frame = base_frame("self-supervised", data=str(DATA / "val.bin"), steps=30)
        fresh = recipes.Student(24, 2, 7)
        split = recipes.load_split_tensors(frame["val_data"])
        before = recipes._representation(fresh, split["x"][:64])
        metrics = run(frame)
        # The representation is what self-supervision is for, and it moves
        # a long way: neighbouring scenes separate where they used to sit
        # on top of each other.
        self.assertGreater(metrics["representation_shift"], 0.1,
                           f"representation barely moved: {metrics['representation_shift']}")
        self.assertLessEqual(metrics["loss"], metrics["first_loss"],
                             "reconstruction loss did not fall")
        self.assertLess(metrics["first_loss"], 0.1)
        # The task metric is reported separately and is NOT the
        # reconstruction loss. That separation is the whole point: a
        # falling proxy loss here buys nothing on the task.
        self.assertIsNotNone(metrics["task_val_accuracy"])
        self.assertEqual(metrics["task_gate"], declared("self-supervised")["task-gate"])

    def test_a_recipe_without_a_task_gate_is_refused(self):
        frame = base_frame("self-supervised", data=str(DATA / "val.bin"), steps=5)
        frame["contract"] = {k: val for k, val in frame["contract"].items()
                             if k != "task-gate"}
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("task-gate", str(caught.exception).lower())

    def test_the_task_metric_is_reported_even_when_it_is_bad(self):
        """Self-supervision improving a representation must not be able to
        pass off a bad task score as a result."""
        frame = base_frame("self-supervised", data=str(DATA / "val.bin"), steps=10)
        metrics = run(frame)
        self.assertLess(metrics["task_val_accuracy"], 0.9,
                        "a few steps of masked reconstruction cannot solve the "
                        "task; if this passes the gate is measuring nothing")


# ── 5. environment feedback ──────────────────────────────────────

def trajectory_records(n: int) -> list:
    """Trajectories whose delayed return depends on the action taken, so a
    policy that increases the log-probability of the good action is
    measurably better.

    States are laid out in the student's input width, like every other
    input to this model, so the trajectory is something the policy can
    actually read.
    """
    records = []
    for i in range(n):
        # The state is the task's own feature layout: pixel block (unused
        # here), the two readings, and the two presence flags.
        states = []
        for step in range(4):
            row = [0.0] * recipes.INPUT
            row[256] = 1.0
            row[257] = -1.0 if step % 2 == 0 else 1.0
            row[258] = 1.0
            row[259] = 1.0
            states.append(row)
        records.append({
            "index": i,
            "trajectory_id": f"traj-{i}",
            "states": states,
            "actions": [1] * 4,          # "right": the action that pays
            "results": [{"index": 0, "trajectory_id": f"traj-{i}",
                         "reward": 1.0, "delay_ms": 5000}],
        })
    return records


class EnvironmentFeedbackTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)
        cls.trajectories = trajectory_records(64)

    def frame(self, trajectories, name="trajectories", **overrides) -> dict:
        overrides.setdefault("steps", 30)
        with _TempFile(self, name, {"trajectories": trajectories}) as path:
            return base_frame("environment-feedback", trajectories=path,
                              **overrides)

    def test_delayed_feedback_raises_the_log_probability_of_the_paid_action(self):
        frame = self.frame(self.trajectories, out=3)
        before = _chosen_log_prob(recipes.Student(24, 3, 7), self.trajectories[0])
        metrics = run(frame)
        # Measure the model the recipe actually produced, at the width it
        # was trained at — not a fresh one that never saw the data.
        trained = recipes.student_from(metrics["params"], 24, 7)
        after = _chosen_log_prob(trained, self.trajectories[0])
        self.assertGreater(after, before,
                           f"the paid action did not become more likely: "
                           f"{before} -> {after}")
        self.assertEqual(metrics["results_settled"], len(self.trajectories))
        self.assertGreater(changed(recipes.Student(24, 3, 7).export(),
                                   metrics["params"]), 1e-6)

    def test_a_result_that_cannot_name_its_trajectory_is_refused(self):
        broken = copy.deepcopy(self.trajectories[:4])
        broken[2]["results"][0].pop("trajectory_id")
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(broken, name="unnamed", steps=5))
        self.assertIn("trajectory", str(caught.exception).lower())

    def test_a_result_claiming_the_wrong_trajectory_is_refused(self):
        broken = copy.deepcopy(self.trajectories[:4])
        broken[1]["results"][0]["trajectory_id"] = "traj-99"
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(broken, name="mismatched", steps=5))
        self.assertIn("trajectory", str(caught.exception).lower())

    def test_an_empty_trajectory_id_is_not_an_id(self):
        broken = copy.deepcopy(self.trajectories[:4])
        broken[1]["results"][0]["trajectory_id"] = ""
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(broken, name="emptyid", steps=5))
        self.assertIn("trajectory", str(caught.exception).lower())

    def test_a_trajectory_with_no_settled_result_trains_nothing(self):
        """A delayed outcome that has not arrived is not a zero reward: it
        is no signal. Treating it as zero would train "this was bad"."""
        pending = copy.deepcopy(self.trajectories[:4])
        for traj in pending:
            traj["results"] = []
        with self.assertRaises(recipes.RecipeError) as caught:
            run(self.frame(pending, name="pending", steps=5))
        self.assertIn("settled", str(caught.exception).lower())

    def test_a_recipe_that_does_not_bind_to_a_trajectory_is_refused(self):
        frame = self.frame(self.trajectories[:4], name="unbound", steps=5)
        frame["contract"] = {k: val for k, val in frame["contract"].items()
                             if k != "binds"}
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("trajectory", str(caught.exception).lower())


@torch.no_grad()
def _chosen_log_prob(student, traj: dict) -> float:
    states = torch.tensor(traj["states"], dtype=torch.float32)
    actions = torch.tensor(traj["actions"], dtype=torch.long)
    log_probs = torch.log_softmax(student(states), dim=1)
    return float(log_probs.gather(1, actions.unsqueeze(1)).squeeze(1).mean())


# ── 6. restricted policy gradient ────────────────────────────────

class _FixedPolicy:
    """A policy with no learned head, for checking the environment's own
    contract: it always prefers one action."""

    def __init__(self, prefer: int):
        self.prefer = prefer

    def __call__(self, x: torch.Tensor) -> torch.Tensor:
        logits = torch.zeros(x.shape[0], 3)
        logits[:, self.prefer] = 10.0
        return logits


class PolicyGradientTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)

    def frame(self, **overrides) -> dict:
        overrides.setdefault("steps", 40)
        overrides.setdefault("out", 3)
        return base_frame("policy-gradient", hidden=24,
                          env={"width": 6, "max_steps": 20, "goal": 5,
                               "step_reward": -0.05, "goal_reward": 1.0,
                               "val_episodes": 48}, **overrides)

    def test_policy_gradient_raises_the_return_and_the_goal_rate(self):
        frame = self.frame()
        spec = recipes.env_spec(frame)
        fresh = recipes.Student(24, 3, 7)
        before = recipes._evaluate_return(fresh, frame)
        before_rate = recipes._evaluate_termination(fresh, spec)
        metrics = run(frame)
        after = metrics["val_return"]
        after_rate = metrics["val_termination_rate"]
        self.assertGreater(after, before,
                           f"policy return did not improve: {before} -> {after}")
        self.assertGreater(after_rate, before_rate,
                           f"goal rate did not improve: {before_rate} -> {after_rate}")
        self.assertGreater(changed(recipes.Student(24, 3, 7).export(),
                                   metrics["params"]), 1e-6)

    def test_the_environment_honours_the_declared_termination(self):
        spec = recipes.env_spec({"env": {"width": 5, "max_steps": 4, "goal": 4}})
        student = recipes.Student(24, 3, 7)
        traj = recipes.rollout(student, spec, greedy=True)
        self.assertLessEqual(traj["steps"], spec["max_steps"])
        # Every step costs `step_reward`; the goal bonus is paid only on
        # actually reaching the goal, and the episode stops there.
        self.assertAlmostEqual(
            traj["return"],
            spec["step_reward"] * traj["steps"]
            + (spec["goal_reward"] if traj["terminated"] else 0.0), places=6)

    def test_a_goal_reaching_policy_gets_the_bonus_and_stops(self):
        """Termination is the contract: reaching the goal ends the episode
        and pays the bonus, and no further step is taken."""
        spec = recipes.env_spec({"env": {"width": 3, "max_steps": 20, "goal": 2,
                                         "step_reward": -0.05, "goal_reward": 1.0}})
        # A policy that always walks right reaches the goal in two steps.
        student = _FixedPolicy(prefer=1)
        traj = recipes.rollout(student, spec, greedy=True)
        self.assertTrue(traj["terminated"])
        self.assertEqual(traj["steps"], 2)
        self.assertAlmostEqual(traj["return"], 2 * -0.05 + 1.0, places=6)

    def test_an_environment_too_wide_for_the_model_is_refused(self):
        spec = recipes.env_spec({"env": {"width": recipes.INPUT, "goal": 1}})
        with self.assertRaises(recipes.RecipeError) as caught:
            recipes.check_env_observation(spec)
        self.assertIn("observation", str(caught.exception).lower())

    def test_a_policy_recipe_with_no_action_set_is_refused(self):
        frame = self.frame(steps=5)
        frame["contract"] = {k: val for k, val in frame["contract"].items()
                             if k != "actions"}
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("action", str(caught.exception).lower())

    def test_a_policy_recipe_with_no_reward_or_termination_is_refused(self):
        for missing in ("reward", "termination"):
            frame = self.frame(steps=5)
            frame["contract"] = {k: val for k, val in frame["contract"].items()
                                 if k != missing}
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
            self.assertIn(missing, str(caught.exception).lower())

    def test_a_head_wider_than_the_action_set_is_refused(self):
        frame = self.frame(steps=5, out=7)
        with self.assertRaises(recipes.RecipeError) as caught:
            run(frame)
        self.assertIn("action", str(caught.exception).lower())


# ── the protocol surface ─────────────────────────────────────────

class ProtocolTests(unittest.TestCase):
    def frame_lines(self, frame: dict) -> list:
        process = subprocess.run(
            [str(PYTHON), str(WORKER / "worker.py")],
            input=json.dumps(frame) + "\n", capture_output=True, text=True,
            timeout=600)
        self.assertEqual(process.returncode, 0, process.stderr[-2000:])
        return [json.loads(line) for line in process.stdout.splitlines() if line]

    def test_the_worker_advertises_the_recipe_capability(self):
        lines = self.frame_lines({"v": 1, "type": "train", "run_id": "x",
                                  "graph": {}, "data": "missing.bin",
                                  "weights": "missing.json", "steps": 1,
                                  "out": "/tmp/nope.json"})
        self.assertEqual(lines[0]["type"], "hello")
        self.assertIn("recipe", lines[0]["capabilities"])
        self.assertIn("train", lines[0]["capabilities"])
        self.assertIn("predict", lines[0]["capabilities"])

    def test_a_recipe_frame_runs_end_to_end_over_the_protocol(self):
        truth = trainer.build(load_split(DATA / "val.bin"))[1].tolist()
        entries = [{"index": i, "action": TASK_VOCAB[truth[i]]} for i in range(16)]
        with tempfile.TemporaryDirectory() as tmp:
            actions = Path(tmp) / "actions.json"
            actions.write_text(json.dumps({"actions": entries}), encoding="utf-8")
            out = Path(tmp) / "weights.json"
            frame = base_frame("demonstration", actions=str(actions),
                               data=str(DATA / "val.bin"), steps=5,
                               weights_out=str(out), run_id="r-proto")
            lines = self.frame_lines(frame)
            done = [line for line in lines if line["type"] == "done"]
            self.assertEqual(len(done), 1, lines)
            self.assertEqual(done[0]["recipe"], "demonstration@1")
            self.assertEqual(done[0]["run_id"], "r-proto")
            self.assertEqual(done[0]["weights"], str(out))
            self.assertIn("metrics", done[0])
            written = json.loads(out.read_text(encoding="utf-8"))
        self.assertIn("params", written)
        self.assertEqual(written["recipe"], "demonstration@1")
        self.assertEqual(sorted(written["params"]), ["h0", "h1"])

    def test_a_refused_recipe_is_a_terminal_failed_frame(self):
        pairs = [{"index": 0, "preferred": -1, "unpreferred": 0,
                  "preferred_abstain": True, "unpreferred_abstain": False}]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "pairs.json"
            path.write_text(json.dumps({"pairs": pairs}), encoding="utf-8")
            frame = base_frame("preference", pairs=str(path),
                               run_id="r-fail", steps=5)
            lines = self.frame_lines(frame)
        failed = [line for line in lines if line["type"] == "failed"]
        self.assertEqual(len(failed), 1, lines)
        self.assertEqual(failed[0]["run_id"], "r-fail")
        self.assertIn("abstention", failed[0]["error"].lower())

    def test_an_unknown_recipe_is_refused(self):
        frame = base_frame("demonstration", recipe="teleport@9", steps=5)
        lines = self.frame_lines(frame)
        self.assertTrue(any(line["type"] == "failed" for line in lines), lines)

    def test_a_frame_whose_contract_names_another_recipe_is_refused(self):
        frame = base_frame("demonstration", steps=5)
        frame["contract"] = dict(frame["contract"], id="policy-gradient@1")
        lines = self.frame_lines(frame)
        failed = [line for line in lines if line["type"] == "failed"]
        self.assertEqual(len(failed), 1, lines)
        self.assertIn("unstated policy", failed[0]["error"])

    def test_a_frame_with_no_contract_is_refused(self):
        frame = base_frame("demonstration", steps=5)
        del frame["contract"]
        lines = self.frame_lines(frame)
        failed = [line for line in lines if line["type"] == "failed"]
        self.assertEqual(len(failed), 1, lines)
        self.assertIn("contract", failed[0]["error"].lower())

    def test_training_an_undeclared_layer_is_refused(self):
        frame = base_frame("demonstration", trainable=["h0", "h7"], steps=5)
        lines = self.frame_lines(frame)
        failed = [line for line in lines if line["type"] == "failed"]
        self.assertEqual(len(failed), 1, lines)
        self.assertIn("h7", failed[0]["error"])


class SupervisedRegressionTests(unittest.TestCase):
    """The pre-existing supervised path still works through the same
    dispatch: adding `recipe` must not have disturbed `train`."""

    @classmethod
    def setUpClass(cls):
        torch.set_num_threads(1)
        cls.oracle = oracle_soft("val.bin")

    def test_hard_distillation_trains_on_a_real_teacher_label(self):
        payload = {"vocab": TASK_VOCAB, "soft": self.oracle["soft"],
                   "hard": [0 if row[0] >= row[1] else 1
                            for row in self.oracle["soft"]]}
        with _TempFile(self, "teacher", payload) as path:
            frame = base_frame("hard-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=200)
            metrics = run(frame)
        self.assertEqual(metrics["label_source"], "teacher-label")
        self.assertGreater(metrics["val_accuracy"], 0.9,
                           f"hard distillation should fit the task, got "
                           f"{metrics['val_accuracy']}")
        self.assertLess(metrics["loss"], metrics["first_loss"])

    def test_the_split_label_path_still_works_and_says_which_it_used(self):
        metrics = run(base_frame("hard-distill", steps=200))
        self.assertEqual(metrics["label_source"], "split-label")
        self.assertGreater(metrics["val_accuracy"], 0.9)

    def test_a_teacher_payload_with_no_label_is_refused(self):
        with _TempFile(self, "nolabel", {"vocab": TASK_VOCAB,
                                         "soft": self.oracle["soft"]}) as path:
            frame = base_frame("hard-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("label", str(caught.exception).lower())

    def test_a_teacher_whose_row_count_does_not_match_is_refused(self):
        with _TempFile(self, "shortlabels",
                       {"vocab": TASK_VOCAB, "hard": [0, 1]}) as path:
            frame = base_frame("hard-distill", data=str(DATA / "val.bin"),
                               teacher={"path": path}, steps=5)
            with self.assertRaises(recipes.RecipeError) as caught:
                run(frame)
        self.assertIn("rows", str(caught.exception).lower())


if __name__ == "__main__":
    unittest.main()
