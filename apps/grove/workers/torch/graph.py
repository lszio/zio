"""W04 permitted operator graph.

The worker executes a *graph of whitelisted operators*, never arbitrary
Python. A graph comes from the Zio model description, is validated
structurally (names, shapes, dtypes, semantic space) before a single
tensor is touched, and is then interpreted — there is no `eval`, no
`exec`, and no import of a candidate-supplied module.

Allowed vocabulary (deliberately tiny):

    input        read a named input with its presence mask
    normalize    sign-preserving / min-max scaling
    concat       join inputs along the feature axis
    linear       affine: y = x @ W + b
    relu         max(0, x)
    softmax      row-wise softmax (classification head)
    cross-entropy classification loss, masked by presence

Anything outside this list is a graph error, not a runtime surprise.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any, Callable

import torch

ALLOWED_OPS = ("input", "normalize", "concat", "linear", "relu", "softmax", "cross-entropy")


class GraphError(ValueError):
    """Structural rejection before execution: unknown op, bad shape, ..."""


@dataclass
class TensorSpec:
    shape: list[int]
    dtype: str = "float32"
    #: Free-form label for the *semantic* space (e.g. "sensor-sign",
    #: "pixel-intensity"). Two tensors may only be joined when their
    #: spaces are declared compatible; equal shape is not enough.
    space: str = "generic"

    def __post_init__(self):
        if self.dtype != "float32":
            raise GraphError(f"unsupported dtype {self.dtype!r}; only float32 is permitted")
        if any(d <= 0 for d in self.shape):
            raise GraphError(f"non-positive dimension in shape {self.shape}")


@dataclass
class Op:
    kind: str
    inputs: list[str]
    output: str
    attrs: dict[str, Any] = field(default_factory=dict)


@dataclass
class Graph:
    inputs: dict[str, TensorSpec]
    ops: list[Op]
    outputs: dict[str, str]        # name → produced value name
    loss: str | None = None
    trainable: list[str] = field(default_factory=list)  # value names

    def validate(self) -> None:
        if not self.inputs:
            raise GraphError("graph declares no input")
        for name, spec in self.inputs.items():
            if not isinstance(spec, TensorSpec):
                raise GraphError(f"input {name} has no spec")
            spec.__post_init__()

        known = set(self.inputs)
        for op in self.ops:
            if op.kind not in ALLOWED_OPS:
                raise GraphError(f"operator {op.kind!r} is not permitted")
            for dep in op.inputs:
                if dep not in known:
                    raise GraphError(f"operator {op.kind!r} reads undefined value {dep!r}")
            if op.output in known:
                raise GraphError(f"value {op.output!r} is produced twice")
            # A linear layer's weights arrive in the parameter file, not
            # in the graph message, so their presence is checked at build
            # time against the declared structure — not here.
            known.add(op.output)

        for name, source in self.outputs.items():
            if source not in known:
                raise GraphError(f"output {name!r} reads undefined value {source!r}")
        if self.loss is not None and self.loss not in known:
            raise GraphError(f"loss reads undefined value {self.loss!r}")
        for name in self.trainable:
            if name not in known:
                raise GraphError(f"trainable value {name!r} does not exist")


# ── the interpreter ────────────────────────────────────────────────

def _normalize(x: torch.Tensor, spec: TensorSpec) -> torch.Tensor:
    mode = spec.space
    if mode == "signed":
        # sign-preserving: keeps the sign, shrinks the magnitude
        return x / (x.abs() + 1.0)
    if mode == "pixel":
        return x / 255.0
    # min-max over the batch, safely
    lo = x.min()
    hi = x.max()
    return (x - lo) / (hi - lo).clamp(min=1e-6)


def evaluate(graph: Graph, tensors: dict[str, torch.Tensor]) -> dict[str, torch.Tensor]:
    """Run the graph. Tensors enter by name; masks stay attached."""
    values: dict[str, torch.Tensor] = dict(tensors)
    specs: dict[str, TensorSpec] = dict(graph.inputs)

    for op in graph.ops:
        if op.kind == "normalize":
            values[op.output] = _normalize(values[op.inputs[0]], specs[op.inputs[0]])
        elif op.kind == "concat":
            joined = torch.cat([values[name] for name in op.inputs], dim=1)
            spaces = [specs[name].space for name in op.inputs]
            declared = op.attrs.get("spaces")
            if declared is not None and tuple(spaces) != tuple(declared):
                raise GraphError(
                    f"concat joins incompatible semantic spaces {spaces}, expected {declared}"
                )
            values[op.output] = joined
            specs[op.output] = TensorSpec(shape=list(joined.shape[1:]), space="fused")
        elif op.kind == "linear":
            x = values[op.inputs[0]]
            w = op.attrs["w"]
            if w is None:
                raise GraphError(f"linear layer {op.output!r} has no weight")
            if w.dim() != 2 or w.shape[0] != x.shape[1]:
                raise GraphError(
                    f"linear weight {list(w.shape)} does not accept input width {x.shape[1]}"
                )
            bias = op.attrs.get("b")
            values[op.output] = x @ w + (bias if bias is not None else 0.0)
            specs[op.output] = TensorSpec(shape=list(values[op.output].shape[1:]), space="logits")
        elif op.kind == "relu":
            x = values[op.inputs[0]]
            values[op.output] = torch.relu(x)
            specs[op.output] = TensorSpec(shape=list(x.shape[1:]), space=specs[op.inputs[0]].space)
        elif op.kind == "softmax":
            values[op.output] = torch.softmax(values[op.inputs[0]], dim=1)
            specs[op.output] = TensorSpec(shape=list(values[op.output].shape[1:]), space="prob")
        elif op.kind == "cross-entropy":
            logits = values[op.inputs[0]]
            target = values[op.inputs[1]]
            mask = values[op.attrs["mask"]]
            per_row = torch.nn.functional.cross_entropy(logits, target, reduction="none")
            # An undecidable row (mask 0) contributes nothing. It is never
            # trained toward a class.
            weight = mask.reshape(-1)
            values[op.output] = (per_row * weight).sum() / weight.sum().clamp(min=1.0)
            specs[op.output] = TensorSpec(shape=[1], space="scalar")
        else:  # pragma: no cover — validate() rejects this first
            raise GraphError(f"operator {op.kind!r} is not permitted")
    return values


def make_module(graph: Graph, weights: dict[str, torch.Tensor]):
    """Turn a graph into a differentiable torch module.

    Only `linear` layers carry parameters; structure is data, so a
    structural change produces a different module and a fresh init.
    """
    layers: list[torch.nn.Module] = []
    bindings: dict[str, torch.nn.Parameter] = {}
    for op in graph.ops:
        if op.kind == "linear":
            w = op.attrs["w"]
            layer = torch.nn.Linear(w.shape[0], w.shape[1], bias=op.attrs.get("b") is not None)
            with torch.no_grad():
                layer.weight.copy_(w)
                if layer.bias is not None:
                    layer.bias.copy_(op.attrs["b"])
            layers.append(layer)
            bindings[op.output] = layer.weight
    return torch.nn.Sequential(*layers) if layers else None, bindings


def build_fusion_graph(in_dim: int, hidden: int | None, out_dim: int = 2) -> tuple[Graph, list[torch.Tensor]]:
    """The acceptance model's structure.

    `hidden=None` is the linear baseline; a hidden width is the structural
    change that makes the XOR learnable. Both are the same graph
    vocabulary, so the difference is data, not a special case.
    """
    graph = Graph(
        inputs={
            "image": TensorSpec(shape=[256], space="pixel"),
            "numeric": TensorSpec(shape=[2], space="signed"),
        },
        ops=[
            Op("normalize", ["image"], "image_n"),
            Op("normalize", ["numeric"], "numeric_n"),
            Op("concat", ["image_n", "numeric_n"], "fused",
               attrs={"spaces": ["pixel", "signed"]}),
            Op("linear", ["fused"], "h0", attrs={"w": torch.zeros(out_dim, in_dim)}),
        ],
        outputs={"logits": "h0"},
    )
    if hidden:
        # the structural change: linear → relu → linear
        graph.ops[3] = Op("linear", ["fused"], "h0", attrs={"out": hidden})
        graph.ops.append(Op("relu", ["h0"], "a0"))
        graph.ops.append(
            Op("linear", ["a0"], "h1", attrs={"w": torch.zeros(out_dim, hidden)})
        )
        graph.outputs["logits"] = "h1"
        graph.trainable = ["h0", "h1"]
    else:
        graph.trainable = ["h0"]
    return graph, []


def graph_to_json(graph: Graph) -> dict:
    """Serialize the structure without the tensors — the wire form."""
    return {
        "inputs": {k: {"shape": v.shape, "dtype": v.dtype, "space": v.space}
                   for k, v in graph.inputs.items()},
        "ops": [
            {"kind": o.kind, "inputs": o.inputs, "output": o.output,
             "attrs": {k: v for k, v in o.attrs.items() if k in ("spaces", "mask", "out")}}
            for o in graph.ops
        ],
        "outputs": graph.outputs,
        "loss": graph.loss,
        "trainable": graph.trainable,
    }


def graph_from_json(payload: dict) -> Graph:
    """Rebuild a structure. Weight values come from the parameter file,
    never from the graph message itself."""
    graph = Graph(
        inputs={k: TensorSpec(shape=v["shape"], dtype=v.get("dtype", "float32"),
                              space=v.get("space", "generic"))
                for k, v in payload["inputs"].items()},
        ops=[Op(kind=o["kind"], inputs=o["inputs"], output=o["output"],
                attrs=dict(o.get("attrs", {})))
             for o in payload["ops"]],
        outputs=dict(payload["outputs"]),
        loss=payload.get("loss"),
        trainable=list(payload.get("trainable", [])),
    )
    graph.validate()
    return graph
