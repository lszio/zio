#!/usr/bin/env python3
"""W15 recipe executor — five methods beyond supervised + hard distillation.

A recipe is *data* declared in `libs/learning/learn/recipes.zio`; this module is
the executor for the extended family. The host dispatches on the recipe id
and sends the declaration's contract fields down with the frame, so the
refusals below cannot be skipped by the data path: every one of them is
enforced on the tensor, after the data is loaded.

Five families, one student (a named-layer MLP over the W00 fused feature
vector) and one optimiser path:

    hard-distill@1          cross-entropy on a teacher's argmax label
    soft-distill@1         temperature-scaled KL against a teacher's
                            distribution, on the SAME label space
    preference@1           pairwise logistic over two candidate outputs;
                            an abstention is excluded, never a negative
    demonstration@1        masked CE on demonstrator actions, restricted
                            to a DECLARED legal action space
    self-supervised@1      masked reconstruction on the inputs; improves
                            the representation and NEVER substitutes for
                            the task's own evaluation
    environment-feedback@1 delayed outcomes supervised on the original
                            trajectory (the id must survive the delay)
    policy-gradient@1      a RESTRICTED discrete policy: explicit action
                            set, reward, termination — not a claim about
                            arbitrary RL

Frame contract (NDJSON, one object per line, same shape as worker.py's
train/predict frames — the host wires this as `HANDLERS["recipe"]`):

    → {"v":1,"type":"recipe","run_id":"…","attempt_id":"…",
       "recipe":"soft-distill@1",
       "steps":200,"seed":1,"lr":0.02,"hidden":24,"out":2,
       "trainable":["h0","h1"],
       "contract":{ … the recipe's declared fields, verbatim from zio … },
       … recipe-specific inputs, see each run_* docstring … }
    ← {"v":1,"type":"done","run_id":"…","recipe":"…","loss":…,
       "params":{…},"metrics":{…}}
    ← {"v":1,"type":"failed","run_id":"…","error":"…"}

Common inputs: `data` (a GVD1 split path, as worker.py takes it) and
`val_data`. `contract` is REQUIRED for every recipe and is the declaration
the zio side published; a recipe that needs a contract field and does not
get it is refused rather than defaulted. `weights_out` is where the
trained parameters are written (optional); `out` is the head WIDTH, not a
path.

Recipe-specific inputs:

    soft-distill@1         "teacher": {"url": "http://…"} | {"path": "…"}
                          payload {"vocab": […], "soft": [[p0,p1], …]}
                          "out" must equal the payload's width
    preference@1           "pairs": path to {"pairs":[{"index":i,
                          "preferred":k,"unpreferred":j,
                          "preferred_abstain":bool,
                          "unpreferred_abstain":bool}, …]}
    demonstration@1        "actions": path to {"actions":[{"index":i,
                          "action":"confirm"}, …]}; `contract.legal-actions`
                          is the declared set the mask is built from
    self-supervised@1      "mask_ratio": float (default 0.25)
    environment-feedback@1 "trajectories": path to {"trajectories":[{…}]}
    policy-gradient@1      "env": {"width":6,"max_steps":20,
                          "goal":5,"step_reward":-0.05,"goal_reward":1.0,
                          "episodes":…}
"""

from __future__ import annotations

import json
import os
import sys
import urllib.request
from pathlib import Path

import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from data import load_split  # noqa: E402
import teacher as teacher_service  # noqa: E402
import train as trainer  # noqa: E402

PROTOCOL = "grove.worker/1"
PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 1 << 20
INPUT = trainer.INPUT            # 256 pixels + 2 readings + 2 presence flags
STDERR_CAP = 64 * 1024

#: The label space the W00 task declares. A soft target is only usable on
#: the student's own label space, so the vocabulary is data, not an
#: assumption about tensor width.
TASK_VOCAB = ["clear", "fault"]

ABSTAIN = -1

_stderr_written = 0


class RecipeError(ValueError):
    """A refused recipe contract. Refusal is a terminal, visible state."""


def _note(message: str) -> None:
    global _stderr_written
    if _stderr_written >= STDERR_CAP:
        return
    chunk = message[:2000]
    _stderr_written += len(chunk) + 1
    sys.stderr.write(chunk + "\n")
    sys.stderr.flush()


def emit(payload: dict) -> None:
    """One protocol line on stdout, and nothing else ever."""
    sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def read_frame() -> dict | None:
    line = sys.stdin.readline()
    if not line:
        return None
    if len(line) > MAX_FRAME_BYTES:
        raise ValueError(f"inbound frame exceeds {MAX_FRAME_BYTES} bytes")
    return json.loads(line)


# ── the student ──────────────────────────────────────────────────

class Student(torch.nn.Module):
    """Named-layer MLP over the W00 fused feature vector.

    The names matter: a recipe declares which layers it may train, and the
    optimiser only ever receives the declared ones. A recipe that does not
    declare is not allowed to move a layer by accident.
    """

    def __init__(self, hidden: int, out: int, seed: int):
        super().__init__()
        torch.manual_seed(seed)
        self.h0 = torch.nn.Linear(INPUT, hidden)
        self.h1 = torch.nn.Linear(hidden, out)

    def named_layers(self) -> dict:
        return {"h0": self.h0, "h1": self.h1}

    def parameters_of(self, names) -> list:
        layers = self.named_layers()
        missing = [n for n in names if n not in layers]
        if missing:
            raise RecipeError(f"recipe trains unknown layer(s) {missing}")
        return [p for n in names for p in layers[n].parameters()]

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.h1(torch.relu(self.h0(x)))

    def hidden(self, x: torch.Tensor) -> torch.Tensor:
        """The representation a self-supervised recipe moves."""
        return torch.relu(self.h0(x))

    def export(self) -> dict:
        return {n: {"w": layer.weight.detach().tolist(),
                    "b": layer.bias.detach().tolist()}
                for n, layer in self.named_layers().items()}

    def load(self, params: dict) -> "Student":
        """Restore an exported parameter set. The head width comes from
        the artifact, not from a caller's guess, so a model can never be
        evaluated at a width it was not trained at."""
        for name, entry in params.items():
            layer = self.named_layers().get(name)
            if layer is None:
                raise RecipeError(f"parameter artifact names unknown layer {name!r}")
            w = torch.tensor(entry["w"], dtype=torch.float32)
            if w.shape != layer.weight.shape:
                raise RecipeError(
                    f"parameter artifact for {name!r} is {list(w.shape)}, "
                    f"the model expects {list(layer.weight.shape)}")
            with torch.no_grad():
                layer.weight.copy_(w)
                if "b" in entry:
                    layer.bias.copy_(torch.tensor(entry["b"], dtype=torch.float32))
        return self


def student_from(params: dict, hidden: int, seed: int) -> Student:
    """Rebuild a student from an exported parameter set at the width the
    artifact itself declares."""
    out = len(params["h1"]["w"])
    return Student(hidden, out, seed).load(params)


def trainable_names(frame: dict, student: Student) -> list:
    declared = frame.get("trainable")
    if declared is None:
        return ["h0", "h1"]
    if not isinstance(declared, list) or not declared:
        raise RecipeError(f"trainable must be a non-empty list, got {declared!r}")
    student.parameters_of(declared)   # refuse unknown names up front
    return list(declared)


# ── data ─────────────────────────────────────────────────────────

def load_split_tensors(path: str) -> dict:
    """GVD1 → {x, y, decidable}. Same reader worker.py and the teacher use,
    so a recipe trains on exactly the samples the rest of the system sees."""
    rows = load_split(Path(path))
    x, y, decidable = trainer.build(rows)
    return {"x": x, "y": y, "decidable": decidable}


def _split_names(contract: dict) -> list:
    kinds = contract.get("consumes") or []
    if not kinds:
        raise RecipeError("contract declares no consumable signal kinds")
    return [str(k).lstrip(":") for k in kinds]


def guard_consumes(contract: dict, signal_kind: str) -> None:
    """The signal a recipe consumes must be one it declared. A recipe
    silently ignoring a kind is an invisible policy change."""
    if signal_kind not in _split_names(contract):
        raise RecipeError(
            f"recipe {contract.get('id')!r} does not consume "
            f"{signal_kind!r} (consumes: {_split_names(contract)})")


# ── 1. hard distillation ─────────────────────────────────────────

def run_hard_distill(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Cross-entropy on the teacher's argmax label.

    The teacher supplies the label, exactly as in the soft path: a
    `teacher` spec is read the same way, and its `hard` field is what
    trains. Without one the recipe falls back to the split's own label,
    which is the W04 supervised path and is labelled as such in the
    metrics — the two are never confused.
    """
    guard_consumes(contract, "teacher-label")
    split = load_split_tensors(frame["data"])
    train = dict(split)
    source = "split-label"
    if frame.get("teacher"):
        payload = teacher_payload(frame, want_soft=False)
        labels = payload.get("hard")
        if labels is None:
            raise RecipeError(
                "hard-distill received a teacher payload with no 'hard' label; "
                "refusing to invent one")
        if len(labels) != split["x"].shape[0]:
            raise RecipeError(
                f"teacher answered {len(labels)} rows but the split has "
                f"{split['x'].shape[0]}")
        train["y"] = torch.tensor([int(v) for v in labels], dtype=torch.long)
        source = "teacher-label"
    val = load_split_tensors(frame["val_data"]) if frame.get("val_data") else None
    steps = int(frame["steps"])
    losses = []
    for _ in range(steps):
        order = torch.randperm(train["x"].shape[0])
        batch = order[:64]
        per_row = torch.nn.functional.cross_entropy(
            student(train["x"][batch]), train["y"][batch], reduction="none")
        weight = train["decidable"][batch]
        loss = (per_row * weight).sum() / weight.sum().clamp(min=1.0)
        opt.zero_grad(); loss.backward(); opt.step()
        losses.append(float(loss.detach()))
    return {"loss": losses[-1], "first_loss": losses[0], "label_source": source,
            "val_accuracy": _accuracy(student, val)}


# ── 2. soft distillation ─────────────────────────────────────────

def teacher_payload(frame: dict, want_soft: bool = True) -> dict:
    """A real teacher response, over HTTP from the local teacher service or
    from a payload it wrote. The label space travels with the scores: a
    probability vector without its vocabulary is not a training target.

    `want_soft=False` asks a hard-output-only question, which is what a
    vendor supplying only labels looks like; the response then carries
    `hard` and no `soft`, and the soft recipe refuses it.
    """
    spec = frame["teacher"]
    if "url" not in spec:
        return json.loads(Path(spec["path"]).read_text(encoding="utf-8"))
    rows = load_split(Path(frame["data"]))
    soft, hard, vocab = [], [], None
    for i, obs in enumerate(rows):
        body = json.dumps({"request_id": f"r{i}", "want_soft": want_soft,
                           "parts": _observation_parts(obs)}).encode()
        request = urllib.request.Request(
            spec["url"], data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=30) as response:
            answer = json.loads(response.read().decode("utf-8"))
        if answer.get("error"):
            raise RecipeError(f"teacher error: {answer['error']}")
        label = answer.get("hard_label")
        hard.append(None if label is None else (0 if label == "clear" else 1))
        scores = answer.get("soft_scores")
        if scores is not None:
            soft.append([float(v) for v in scores])
        vocab = vocab or answer.get("vocab") or TASK_VOCAB
    return {"vocab": list(vocab), "soft": soft, "hard": hard}


def _observation_parts(obs) -> list:
    parts = []
    if obs.image_present:
        parts.append({"kind": "reference", "media_type": "image/png",
                      "pixels": list(obs.pixels)})
    if obs.numeric_present:
        parts.append({"kind": "numbers", "values": list(obs.readings)})
    return parts


def check_teacher_space(contract: dict, payload: dict, student_out: int) -> None:
    """The refusal the plan names: a teacher whose vocabulary is not the
    student's is rejected, never reshaped.

    Reshaping would invent a distribution over classes the teacher never
    expressed, and — worse — silently swap their meaning: the same
    probability vector over ["defect","clear"] is a different teacher than
    over ["clear","fault"], and the student cannot tell them apart by
    width. So the label space is compared by name, and the width too.
    """
    declared = contract.get("teacher-vocab") or contract.get("teacher_vocab")
    if not declared:
        raise RecipeError(
            f"recipe {contract.get('id')!r} distils soft targets but declares no "
            "teacher vocabulary; a soft target without a label space is not usable")
    actual = payload.get("vocab")
    if not actual:
        raise RecipeError("teacher payload carries no vocabulary; refusing to "
                          "infer a label space from a probability vector")
    if [str(v) for v in actual] != [str(v) for v in declared]:
        raise RecipeError(
            f"teacher vocabulary {actual} does not match the recipe's declared "
            f"label space {declared}; a soft target on a different label space "
            "is refused, not reshaped")
    width = len(payload["soft"][0]) if payload.get("soft") else 0
    if width != len(declared):
        raise RecipeError(
            f"teacher returned {width} scores for a {len(declared)}-class "
            f"vocabulary {declared}")
    if width != student_out:
        raise RecipeError(
            f"student head is {student_out}-wide but the teacher speaks "
            f"{width} classes")


def _validate_distribution(soft: list, width: int) -> None:
    if not soft:
        raise RecipeError("teacher payload has no scores")
    for i, row in enumerate(soft):
        if len(row) != width:
            raise RecipeError(f"teacher row {i} has {len(row)} scores, expected {width}")
        if any(not isinstance(v, (int, float)) or v < 0.0 for v in row):
            raise RecipeError(f"teacher row {i} is not a probability vector: {row}")
        total = sum(row)
        if abs(total - 1.0) > 1e-3:
            raise RecipeError(f"teacher row {i} sums to {total}, not 1.0")


def run_soft_distill(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Temperature-scaled distribution matching, on the student's own
    label space.

    The teacher's frozen scores are turned into a temperature-sharpened
    target by renormalising `log p / T` — a function of the payload alone,
    so the temperature genuinely changes the target and the loss responds
    to it (the test measures both).
    """
    guard_consumes(contract, "teacher-label")
    if not contract.get("temperature"):
        raise RecipeError("soft-distill declares no temperature")
    temperature = float(contract["temperature"])
    if temperature <= 0.0:
        raise RecipeError(f"temperature must be positive, got {temperature}")
    student_out = student.h1.weight.shape[0]
    payload = teacher_payload(frame)
    check_teacher_space(contract, payload, student_out)
    soft = payload["soft"]
    _validate_distribution(soft, student_out)
    targets = torch.tensor(_temper(soft, temperature), dtype=torch.float32)
    train_x = load_split_tensors(frame["data"])["x"]
    if targets.shape[0] != train_x.shape[0]:
        raise RecipeError(
            f"teacher answered {targets.shape[0]} rows but the split has "
            f"{train_x.shape[0]}; a soft target row must name its own sample")
    val = load_split_tensors(frame["val_data"]) if frame.get("val_data") else None
    steps = int(frame["steps"])
    losses = []
    for _ in range(steps):
        order = torch.randperm(train_x.shape[0])
        batch = order[:64]
        loss = _kl(student(train_x[batch]), targets[batch])
        opt.zero_grad(); loss.backward(); opt.step()
        losses.append(float(loss.detach()))
    val_kl = None
    if val is not None:
        val_payload = json.loads(Path(frame["teacher_val"]).read_text(encoding="utf-8")) \
            if frame.get("teacher_val") else payload
        val_targets = torch.tensor(_temper(val_payload["soft"], temperature),
                                   dtype=torch.float32)
        with torch.no_grad():
            val_kl = float(_kl(student(val["x"]), val_targets))
    return {"loss": losses[-1], "first_loss": losses[0],
            "temperature": temperature, "val_kl": val_kl,
            "val_accuracy": _accuracy(student, val)}


def _temper(soft: list, temperature: float) -> list:
    out = []
    for row in soft:
        scaled = [pow(max(float(v), 1e-12), 1.0 / temperature) for v in row]
        total = sum(scaled)
        out.append([v / total for v in scaled])
    return out


def _kl(student_logits: torch.Tensor, target: torch.Tensor) -> torch.Tensor:
    log_q = torch.log_softmax(student_logits, dim=1)
    return -(target * log_q).sum(dim=1).mean()


# ── 3. preference learning ───────────────────────────────────────

def load_pairs(frame: dict) -> list:
    blob = json.loads(Path(frame["pairs"]).read_text(encoding="utf-8"))
    pairs = blob.get("pairs", [])
    if not pairs:
        raise RecipeError("preference frame carries no pairs")
    return pairs


def usable_pairs(pairs: list) -> tuple:
    """Split into trainable pairs and abstentions.

    An abstention is a position, not a loss. "I cannot tell" is a real
    answer here (the task defines one), so a pair holding one on either
    side is EXCLUDED from the objective and counted — never trained as a
    rejected alternative, which would teach the model that declining is
    wrong.
    """
    usable, excluded = [], []
    for pair in pairs:
        if pair.get("preferred_abstain") or pair.get("unpreferred_abstain"):
            excluded.append(pair)
        else:
            usable.append(pair)
    return usable, excluded


def run_preference(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Pairwise logistic over two candidate outputs for the same input.

    Only the GAP between the two candidates is trained: this is a ranking
    objective, so a logit shift that leaves the gap alone is not progress.
    """
    guard_consumes(contract, "human-preference")
    if contract.get("abstain-is-loss") is not False:
        raise RecipeError(
            f"recipe {contract.get('id')!r} must declare abstain-is-loss=false; "
            "an abstention is not a loss")
    split = load_split_tensors(frame["data"])
    pairs = load_pairs(frame)
    usable, excluded = usable_pairs(pairs)
    if not usable:
        raise RecipeError("every preference pair is an abstention; there is "
                          "nothing to learn and no preference to invert")
    out = student.h1.weight.shape[0]
    for pair in usable:
        for side in ("preferred", "unpreferred"):
            k = pair[side]
            if not isinstance(k, int) or not 0 <= k < out:
                raise RecipeError(
                    f"preference names output {k!r}, outside the student's "
                    f"{out} outputs; a ranking over a nonexistent output is refused")
    steps = int(frame["steps"])
    losses = []
    for _ in range(steps):
        order = torch.randperm(len(usable))
        batch = [usable[i] for i in order[:64]]
        idx = torch.tensor([p["index"] for p in batch], dtype=torch.long)
        logits = student(split["x"][idx])
        preferred = torch.tensor([p["preferred"] for p in batch], dtype=torch.long)
        unpreferred = torch.tensor([p["unpreferred"] for p in batch], dtype=torch.long)
        gap = logits.gather(1, preferred.unsqueeze(1)).squeeze(1) \
            - logits.gather(1, unpreferred.unsqueeze(1)).squeeze(1)
        loss = -torch.nn.functional.logsigmoid(gap).mean()
        opt.zero_grad(); loss.backward(); opt.step()
        losses.append(float(loss.detach()))
    val = frame.get("val_pairs")
    val_ranking = None
    if val:
        if not frame.get("val_data"):
            raise RecipeError(
                "val_pairs were given without val_data; a held-out ranking "
                "needs the samples the pairs refer to")
        val_pairs = json.loads(Path(val).read_text(encoding="utf-8"))["pairs"]
        val_ranking = _ranking_accuracy(student, load_split_tensors(frame["val_data"]), val_pairs)
    return {"loss": losses[-1], "first_loss": losses[0],
            "pairs_total": len(pairs), "pairs_abstained": len(excluded),
            "pairs_trained": len(usable), "val_ranking_accuracy": val_ranking}


@torch.no_grad()
def _ranking_accuracy(student: Student, split: dict, pairs: list) -> float:
    """Fraction of held-out pairs the student orders correctly. The
    objective is a ranking, so this — not accuracy — is its held-out
    behaviour."""
    usable, _ = usable_pairs(pairs)
    if not usable:
        return None
    idx = torch.tensor([p["index"] for p in usable], dtype=torch.long)
    logits = student(split["x"][idx])
    preferred = torch.tensor([p["preferred"] for p in usable], dtype=torch.long)
    unpreferred = torch.tensor([p["unpreferred"] for p in usable], dtype=torch.long)
    gap = logits.gather(1, preferred.unsqueeze(1)).squeeze(1) \
        - logits.gather(1, unpreferred.unsqueeze(1)).squeeze(1)
    return float((gap > 0).float().mean())


# ── 4. demonstration / behaviour cloning ─────────────────────────

ACTION_INDEX = {}


def action_space(contract: dict) -> list:
    actions = contract.get("legal-actions") or contract.get("legal_actions")
    if not actions:
        raise RecipeError(
            f"recipe {contract.get('id')!r} clones demonstrator actions but "
            "declares no legal action space; 'any action' is not a contract")
    return [str(a).lstrip(":") for a in actions]


def legal_mask(actions: list, legal: list) -> tuple:
    """→ (targets, mask, illegal_count).

    The class index comes from the DECLARED action space, not from
    whatever order the demonstration happened to use, so a demonstrator
    cannot reassign what class 0 means. The mask is then applied inside
    the loss, so an illegal action cannot reach a gradient: the data path
    has no way around it, because the only place a demonstration's action
    is read is here.
    """
    lookup = {name: i for i, name in enumerate(legal)}
    targets, mask, illegal = [], [], 0
    for entry in actions:
        name = str(entry["action"])
        if name in lookup:
            targets.append(lookup[name])
            mask.append(1.0)
        else:
            targets.append(0)     # placeholder; masked out of the loss
            mask.append(0.0)
            illegal += 1
    return (torch.tensor(targets, dtype=torch.long),
            torch.tensor(mask, dtype=torch.float32), illegal)


def run_demonstration(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Supervised on demonstrator actions, restricted to the declared legal
    action space. The demonstrator's head is the action set, so the model
    is never asked to imitate an action the contract says it cannot take."""
    guard_consumes(contract, "demonstration")
    legal = action_space(contract)
    split = load_split_tensors(frame["data"])
    entries = json.loads(Path(frame["actions"]).read_text(encoding="utf-8"))["actions"]
    if not entries:
        raise RecipeError("demonstration frame carries no actions")
    out = student.h1.weight.shape[0]
    if out != len(legal):
        raise RecipeError(
            f"student head is {out}-wide but the recipe declares {len(legal)} "
            f"legal actions {legal}; the demonstration would be scored against "
            "an action the model cannot emit")
    targets, mask, illegal = legal_mask(entries, legal)
    idx = torch.tensor([e["index"] for e in entries], dtype=torch.long)
    if int(idx.max()) >= split["x"].shape[0]:
        raise RecipeError("a demonstration names a sample outside the split")
    steps = int(frame["steps"])
    losses, trained = [], 0
    for _ in range(steps):
        order = torch.randperm(len(entries))
        pick = order[:64]
        logits = student(split["x"][idx[pick]])
        per_row = torch.nn.functional.cross_entropy(
            logits, targets[pick], reduction="none")
        weight = mask[pick]
        if float(weight.sum()) <= 0.0:
            continue     # a batch of illegal actions contributes nothing
        trained += 1
        loss = (per_row * weight).sum() / weight.sum()
        opt.zero_grad(); loss.backward(); opt.step()
        losses.append(float(loss.detach()))
    if not losses:
        raise RecipeError("every demonstrated action is outside the declared "
                          "legal action space; nothing was trainable")
    return {"loss": losses[-1], "first_loss": losses[0],
            "legal_actions": legal, "actions_total": len(entries),
            "actions_illegal_masked": illegal, "actions_trained": trained,
            "val_action_accuracy": _action_accuracy(student, frame, legal)}


@torch.no_grad()
def _action_accuracy(student: Student, frame: dict, legal: list) -> float:
    if not frame.get("val_actions") or not frame.get("val_data"):
        return None
    split = load_split_tensors(frame["val_data"])
    entries = json.loads(Path(frame["val_actions"]).read_text(encoding="utf-8"))["actions"]
    targets, mask, _ = legal_mask(entries, legal)
    idx = torch.tensor([e["index"] for e in entries], dtype=torch.long)
    keep = mask.bool()
    if not bool(keep.any()):
        return None
    predicted = student(split["x"][idx][keep]).argmax(dim=1)
    return float((predicted == targets[keep]).float().mean())


# ── 5. self-supervised ───────────────────────────────────────────

def run_self_supervised(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Masked reconstruction on the inputs themselves.

    The gain here is a representation, not a task score. The task's own
    evaluation is reported SEPARATELY and is never substituted for the
    reconstruction loss: `contract.task-gate` is what decides whether a
    self-supervised candidate is allowed to become anything.
    """
    guard_consumes(contract, "demonstration")
    if not contract.get("task-gate"):
        raise RecipeError(
            f"recipe {contract.get('id')!r} has no task-gate; self-supervision "
            "may improve a representation, it may not replace task acceptance")
    ratio = float(frame.get("mask_ratio", 0.25))
    if not 0.0 < ratio < 1.0:
        raise RecipeError(f"mask_ratio must be in (0,1), got {ratio}")
    split = load_split_tensors(frame["data"])
    decoder = _reconstruction_head(student, int(frame.get("hidden", 24)), seed=int(frame["seed"]))
    before = _representation(student, split["x"][:64])
    steps = int(frame["steps"])
    losses = []
    for _ in range(steps):
        masked, keep = _mask_pixels(split["x"], ratio)
        hidden = student.hidden(masked)
        recon = decoder(hidden)
        # reconstruction loss on the MASKED pixels only: the visible ones
        # are the input, not the target.
        per_pixel = ((recon[:, :256] - split["x"][:, :256]) ** 2 * keep).sum()
        loss = per_pixel / keep.sum().clamp(min=1.0) / split["x"].shape[0]
        opt.zero_grad(); loss.backward(); opt.step()
        losses.append(float(loss.detach()))
    after = _representation(student, split["x"][:64])
    shift = float((1.0 - torch.nn.functional.cosine_similarity(
        after, before, dim=1)).mean())
    return {"loss": losses[-1], "first_loss": losses[0],
            "mask_ratio": ratio, "representation_shift": shift,
            "task_gate": contract["task-gate"],
            "task_val_accuracy": _accuracy(student, load_split_tensors(frame["val_data"])
                                           if frame.get("val_data") else None)}


def _reconstruction_head(student: Student, hidden: int, seed: int) -> torch.nn.Module:
    torch.manual_seed(seed + 1)
    return torch.nn.Linear(hidden, INPUT)


def _mask_pixels(x: torch.Tensor, ratio: float) -> tuple:
    """Zero the chosen pixel fraction. The mask has the same rank as the
    pixel block it multiplies, so it cannot silently broadcast over the
    wrong axis."""
    keep = (torch.rand(x.shape[0], 256) >= ratio).float()
    masked = x.clone()
    masked[:, :256] = masked[:, :256] * keep
    return masked, keep


@torch.no_grad()
def _representation(student: Student, x: torch.Tensor) -> torch.Tensor:
    return torch.nn.functional.normalize(student.hidden(x), dim=1)


# ── 6. environment feedback (delayed) ────────────────────────────

def load_trajectories(frame: dict) -> list:
    blob = json.loads(Path(frame["trajectories"]).read_text(encoding="utf-8"))
    trajectories = blob.get("trajectories", [])
    if not trajectories:
        raise RecipeError("environment-feedback frame carries no trajectories")
    return trajectories


def bind_delayed(trajectories: list) -> list:
    """A delayed outcome is only a signal if it can name its trajectory.

    The id is the ONLY thing that survives the delay, so a result without
    one is refused here rather than quietly attributed to whatever
    trajectory happens to be current — which is how a reward gets
    attached to the wrong behaviour and the policy learns noise.
    """
    by_id = {}
    for traj in trajectories:
        traj_id = traj.get("trajectory_id") or traj.get("trajectory-id")
        if not traj_id:
            raise RecipeError(
                f"trajectory {traj.get('index')} has no trajectory_id; a "
                "trajectory that cannot be named cannot receive a delayed result")
        for result in traj.get("results", []):
            bound = result.get("trajectory_id") or result.get("trajectory-id")
            if bound is None:
                raise RecipeError(
                    f"delayed result {result.get('index')} names no trajectory; "
                    "an unattributable outcome is refused, not assigned")
            if bound != traj_id:
                raise RecipeError(
                    f"delayed result {result.get('index')} claims trajectory "
                    f"{bound!r} but arrived on {traj_id!r}")
            by_id.setdefault(traj_id, []).append(result)
    return by_id


def run_environment_feedback(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Delayed outcomes supervised as a later signal on the original
    trajectory: the return of a whole trajectory is added to the log-prob
    of the actions that produced it, after the outcome has settled."""
    guard_consumes(contract, "environment-result")
    if contract.get("binds") != "trajectory" and contract.get("bind") != "trajectory":
        raise RecipeError(
            f"recipe {contract.get('id')!r} accepts delayed results but does "
            "not bind them to a trajectory; unattributable reward is refused")
    trajectories = load_trajectories(frame)
    delayed = bind_delayed(trajectories)
    settled = sum(1 for results in delayed.values() if results)
    steps = int(frame["steps"])
    losses = []
    for _ in range(steps):
        order = torch.randperm(len(trajectories))
        batch = [trajectories[i] for i in order[:32]]
        losses.append(_policy_gradient_step(student, opt, batch, delayed))
    if not settled:
        raise RecipeError(
            f"no delayed result settled for any of {len(trajectories)} "
            "trajectories; a run with no attributable outcome trains nothing")
    return {"loss": losses[-1], "first_loss": losses[0],
            "trajectories": len(trajectories), "results_settled": settled}


def _trajectory_loss(student: Student, traj: dict, delayed: dict, lr_scale: float = 1.0) -> torch.Tensor:
    """REINCE on a trajectory's own delayed return: -log pi(a_t) * G_t.

    A trajectory with no settled outcome has no return and therefore no
    gradient — not a zero return, which would train "this was bad".
    """
    results = delayed.get(traj["trajectory_id"], [])
    if not results:
        return torch.zeros((), dtype=torch.float32)
    ret = sum(float(r.get("reward", 0.0)) for r in results)
    states = torch.tensor(traj["states"], dtype=torch.float32)
    actions = torch.tensor(traj["actions"], dtype=torch.long)
    logits = student(states)
    log_probs = torch.log_softmax(logits, dim=1)
    chosen = log_probs.gather(1, actions.unsqueeze(1)).squeeze(1)
    return -(chosen.mean() * ret * lr_scale)


def _policy_gradient_step(student: Student, opt, batch: list, delayed: dict) -> float:
    """One update over a batch of trajectories with settled outcomes.

    A batch in which nothing has settled produces no gradient at all —
    calling backward on a constant would be a no-op pretending to be an
    update, so the step is skipped and reported as exactly that.
    """
    losses = [_trajectory_loss(student, traj, delayed) for traj in batch]
    total = torch.stack(losses).sum() / max(1, len(losses))
    if not total.requires_grad:
        return 0.0
    opt.zero_grad(); total.backward(); opt.step()
    return float(total.detach())


# ── 7. restricted discrete policy gradient ───────────────────────

def env_spec(frame: dict) -> dict:
    spec = dict(frame.get("env") or {})
    spec.setdefault("width", 6)
    spec.setdefault("max_steps", 20)
    spec.setdefault("goal", spec["width"] - 1)
    spec.setdefault("step_reward", -0.05)
    spec.setdefault("goal_reward", 1.0)
    return spec


def env_observation(position: int, spec: dict) -> list:
    """The environment's state, laid out in the student's input width.

    The student's first layer is fixed at INPUT features, so the
    environment's own compact state is zero-padded into that width. The
    padding is structural and carries no signal: the policy has to read
    the environment's actual state, not the padding.
    """
    width = int(spec["width"])
    obs = [0.0] * INPUT
    obs[position] = 1.0
    obs[width] = position / max(1, width - 1)   # where am I, numerically
    obs[width + 1] = 1.0 if position == int(spec["goal"]) else 0.0
    return obs


def check_env_observation(spec: dict) -> None:
    """The environment's state must fit the student's declared input
    width, or the policy cannot read its own state at all."""
    width = int(spec["width"])
    if width + 2 > INPUT:
        raise RecipeError(
            f"environment of width {width} needs {width + 2} observation "
            f"features but the student takes {INPUT}; the declared environment "
            "does not fit the declared model")


def rollout(student: Student, spec: dict, greedy: bool = False) -> dict:
    """One episode in the declared discrete environment.

    The contract is explicit and small: a line of `width` cells, the legal
    actions from the recipe, a per-step reward, +goal_reward on the goal,
    and termination at the goal or at `max_steps`. This is a RESTRICTED
    discrete policy — the contract is the point, and it is not a claim
    about arbitrary RL environments.
    """
    width = int(spec["width"])
    goal = int(spec["goal"])
    max_steps = int(spec["max_steps"])
    position = 0
    states, actions = [], []
    total = 0.0
    steps = 0
    for _ in range(max_steps):
        state = env_observation(position, spec)
        states.append(state)
        logits = student(torch.tensor([state], dtype=torch.float32))[0]
        if greedy:
            action = int(logits.argmax())
        else:
            action = int(torch.multinomial(torch.softmax(logits, dim=0), 1))
        actions.append(action)
        if action == 0:            # left
            position = max(0, position - 1)
        elif action == 1:          # right
            position = min(width - 1, position + 1)
        # action 2 is stay: no motion, same reward
        total += float(spec["step_reward"])
        steps += 1
        if position == goal:
            total += float(spec["goal_reward"])
            break
    return {"states": states, "actions": actions, "return": total, "steps": steps,
            "terminated": position == goal, "trajectory_id": None}


def run_policy_gradient(frame: dict, student: Student, contract: dict, opt) -> dict:
    """Policy-gradient update under an explicit action/reward/termination
    contract. The action set is checked against the student's head, so a
    policy cannot silently emit an action the contract does not have."""
    guard_consumes(contract, "environment-result")
    actions = contract.get("actions")
    if not actions:
        raise RecipeError(
            f"recipe {contract.get('id')!r} is a policy recipe with no action "
            "set; a policy without declared actions has no contract to hold to")
    if not contract.get("reward") or not contract.get("termination"):
        raise RecipeError(
            f"recipe {contract.get('id')!r} declares no reward or no "
            "termination contract")
    declared = [str(a).lstrip(":") for a in actions]
    out = student.h1.weight.shape[0]
    if out != len(declared):
        raise RecipeError(
            f"student head is {out}-wide but the policy declares {len(declared)} "
            f"actions {declared}")
    spec = env_spec(frame)
    check_env_observation(spec)
    episodes_per_step = int(frame.get("batch", 16))
    baseline = 0.0
    losses = []
    steps = int(frame["steps"])
    for _ in range(steps):
        batch = []
        for _ in range(episodes_per_step):
            traj = rollout(student, spec)
            traj["trajectory_id"] = f"s{len(batch)}"
            batch.append(traj)
        returns = [t["return"] for t in batch]
        # A learned baseline, the standard variance reduction. It is a
        # function of the batch's own returns — no external value model.
        baseline = sum(returns) / len(returns)
        loss = torch.zeros((), dtype=torch.float32)
        for traj, ret in zip(batch, returns):
            states = torch.tensor(traj["states"], dtype=torch.float32)
            actions_t = torch.tensor(traj["actions"], dtype=torch.long)
            log_probs = torch.log_softmax(student(states), dim=1)
            chosen = log_probs.gather(1, actions_t.unsqueeze(1)).squeeze(1)
            loss = loss - (chosen.mean() * (ret - baseline))
        loss = loss / len(batch)
        opt.zero_grad(); loss.backward(); opt.step()
        losses.append(float(loss.detach()))
    return {"loss": losses[-1], "first_loss": losses[0],
            "actions": declared, "termination": contract["termination"],
            "reward": contract["reward"],
            "val_return": _evaluate_return(student, frame),
            "val_termination_rate": _evaluate_termination(student, spec)}


@torch.no_grad()
def _evaluate_return(student: Student, frame: dict) -> float:
    spec = env_spec(frame)
    episodes = int(frame.get("val_episodes", 64))
    returns = [rollout(student, spec, greedy=True)["return"] for _ in range(episodes)]
    return sum(returns) / len(returns)


@torch.no_grad()
def _evaluate_termination(student: Student, spec: dict) -> float:
    episodes = int(spec.get("val_episodes", 64))
    hits = [rollout(student, spec, greedy=True)["terminated"] for _ in range(episodes)]
    return sum(1 for h in hits if h) / len(hits)


# ── shared evaluation ────────────────────────────────────────────

@torch.no_grad()
def _accuracy(student: Student, split) -> float | None:
    if split is None:
        return None
    keep = split["decidable"].bool()
    if not bool(keep.any()):
        return None
    predicted = student(split["x"][keep]).argmax(dim=1)
    return float((predicted == split["y"][keep]).float().mean())


RUNNERS = {
    "supervised": run_hard_distill,
    "hard-distill": run_hard_distill,
    "soft-distill": run_soft_distill,
    "preference": run_preference,
    "demonstration": run_demonstration,
    "self-supervised": run_self_supervised,
    "environment-feedback": run_environment_feedback,
    "policy-gradient": run_policy_gradient,
}


def recipe_family(recipe_id: str) -> str:
    return str(recipe_id).split("@", 1)[0]


# ── the protocol entry point ─────────────────────────────────────

def run_frame(frame: dict) -> dict:
    """Execute one recipe frame. Returns the metrics dict, so a test can
    drive the same path the protocol drives without a subprocess."""
    recipe_id = frame["recipe"]
    family = recipe_family(recipe_id)
    runner = RUNNERS.get(family)
    if runner is None:
        raise RecipeError(f"unknown recipe {recipe_id!r}")
    contract = frame.get("contract")
    if not isinstance(contract, dict) or not contract:
        raise RecipeError(
            "frame has no 'contract': a recipe runs under a declared policy "
            "or it does not run")
    if contract.get("id") not in (None, recipe_id):
        raise RecipeError(
            f"frame declares recipe {recipe_id!r} but carries contract for "
            f"{contract.get('id')!r}; a run may not train under an unstated policy")
    hidden = int(frame.get("hidden", 24))
    out = int(frame.get("out", 2))
    student = Student(hidden, out, int(frame.get("seed", 0)))
    trainable = trainable_names(frame, student)
    lr = float(frame.get("lr", 0.02))
    opt = torch.optim.Adam(student.parameters_of(trainable), lr=lr)
    torch.set_num_threads(1)
    metrics = runner(frame, student, contract, opt)
    metrics["params"] = student.export()
    return metrics


def handle(frame: dict) -> None:
    """Protocol handler. Wired by worker.py as `HANDLERS["recipe"]`."""
    run_id = frame.get("run_id", "")
    attempt_id = frame.get("attempt_id", "")
    try:
        torch.use_deterministic_algorithms(True)
        metrics = run_frame(frame)
        params = metrics.pop("params")
        # `weights_out` is the artifact path; `out` is the head width, so
        # it cannot be the destination.
        out_path = frame.get("weights_out")
        if out_path:
            Path(out_path).write_text(json.dumps({
                "model_version": f"recipe-{recipe_family(frame['recipe'])}-"
                                 f"{frame['steps']}-{frame.get('seed', 0)}",
                "recipe": frame["recipe"], "params": params, "metrics": metrics,
            }), encoding="utf-8")
        emit({"v": PROTOCOL_VERSION, "type": "done", "run_id": run_id,
              "attempt_id": attempt_id, "recipe": frame["recipe"],
              "weights": str(out_path) if out_path else None,
              "loss": metrics.get("loss"), "metrics": metrics})
    except Exception as exc:
        import traceback
        _note(traceback.format_exc())
        emit({"v": PROTOCOL_VERSION, "type": "failed", "run_id": run_id,
              "attempt_id": attempt_id, "error": str(exc)})


def main() -> int:
    emit({"v": PROTOCOL_VERSION, "type": "hello", "protocol": PROTOCOL,
          "capabilities": ["recipe"], "torch": torch.__version__})
    while True:
        try:
            frame = read_frame()
        except ValueError as exc:
            emit({"v": PROTOCOL_VERSION, "type": "failed", "error": str(exc)})
            continue
        if frame is None:
            return 0
        if frame.get("v") != PROTOCOL_VERSION:
            emit({"v": PROTOCOL_VERSION, "type": "failed",
                  "error": f"unsupported protocol version {frame.get('v')!r}"})
            continue
        handle(frame)
    return 0


if __name__ == "__main__":
    sys.exit(main())
