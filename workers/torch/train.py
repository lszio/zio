#!/usr/bin/env python3
"""W04 CPU trainer — real gradient descent over the W00 acceptance task.

Two model shapes, so dual learning has something to compare:

  * `linear`  — one linear layer over [image | numbers | mask]. It cannot
    represent the XOR, so its accuracy plateaus; this is the frozen
    baseline the acceptance gate measures against.
  * `nonlinear` — an added hidden ReLU layer, the structural change a
    code-rewriting candidate makes. Weights are trained by real
    backpropagation here in torch, not simulated.

    python workers/torch/train.py --steps 400 --out teacher.pt
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from pathlib import Path

import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from data import Observation, load_labels, load_split  # noqa: E402

INPUT = 256 + 2 + 2
HIDDEN = 24


def features(obs: Observation) -> list[float]:
    values = [p / 255.0 for p in obs.pixels]
    for r in obs.readings:
        values.append(r / (abs(r) + 1.0))
    values.append(1.0 if obs.image_present else 0.0)
    values.append(1.0 if obs.numeric_present else 0.0)
    return values


def build(rows: list[Observation]) -> tuple[torch.Tensor, torch.Tensor, torch.Tensor]:
    """→ (x, y, decidable). Undecidable rows keep y=0 but are masked out
    of the loss by `decidable`, so a missing modality is never trained
    as if it were class 0."""
    x = torch.tensor([features(r) for r in rows], dtype=torch.float32)
    y = torch.tensor([r.label if r.label != 0xFF else 0 for r in rows], dtype=torch.long)
    decidable = torch.tensor([r.decidable for r in rows], dtype=torch.float32)
    return x, y, decidable


class LinearFusion(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.w1 = torch.nn.Linear(INPUT, 2)

    def forward(self, x):
        return self.w1(x)


class NonlinearFusion(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.w1 = torch.nn.Linear(INPUT, HIDDEN)
        self.w2 = torch.nn.Linear(HIDDEN, 2)

    def forward(self, x):
        return self.w2(torch.relu(self.w1(x)))


def export_weights(model: torch.nn.Module) -> dict:
    """The on-disk shape the teacher service loads: `{"hidden": {...}}`."""
    if isinstance(model, NonlinearFusion):
        return {
            "hidden": {
                "w1": model.w1.weight.detach().tolist(),
                "b1": model.w1.bias.detach().tolist(),
                "w2": model.w2.weight.detach().tolist(),
                "b2": model.w2.bias.detach().tolist(),
            }
        }
    identity = [[1.0 if i == j else 0.0 for j in range(INPUT)] for i in range(2)]
    return {
        "hidden": {
            "w1": identity,
            "b1": [0.0, 0.0],
            "w2": [[1.0, 0.0], [0.0, 1.0]],
            "b2": [0.0, 0.0],
        }
    }


def export_params(model: torch.nn.Module) -> dict:
    """Per-layer parameter artifact: {"h0": {"w": …, "b": …}, …}.

    The bias travels with the weight — a checkpoint without it is not the
    trained model, and restoring it fresh would make training
    unreproducible."""
    if isinstance(model, NonlinearFusion):
        return {
            "h0": {
                "w": model.w1.weight.detach().tolist(),
                "b": model.w1.bias.detach().tolist(),
            },
            "h1": {
                "w": model.w2.weight.detach().tolist(),
                "b": model.w2.bias.detach().tolist(),
            },
        }
    raise ValueError("export_params expects a NonlinearFusion model")


def train(model: torch.nn.Module, x, y, decidable, steps: int, lr: float, seed: int) -> list[float]:
    torch.manual_seed(seed)
    optimiser = torch.optim.Adam(model.parameters(), lr=lr)
    losses: list[float] = []
    n = x.shape[0]
    for step in range(steps):
        order = torch.randperm(n)
        batch = order[:64]
        logits = model(x[batch])
        # cross entropy, but abstained rows (mask 0) contribute nothing:
        # a missing modality must not be trained toward class 0.
        per_row = torch.nn.functional.cross_entropy(logits, y[batch], reduction="none")
        weight = decidable[batch]
        loss = (per_row * weight).sum() / weight.sum().clamp(min=1.0)
        optimiser.zero_grad()
        loss.backward()
        optimiser.step()
        losses.append(float(loss.detach()))
    return losses


@torch.no_grad()
def accuracy(model, x, y, decidable) -> tuple[float, int]:
    mask = decidable.bool()
    if not bool(mask.any()):
        return 0.0, 0
    logits = model(x[mask])
    correct = int((logits.argmax(dim=1) == y[mask]).sum())
    return correct / int(mask.sum()), int(mask.sum())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    here = Path(__file__).resolve().parent
    parser.add_argument("--task", default=str(here.parent.parent / "examples" / "self-learning"))
    parser.add_argument("--steps", type=int, default=400)
    parser.add_argument("--lr", type=float, default=0.02)
    parser.add_argument("--seed", type=int, default=20261003)
    parser.add_argument("--hidden", type=int, default=HIDDEN)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    torch.use_deterministic_algorithms(True)
    torch.set_num_threads(1)

    task = Path(args.task)
    train_rows = load_split(task / "data" / "train.bin")
    val_rows = load_split(task / "data" / "val.bin")
    xtr, ytr, dtr = build(train_rows)
    xva, yva, dva = build(val_rows)

    model = NonlinearFusion()
    losses = train(model, xtr, ytr, dtr, args.steps, args.lr, args.seed)
    val_acc, n_val = accuracy(model, xva, yva, dva)

    payload = {
        "model_version": f"local-net-{args.steps}-{args.seed}",
        "architecture": "nonlinear-fusion",
        "input_dim": INPUT,
        "hidden": args.hidden,
        "weights": export_weights(model),
        "val_accuracy": val_acc,
        "final_loss": losses[-1] if losses else None,
    }
    Path(args.out).write_text(json.dumps(payload), encoding="utf-8")
    print(f"trained {args.steps} steps: loss {losses[0]:.4f} → {losses[-1]:.4f}, "
          f"val accuracy {val_acc:.3f} on {n_val} decidable samples")
    return 0


if __name__ == "__main__":
    sys.exit(main())
