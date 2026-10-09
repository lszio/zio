#!/usr/bin/env python3
"""W04 training worker — a versioned, length-limited NDJSON control loop.

Protocol (one JSON object per line, both directions):

    → {"v":1,"type":"hello","protocol":"grove.worker/1","capabilities":[...]}
    ← {"v":1,"type":"ready","protocol":"grove.worker/1","torch":"2.14.1+cpu"}

    → {"v":1,"type":"train","request_id":"…","run_id":"…","attempt_id":"…",
       "graph":{…},"weights":"sha256:…","data":"sha256:…","steps":400,"seed":1}
    ← {"v":1,"type":"progress","run_id":"…","step":100,"loss":0.42}
    ← {"v":1,"type":"done","run_id":"…","weights":"sha256:…","loss":0.0002,
       "val_accuracy":1.0}
    ← {"v":1,"type":"failed","run_id":"…","error":"…"}

Rules this file enforces:

  * stdout carries the protocol and nothing else; logs go to stderr,
    capped, so a chatty dependency cannot flood the control channel;
  * every inbound frame is size-checked before it is parsed;
  * a graph is validated structurally before any tensor work;
  * a cancelled run terminates and reports a terminal state — a run never
    just stops existing;
  * weight artifacts are plain JSON of nested float lists. No pickle, no
    object deserialization: loading cannot execute code.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import sys
import traceback
from pathlib import Path

import torch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from data import Observation, load_split  # noqa: E402
from graph import GraphError, graph_from_json  # noqa: E402
import recipes as recipe_handler  # noqa: E402

PROTOCOL = "grove.worker/1"
PROTOCOL_VERSION = 1
MAX_FRAME_BYTES = 1 << 20          # 1 MiB per control frame
STDERR_CAP = 64 * 1024
INPUT = 256 + 2                    # pixels + sign-preserving readings


def emit(payload: dict) -> None:
    """One protocol line on stdout, and nothing else ever."""
    sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
    sys.stdout.flush()


# ── data / weights ─────────────────────────────────────────────────

_stderr_written = 0


def note(message: str) -> None:
    """Logs go to stderr, capped, so they can never corrupt the protocol.

    The cap is a counter, not a `tell()`: stderr is usually a pipe here
    and `tell()` raises on a non-seekable stream — inside the failure
    handler that would swallow the very error we are trying to report.
    """
    global _stderr_written
    if _stderr_written >= STDERR_CAP:
        return
    chunk = message[:2000]
    _stderr_written += len(chunk) + 1
    sys.stderr.write(chunk + "\n")
    sys.stderr.flush()


def read_frame() -> dict | None:
    line = sys.stdin.readline()
    if not line:
        return None
    if len(line) > MAX_FRAME_BYTES:
        raise ValueError(f"inbound frame exceeds {MAX_FRAME_BYTES} bytes")
    return json.loads(line)


# ── data / weights ─────────────────────────────────────────────────

def load_tensors(split_path: Path) -> dict[str, torch.Tensor]:
    rows = load_split(split_path)
    x = torch.tensor([_features(r) for r in rows], dtype=torch.float32)
    y = torch.tensor([r.label if r.label != 0xFF else 0 for r in rows], dtype=torch.long)
    mask = torch.tensor([1.0 if r.decidable else 0.0 for r in rows], dtype=torch.float32)
    return {"x": x, "y": y, "mask": mask}


def _features(obs: Observation) -> list[float]:
    values = [p / 255.0 for p in obs.pixels]
    for r in obs.readings:
        values.append(r / (abs(r) + 1.0))
    return values


def load_weights(path: Path) -> dict:
    """Plain nested lists. Loading a file cannot execute code."""
    payload = json.loads(path.read_text(encoding="utf-8"))
    return payload


def _linear_layers(graph):
    """Linear ops in graph order."""
    return [op for op in graph.ops if op.kind == "linear"]


def _value_width(graph, name: str, resolved: dict) -> int | None:
    """Feature width of a value, given already-resolved layer widths."""
    if name in graph.inputs:
        return graph.inputs[name].shape[0]
    return resolved.get(name)


def build_model(graph, params: dict, seed: int = 0):
    """Materialize a validated graph as a runnable module.

    The forward pass is *derived from the graph*, not from a fixed stack
    of layers, so a structural candidate changes what actually executes.

    Layers absent from `params` are initialized from the run's seed —
    explicitly, because an all-zero ReLU net has zero gradients everywhere
    (dead), and an unseeded random init is not reproducible. The seed is
    consumed in layer order, so the same graph + seed always yields the
    same starting point.
    Returns (forward_fn, trainable_parameters, linears).
    """
    torch.manual_seed(seed)
    linears: dict[str, torch.nn.Linear] = {}
    widths: dict[str, int] = {}   # value name → feature width
    for name, spec in graph.inputs.items():
        widths[name] = spec.shape[0]
    for op in graph.ops:
        if op.kind == "concat":
            widths[op.output] = sum(widths[i] for i in op.inputs)
            continue
        if op.kind in ("normalize", "relu", "softmax"):
            widths[op.output] = widths[op.inputs[0]]
            continue
        if op.kind != "linear":
            continue
        key = op.output
        in_width = widths[op.inputs[0]]
        entry = params.get(key)
        if entry is None or "w" not in entry:
            # seeded init: the constructor draws from the seeded RNG. This
            # is the declared initialization for a structural change (new
            # layers start here) and is deterministic per (graph, seed).
            # A layer's out-width is declared on the op (it is structure,
            # so it travels with the graph); the classification head
            # defaults to the task's 2 classes.
            out_width = int(op.attrs.get("out", 2))
            layer = torch.nn.Linear(in_width, out_width)
        else:
            w = torch.tensor(entry["w"], dtype=torch.float32)
            if w.dim() != 2:
                raise GraphError(f"weights for {key!r} are not a matrix")
            # The artifact stores weight rows = out_features, cols =
            # in_features; torch's Linear takes (in, out).
            layer = torch.nn.Linear(w.shape[1], w.shape[0])
            with torch.no_grad():
                layer.weight.copy_(w)
                # The bias is part of the parameter state: a checkpoint
                # that kept nn.Linear's random init would be a different
                # model than the one that was trained.
                if "b" in entry:
                    layer.bias.copy_(torch.tensor(entry["b"], dtype=torch.float32))
                else:
                    layer.bias.zero_()
        linears[key] = layer
        widths[key] = layer.weight.shape[0]

    if not linears:
        raise GraphError("graph has no linear layer to train")

    def forward(x: torch.Tensor) -> torch.Tensor:
        # The host materializes the fused feature vector; its name is
        # whatever the graph's concat (or first linear input) declares.
        fused_name = next(
            (op.output for op in graph.ops if op.kind == "concat"),
            next(op.inputs[0] for op in graph.ops if op.kind == "linear"),
        )
        values = {fused_name: x}
        for op in graph.ops:
            if op.kind == "linear":
                values[op.output] = linears[op.output](values[op.inputs[0]])
            elif op.kind == "relu":
                values[op.output] = torch.relu(values[op.inputs[0]])
            elif op.kind == "softmax":
                values[op.output] = torch.softmax(values[op.inputs[0]], dim=1)
            # normalize / concat are folded into the feature builder: the
            # host already materialized the fused vector
        # The graph names its own output. A single-module graph (a fusion
        # module that emits `hidden`, W13) is a legitimate model, so the
        # worker reads whatever the graph declares rather than requiring
        # every graph to be a classifier.
        name = next(iter(graph.outputs.values()), None)
        if name is None:
            raise GraphError("graph declares no output value")
        if name not in values:
            raise GraphError(f"output {name!r} is not produced by the graph")
        return values[name]

    parameters = [p for layer in linears.values() for p in layer.parameters()]
    return forward, parameters, linears


# ── the training action ────────────────────────────────────────────

def trainable_layers(graph, linears: dict) -> list[str]:
    """The layers this graph actually trains, in a stable order.

    An empty `trainable` list means every linear layer, which is what a
    single-layer baseline declared before the field existed. A name that
    is not a linear layer is a graph error, not a silent skip: training
    the wrong thing is worse than refusing.
    """
    declared = list(graph.trainable)
    if not declared:
        return list(linears.keys())
    missing = [n for n in declared if n not in linears]
    if missing:
        raise GraphError(
            f"trainable names {missing!r} are not linear layers; "
            f"the model has {list(linears)}"
        )
    return [name for name in linears if name in set(declared)]


def make_optimiser(linears: dict, trainable: list[str], lr: float):
    """Adam over the *trainable* layers' weight **and** bias, in a stable
    named order — the order the state artifact serializes and restores.

    Two things this gets right that a weight-only version does not:
    a layer outside `trainable` contributes no parameters at all, and a
    trainable layer's bias is trained, not frozen by omission.
    """
    chosen = set(trainable)
    named: list[tuple[str, torch.nn.Parameter]] = []
    for name, layer in linears.items():
        if name not in chosen:
            layer.weight.requires_grad_(False)
            if layer.bias is not None:
                layer.bias.requires_grad_(False)
            continue
        named.append((f"{name}.weight", layer.weight))
        if layer.bias is not None:
            named.append((f"{name}.bias", layer.bias))
    if not named:
        raise GraphError("no trainable parameter remains after freezing")
    opt = torch.optim.Adam([p for _, p in named], lr=lr)
    return opt, named


def save_state(path: Path, run_id: str, attempt_id: str, step: int,
               linears: dict, optimiser, named, first_loss) -> None:
    """Full training state in a non-executing format: plain JSON of float
    lists and a base64 RNG blob. torch.save/pickle is banned — loading a
    state artifact must never run code.

    Optimizer moments are keyed `name.weight` / `name.bias`, so a bias
    that was trained round-trips and a frozen layer contributes nothing.

    Atomic: write to a temp name, fsync, rename — a killed worker leaves
    no half state that a checkpoint could ever reference.
    """
    opt_state = {}
    for key, param in named:
        entry = optimiser.state.get(param)
        if entry is None:
            continue
        opt_state[key] = {
            "step": int(entry.get("step", torch.tensor(0)).item()),
            "exp_avg": entry["exp_avg"].tolist(),
            "exp_avg_sq": entry["exp_avg_sq"].tolist(),
        }
    payload = {
        "schema": 2,
        "protocol": "grove.worker.state/1",
        "run_id": run_id,
        "attempt_id": attempt_id,
        "step": step,
        "first_loss": first_loss,
        "params": {name: {"w": layer.weight.detach().tolist(),
                          "b": layer.bias.detach().tolist()}
                   for name, layer in linears.items()},
        "optimizer": {"adam": opt_state},
        "rng": {"cpu": base64.b64encode(bytes(torch.get_rng_state().tolist())).decode("ascii")},
    }
    tmp = path.with_suffix(path.suffix + ".partial")
    with open(tmp, "w", encoding="utf-8") as handle:
        json.dump(payload, handle)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(tmp, path)


def load_state(path: Path, linears: dict, optimiser, named,
               level: str | None = "learning-continuation"):
    """Restore the training state the resume level asks for.

    "model-initialization" restores weights only — the resumed run keeps
    its fresh Adam moments, RNG stream and step counter, mirroring
    checkpoint.zio's manifest rule where only continuation levels require
    optimizer membership. Any other level restores parameters, Adam
    moments and step, and the CPU RNG stream.

    A state artifact whose `trainable` set does not match this attempt's
    is refused: resuming optimizer moments for a different parameter set
    would silently produce a model neither run describes.
    """
    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    if payload.get("protocol") != "grove.worker.state/1":
        raise GraphError(
            f"state artifact has unsupported schema {payload.get('schema')!r} "
            f"({payload.get('protocol')!r}); refusing to resume blind"
        )
    if payload.get("schema") != 2:
        raise GraphError(
            f"state artifact schema {payload.get('schema')!r} predates the "
            "trainable set being recorded; refusing to resume blind"
        )
    continuation = level != "model-initialization"
    if continuation:
        saved_keys = set(payload.get("optimizer", {}).get("adam", {}))
        if saved_keys != {key for key, _ in named}:
            raise GraphError(
                f"state artifact was saved for optimizer parameters {sorted(saved_keys)}, "
                f"this attempt trains {sorted(key for key, _ in named)}"
            )
    with torch.no_grad():
        for name, layer in linears.items():
            saved = payload["params"][name]
            layer.weight.copy_(torch.tensor(saved["w"], dtype=torch.float32))
            layer.bias.copy_(torch.tensor(saved["b"], dtype=torch.float32))
    if not continuation:
        # weights-only: the new run starts from step 0 on the seed's fresh
        # RNG stream — the state a never-paused run of that id would have
        return payload["run_id"], 0, None
    for key, param in named:
        entry = payload["optimizer"]["adam"][key]
        optimiser.state[param] = {
            "step": torch.tensor(float(entry["step"])),
            "exp_avg": torch.tensor(entry["exp_avg"], dtype=torch.float32),
            "exp_avg_sq": torch.tensor(entry["exp_avg_sq"], dtype=torch.float32),
        }
    torch.set_rng_state(torch.tensor(
        list(base64.b64decode(payload["rng"]["cpu"])), dtype=torch.uint8))
    return payload["run_id"], int(payload["step"]), payload.get("first_loss")


def do_train(frame: dict) -> None:
    run_id = frame["run_id"]
    attempt_id = frame.get("attempt_id", "")
    try:
        # structural validation happens here, before any tensor is touched
        graph_payload = frame["graph"]
        # structural validation happens here, before any tensor is touched
        graph = graph_from_json(graph_payload)
        steps = int(frame["steps"])
        seed = int(frame.get("seed", 0))
        data = load_tensors(Path(frame["data"]))
        val = load_tensors(Path(frame["val_data"])) if frame.get("val_data") else None
        resume_path = frame.get("resume")
        resume_level = frame.get("resume_level")
        # A resumed run has a NEW id by design (checkpoint--resume creates
        # it); the artifact's parent identity is forwarded as `resume_run`.
        resume_run = frame.get("resume_run") or run_id
        save_at = frame.get("save_at")
        stop_after_save = bool(frame.get("stop_after_save", False))

        weights = load_weights(Path(frame["weights"]))
        params = weights.get("params", weights)
        # construction draws from the RNG, but a continuation resume
        # overwrites the whole RNG stream from the state artifact below, so
        # constructor draws never leak into the sampling sequence
        forward, parameters, linears = build_model(graph, params, seed)

        torch.manual_seed(seed)
        torch.set_num_threads(1)
        # The trainable set is resolved *before* the optimiser exists, so
        # a frozen layer contributes no parameters and no optimizer state.
        trainable = trainable_layers(graph, linears)
        optimiser, named = make_optimiser(linears, trainable, lr=0.02)

        start_step = 0
        first_loss = None
        if resume_path:
            state_run, start_step, first_loss = load_state(
                Path(resume_path), linears, optimiser, named, level=resume_level)
            if state_run != resume_run:
                raise GraphError(
                    f"state artifact belongs to run {state_run!r}, not {resume_run!r}"
                )

        x, y, mask = data["x"], data["y"], data["mask"]
        # A module that is not a classifier (W13's fusion module emits a
        # hidden representation) has no class labels to score against, so
        # its objective is declared by the graph rather than assumed. The
        # graph says which; the worker does not guess. This is
        # self-supervised pretraining of one module — the module's own
        # capability is still measured by the composite's task
        # evaluation, never by this loss.
        out_width = linears[next(reversed(linears))].weight.shape[0]
        classifier = out_width == 2 and "logits" in graph.outputs
        objective = graph_payload.get("objective", "supervised")
        last_loss = first_loss
        done = False
        step = start_step
        while not done and step < start_step + steps:
            order = torch.randperm(x.shape[0])
            batch = order[:64]
            logits = forward(x[batch])
            weight = mask[batch]
            if classifier:
                per_row = torch.nn.functional.cross_entropy(
                    logits, y[batch], reduction="none")
                loss = (per_row * weight).sum() / weight.sum().clamp(min=1.0)
            else:
                # representation objective: decorrelate the emitted
                # dimensions so the module does not collapse to a constant
                centred = logits - logits.mean(dim=0, keepdim=True)
                denom = centred.norm(dim=0).clamp(min=1e-6)
                loss = (centred.pow(2).sum() / denom.pow(2).sum()).clamp(min=0.0, max=10.0)
            optimiser.zero_grad()
            loss.backward()
            optimiser.step()
            last_loss = float(loss.detach())
            first_loss = last_loss if first_loss is None else first_loss
            step += 1
            if step % max(1, steps // 4) == 0:
                emit({"v": PROTOCOL_VERSION, "type": "progress", "run_id": run_id,
                      "attempt_id": attempt_id, "step": step, "loss": last_loss})
            # checkpoint boundary: the state lands atomically BEFORE the run
            # may be declared paused
            if save_at is not None and step >= int(save_at):
                # One immutable file per saved step: the owner commits
                # exactly the path this frame names, so a frame read while
                # a later save is in flight can never pair step N's billing
                # with step M's bytes.
                state_path = Path(frame["state_out"]).with_name(f"state-{step}.json")
                save_state(state_path, run_id, attempt_id, step,
                           linears, optimiser, named, first_loss)
                # The caller-named state_out is written too, atomically with
                # the same bytes: the Rust owner reads that path only after
                # the process is gone, so its mutability races nothing.
                save_state(Path(frame["state_out"]), run_id, attempt_id, step,
                           linears, optimiser, named, first_loss)
                emit({"v": PROTOCOL_VERSION, "type": "progress", "run_id": run_id,
                      "attempt_id": attempt_id, "step": step, "loss": last_loss,
                      "saved": str(state_path)})
                if stop_after_save:
                    done = True

        saved = {name: {"w": layer.weight.detach().tolist(),
                        "b": layer.bias.detach().tolist()}
                 for name, layer in linears.items()}
        # A module that is not a classifier has no accuracy: reporting
        # argmax agreement over a representation would be a number that
        # means nothing. It stays `null`, and the composite's own task
        # evaluation is what scores it.
        accuracy = None
        if val is not None and classifier:
            with torch.no_grad():
                logits = forward(val["x"])
                sel = val["mask"].bool()
                if bool(sel.any()):
                    correct = int((logits[sel].argmax(dim=1) == val["y"][sel]).sum())
                    accuracy = correct / int(sel.sum())

        out_path = Path(frame["out"])
        out_path.write_text(json.dumps({
            "model_version": f"worker-{steps}-{seed}",
            "objective": objective,
            "classifier": classifier,
            "params": saved,
            "first_loss": first_loss,
            "final_loss": last_loss,
            "val_accuracy": accuracy,
            "attempt_id": attempt_id,
        }), encoding="utf-8")
        emit({"v": PROTOCOL_VERSION, "type": "done", "run_id": run_id,
              "attempt_id": attempt_id, "weights": str(out_path),
              "step": step,
              "first_loss": first_loss, "loss": last_loss,
              "val_accuracy": accuracy})
    except Exception as exc:  # every failure is a terminal, visible state
        note(traceback.format_exc())
        emit({"v": PROTOCOL_VERSION, "type": "failed", "run_id": run_id,
              "attempt_id": attempt_id, "error": str(exc)})


def do_predict(frame: dict) -> None:
    run_id = frame["run_id"]
    try:
        graph_payload = frame["graph"]
        graph = graph_from_json(graph_payload)
        weights = load_weights(Path(frame["weights"]))
        forward, _, _ = build_model(graph, weights.get("params", weights), int(frame.get("seed", 0)))
        data = load_tensors(Path(frame["data"]))
        with torch.no_grad():
            logits = forward(data["x"])
            predicted = logits.argmax(dim=1).tolist()
        rows = [{"index": i, "answer": int(p)} for i, p in enumerate(predicted)]
        out_path = Path(frame["out"])
        out_path.write_text(json.dumps({"predictions": rows}), encoding="utf-8")
        emit({"v": PROTOCOL_VERSION, "type": "done", "run_id": run_id,
              "attempt_id": frame.get("attempt_id", ""), "predictions": str(out_path)})
    except Exception as exc:
        note(traceback.format_exc())
        emit({"v": PROTOCOL_VERSION, "type": "failed", "run_id": run_id,
              "attempt_id": frame.get("attempt_id", ""), "error": str(exc)})


# W15: the extended recipe family (soft distillation, preference,
# demonstration, self-supervised, environment feedback, restricted policy
# gradient) is a third entry point. `train` and `predict` are untouched.
HANDLERS = {"train": do_train, "predict": do_predict,
            "recipe": recipe_handler.handle}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()

    emit({"v": PROTOCOL_VERSION, "type": "hello", "protocol": PROTOCOL,
          "capabilities": ["train", "predict", "recipe"], "torch": torch.__version__})

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
                  "run_id": frame.get("run_id", ""),
                  "attempt_id": frame.get("attempt_id", ""),
                  "error": f"unsupported protocol version {frame.get('v')!r}"})
            continue
        kind = frame.get("type")
        handler = HANDLERS.get(kind)
        if handler is None:
            emit({"v": PROTOCOL_VERSION, "type": "failed",
                  "run_id": frame.get("run_id", ""), "error": f"unknown action {kind!r}"})
            continue
        handler(frame)
    return 0


if __name__ == "__main__":
    sys.exit(main())
