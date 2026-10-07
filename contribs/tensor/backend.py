#!/usr/bin/env python3
"""Bounded CPU tensor kernels. Application policy lives in the caller."""
from __future__ import annotations

import argparse
import json
import math
import sys

import torch
import torch.nn.functional as F

DTYPES = {"float32": torch.float32, "float64": torch.float64,
          "int64": torch.int64, "bool": torch.bool}
SCHEMA = "zio.tensor.state/1"


class TensorError(ValueError):
    def __init__(self, message, kind="invalid-input"):
        super().__init__(message)
        self.kind = kind


def require(condition, message, kind="invalid-input"):
    if not condition:
        raise TensorError(message, kind)


def integer(value, minimum=None):
    require(type(value) is int and (minimum is None or value >= minimum), "invalid integer")
    return value


def number(value):
    require(type(value) in (int, float) and math.isfinite(value), "expected finite number")
    return value


def boolean(value):
    require(type(value) is bool, "expected boolean")
    return value


def name(value):
    require(type(value) is str and 0 < len(value) <= 256, "invalid object name")
    return value


def fields(value, required, optional=()):
    require(type(value) is dict and set(required) <= set(value) <= set(required) | set(optional),
            "missing or unknown object fields")


def finite(tensor):
    require(bool(torch.isfinite(tensor).all()), "nonfinite tensor", "nonfinite")
    return tensor


def descriptor(tensor, data=False):
    result = {"shape": list(tensor.shape), "dtype": str(tensor.dtype).removeprefix("torch."),
              "requires_grad": tensor.requires_grad}
    if data:
        result["data"] = tensor.detach().reshape(-1).tolist()
    return result


def packed(tensor):
    value = descriptor(tensor, True)
    del value["requires_grad"]
    return value


class Backend:
    def __init__(self, max_elements=8388608, max_tensors=4096):
        self.max_elements = integer(max_elements, 1)
        self.max_tensors = integer(max_tensors, 1)
        self.tensors = {}
        self.optimizers = {}
        self.rng = torch.Generator(device="cpu").manual_seed(0)

    def shape(self, shape):
        require(type(shape) is list and len(shape) <= 32, "invalid shape")
        for dim in shape:
            integer(dim, 0)
        require(math.prod(shape) <= self.max_elements, "tensor element cap exceeded", "resource-limit")
        return shape

    def allocation(self, tensors=None, optimizers=None):
        tensors = self.tensors if tensors is None else tensors
        optimizers = self.optimizers if optimizers is None else optimizers
        total = sum(t.numel() + (t.grad.numel() if t.is_leaf and t.grad is not None else 0)
                    for t in tensors.values())
        total += sum(v.numel() for opt in optimizers.values() for state in opt["state"].values()
                     for v in state.values() if isinstance(v, torch.Tensor))
        require(len(tensors) + len(optimizers) <= self.max_tensors and total <= self.max_elements,
                "tensor storage cap exceeded", "resource-limit")

    def tensor(self, ref):
        name(ref)
        require(ref in self.tensors, f"unknown tensor {ref}")
        return self.tensors[ref]

    def new_name(self, ref):
        name(ref)
        require(ref not in self.tensors and ref not in self.optimizers, f"object already exists: {ref}")
        require(len(self.tensors) + len(self.optimizers) < self.max_tensors,
                "object count cap exceeded", "resource-limit")
        return ref

    def put(self, ref, tensor):
        self.new_name(ref)
        finite(tensor)
        candidate = dict(self.tensors)
        candidate[ref] = tensor
        self.allocation(candidate)
        self.tensors[ref] = tensor
        return {"tensor": ref, **descriptor(tensor)}

    def decode(self, data, shape, dtype, requires_grad=False):
        self.shape(shape)
        require(dtype in DTYPES, "unsupported dtype")
        boolean(requires_grad)
        require(not requires_grad or dtype in ("float32", "float64"), "only floats may require gradients")
        require(type(data) is list and len(data) == math.prod(shape), "data/shape mismatch", "shape-error")
        if dtype == "bool":
            for item in data:
                boolean(item)
        elif dtype == "int64":
            for item in data:
                integer(item)
                require(-(1 << 63) <= item < (1 << 63), "int64 overflow")
        else:
            for item in data:
                number(item)
        return finite(torch.tensor(data, dtype=DTYPES[dtype]).reshape(shape).requires_grad_(requires_grad))

    def unpack(self, value, grad=False):
        fields(value, ("data", "shape", "dtype"))
        return self.decode(value["data"], value["shape"], value["dtype"], grad)

    def optimizer(self, ref):
        name(ref)
        require(ref in self.optimizers, "unknown optimizer")
        return self.optimizers[ref]

    def adam_config(self, value):
        parameters = value["parameters"]
        require(type(parameters) is list and parameters and len(set(parameters)) == len(parameters),
                "optimizer requires distinct parameters")
        for ref in parameters:
            tensor = self.tensor(ref)
            require(tensor.is_leaf and tensor.is_floating_point(), "optimizer parameters must be float leaves")
        lr = number(value["lr"])
        betas = value.get("betas", [.9, .999])
        require(type(betas) is list and len(betas) == 2 and all(0 <= number(v) < 1 for v in betas),
                "invalid Adam betas")
        eps = number(value.get("eps", 1e-8))
        decay = number(value.get("weight_decay", 0))
        require(lr > 0 and eps > 0 and decay >= 0, "invalid Adam hyperparameters")
        return {"parameters": list(parameters), "lr": lr, "betas": list(betas), "eps": eps,
                "weight_decay": decay, "amsgrad": boolean(value.get("amsgrad", False)), "state": {}}

    def adam_step(self, opt):
        beta1, beta2 = opt["betas"]
        updates = {}
        states = dict(opt["state"])
        with torch.no_grad():
            for ref in opt["parameters"]:
                parameter = self.tensor(ref)
                if not parameter.requires_grad or parameter.grad is None:
                    continue
                gradient = finite(parameter.grad)
                require(gradient.shape == parameter.shape and gradient.dtype == parameter.dtype,
                        "gradient shape/dtype mismatch", "shape-error")
                if opt["weight_decay"]:
                    gradient = gradient + opt["weight_decay"] * parameter
                prior = states.get(ref)
                step = prior["step"] + 1 if prior else 1
                average = prior["exp_avg"] if prior else torch.zeros_like(parameter)
                square = prior["exp_avg_sq"] if prior else torch.zeros_like(parameter)
                average = finite(beta1 * average + (1 - beta1) * gradient)
                square = finite(beta2 * square + (1 - beta2) * gradient.square())
                state = {"step": step, "exp_avg": average, "exp_avg_sq": square}
                denominator = square
                if opt["amsgrad"]:
                    maximum = torch.maximum(prior["max_exp_avg_sq"] if prior else torch.zeros_like(parameter), square)
                    state["max_exp_avg_sq"] = maximum
                    denominator = maximum
                denominator = denominator.sqrt() / math.sqrt(1 - beta2 ** step) + opt["eps"]
                updates[ref] = finite(parameter - (opt["lr"] / (1 - beta1 ** step)) * average / denominator)
                states[ref] = state
            candidates = dict(self.optimizers)
            candidates[next(k for k, v in self.optimizers.items() if v is opt)] = {**opt, "state": states}
            self.allocation(optimizers=candidates)
            for ref, update in updates.items():
                self.tensors[ref].copy_(update)
            opt["state"] = states
        return {"updated": list(updates)}

    def export_state(self):
        tensors = {}
        for ref, tensor in self.tensors.items():
            if tensor.is_leaf:
                finite(tensor)
                tensors[ref] = {**descriptor(tensor, True),
                                "grad": packed(finite(tensor.grad)) if tensor.grad is not None else None}
        optimizers = {}
        for ref, opt in self.optimizers.items():
            optimizers[ref] = {**opt, "state": {key: {field: packed(value) if isinstance(value, torch.Tensor) else value
                                                      for field, value in state.items()}
                                                for key, state in opt["state"].items()}}
        return {"schema": SCHEMA, "tensors": tensors, "optimizers": optimizers,
                "rng": self.rng.get_state().tolist()}

    def import_state(self, state, strict):
        fields(state, ("schema", "tensors", "optimizers", "rng"))
        require(state["schema"] == SCHEMA, "unsupported tensor state schema")
        require(type(state["tensors"]) is dict and type(state["optimizers"]) is dict, "invalid state tables")
        require(len(state["tensors"]) + len(state["optimizers"]) <= self.max_tensors,
                "state object cap exceeded", "resource-limit")
        candidate = Backend(self.max_elements, self.max_tensors)
        for ref, entry in state["tensors"].items():
            fields(entry, ("shape", "dtype", "data", "requires_grad", "grad"))
            tensor = candidate.decode(entry["data"], entry["shape"], entry["dtype"], entry["requires_grad"])
            candidate.put(ref, tensor)
            if entry["grad"] is not None:
                require(tensor.requires_grad, "frozen tensor cannot carry gradient")
                gradient = candidate.unpack(entry["grad"])
                require(gradient.shape == tensor.shape and gradient.dtype == tensor.dtype,
                        "saved gradient mismatch", "shape-error")
                tensor.grad = gradient
        for ref, saved in state["optimizers"].items():
            candidate.new_name(ref)
            fields(saved, ("parameters", "lr", "betas", "eps", "weight_decay", "amsgrad", "state"))
            opt = candidate.adam_config(saved)
            require(type(saved["state"]) is dict and set(saved["state"]) <= set(opt["parameters"]),
                    "foreign optimizer state")
            require(not any(set(opt["parameters"]) & set(existing["parameters"])
                            for existing in candidate.optimizers.values()), "duplicate parameter ownership")
            for param, saved_state in saved["state"].items():
                expected = ["step", "exp_avg", "exp_avg_sq"] + (["max_exp_avg_sq"] if opt["amsgrad"] else [])
                fields(saved_state, expected)
                current = {"step": integer(saved_state["step"], 1)}
                require(current["step"] <= (1 << 53), "optimizer step overflow")
                tensor = candidate.tensor(param)
                for field in expected[1:]:
                    value = candidate.unpack(saved_state[field])
                    require(value.shape == tensor.shape and value.dtype == tensor.dtype,
                            "optimizer state shape/dtype mismatch", "shape-error")
                    if field != "exp_avg":
                        require(bool((value >= 0).all()), "negative Adam second moment")
                    current[field] = value
                if opt["amsgrad"]:
                    require(bool((current["max_exp_avg_sq"] >= current["exp_avg_sq"]).all()),
                            "AMSGrad maximum smaller than second moment")
                opt["state"][param] = current
            candidate.optimizers[ref] = opt
            candidate.allocation()
        rng = state["rng"]
        require(type(rng) is list and len(rng) == len(self.rng.get_state()), "invalid CPU RNG state")
        require(all(type(byte) is int and 0 <= byte <= 255 for byte in rng), "invalid RNG bytes")
        candidate.rng.set_state(torch.tensor(rng, dtype=torch.uint8))
        candidate.allocation()
        if strict and (any(t.is_leaf for t in self.tensors.values()) or self.optimizers):
            leaves = {ref: descriptor(tensor) for ref, tensor in self.tensors.items() if tensor.is_leaf}
            require(leaves == {ref: descriptor(tensor) for ref, tensor in candidate.tensors.items()},
                    "state leaf names/shape/dtype/trainable mismatch")
            metadata = lambda opts: {ref: {key: val for key, val in opt.items() if key != "state"}
                                     for ref, opt in opts.items()}
            require(metadata(self.optimizers) == metadata(candidate.optimizers), "state optimizer contract mismatch")
        self.tensors, self.optimizers, self.rng = candidate.tensors, candidate.optimizers, candidate.rng
        return {"tensors": list(self.tensors), "optimizers": list(self.optimizers)}

    def call(self, request):
        try:
            return self.dispatch(request)
        except TensorError:
            raise
        except (RuntimeError, ValueError, TypeError, KeyError, IndexError, OverflowError) as error:
            raise TensorError(str(error), "shape-error") from error

    def dispatch(self, r):
        require(type(r) is dict and type(r.get("op")) is str, "operation must be object with op")
        op = r["op"]
        check = lambda required=(), optional=(): fields(r, ("op", *required), optional)
        if op == "create":
            check(("out", "data"), ("shape", "dtype", "requires_grad"))
            self.new_name(r["out"])
            return self.put(r["out"], self.decode(r["data"], r.get("shape", [len(r["data"])]),
                                                   r.get("dtype", "float32"), r.get("requires_grad", False)))
        if op == "full":
            check(("out", "shape", "value"), ("dtype", "requires_grad"))
            self.new_name(r["out"])
            shape = self.shape(r["shape"])
            return self.put(r["out"], self.decode([r["value"]] * math.prod(shape), shape,
                                                   r.get("dtype", "float32"), r.get("requires_grad", False)))
        if op == "seed":
            check(("seed",))
            seed = integer(r["seed"], 0)
            require(seed < (1 << 63), "seed overflow")
            self.rng.manual_seed(seed)
            return None
        if op in ("rand", "randn", "randperm", "multinomial"):
            required, optional = (("out", "n"), ()) if op == "randperm" else (
                (("out", "input", "num_samples"), ("replacement",)) if op == "multinomial"
                else (("out", "shape"), ("dtype", "requires_grad")))
            check(required, optional)
            self.new_name(r["out"])
            previous = self.rng.get_state()
            try:
                if op == "randperm":
                    n = integer(r["n"], 0)
                    self.shape([n])
                    tensor = torch.randperm(n, generator=self.rng)
                elif op == "multinomial":
                    weights = self.tensor(r["input"])
                    samples = integer(r["num_samples"], 1)
                    self.shape([*weights.shape[:-1], samples])
                    tensor = torch.multinomial(weights, samples, boolean(r.get("replacement", False)), generator=self.rng)
                else:
                    shape = self.shape(r["shape"])
                    dtype = r.get("dtype", "float32")
                    require(dtype in ("float32", "float64"), "sampling requires float dtype")
                    tensor = getattr(torch, op)(shape, dtype=DTYPES[dtype], generator=self.rng)
                    tensor.requires_grad_(boolean(r.get("requires_grad", False)))
                return self.put(r["out"], tensor)
            except Exception:
                self.rng.set_state(previous)
                raise
        if op in ("value", "info"):
            check(("input",))
            tensor = finite(self.tensor(r["input"]))
            return descriptor(tensor, True) if op == "value" else {"tensor": r["input"], **descriptor(tensor)}
        if op in ("drop", "zero-grad"):
            check(("inputs",))
            require(type(r["inputs"]) is list, "inputs must be array")
            tensors = [self.tensor(ref) for ref in r["inputs"]]
            if op == "drop":
                require(not any(ref in opt["parameters"] for ref in r["inputs"] for opt in self.optimizers.values()),
                        "cannot drop optimizer parameter")
                for ref in set(r["inputs"]):
                    del self.tensors[ref]
            else:
                require(all(t.is_leaf for t in tensors), "zero-grad requires leaves")
                for tensor in tensors:
                    tensor.grad = None
            return None
        if op == "freeze":
            check(("input", "frozen"))
            tensor = self.tensor(r["input"])
            require(tensor.is_leaf and tensor.is_floating_point(), "freeze requires float leaf")
            tensor.requires_grad_(not boolean(r["frozen"]))
            if r["frozen"]:
                tensor.grad = None
            return None
        if op == "assign":
            check(("input", "data"))
            tensor = self.tensor(r["input"])
            require(tensor.is_leaf, "assign requires leaf")
            new = self.decode(r["data"], list(tensor.shape), descriptor(tensor)["dtype"])
            with torch.no_grad():
                tensor.copy_(new)
            tensor.grad = None
            return None
        if op == "backward":
            check(("input",), ("gradient", "retain_graph"))
            tensor = self.tensor(r["input"])
            gradient = self.tensor(r["gradient"]) if "gradient" in r else None
            if gradient is not None:
                require(gradient.shape == tensor.shape and gradient.dtype == tensor.dtype,
                        "backward gradient mismatch", "shape-error")
            leaves = [t for t in self.tensors.values() if t.is_leaf and t.requires_grad]
            previous = [(t, t.grad.clone() if t.grad is not None else None) for t in leaves]
            try:
                tensor.backward(gradient, retain_graph=boolean(r.get("retain_graph", False)))
                for t in leaves:
                    if t.grad is not None:
                        finite(t.grad)
                self.allocation()
            except Exception:
                for t, grad in previous:
                    t.grad = grad
                raise
            return None
        if op == "grad":
            check(("input", "out"))
            tensor = self.tensor(r["input"])
            require(tensor.is_leaf, "grad requires leaf")
            return self.put(r["out"], tensor.grad.detach().clone()) if tensor.grad is not None else None
        if op == "adam-create":
            check(("name", "parameters", "lr"), ("betas", "eps", "weight_decay", "amsgrad"))
            self.new_name(r["name"])
            opt = self.adam_config(r)
            require(not any(set(opt["parameters"]) & set(other["parameters"]) for other in self.optimizers.values()),
                    "tensor already belongs to optimizer")
            self.optimizers[r["name"]] = opt
            return {"optimizer": r["name"]}
        if op in ("adam-step", "adam-drop"):
            check(("optimizer",))
            opt = self.optimizer(r["optimizer"])
            if op == "adam-step":
                return self.adam_step(opt)
            del self.optimizers[r["optimizer"]]
            return None
        if op == "state-export":
            check()
            return self.export_state()
        if op == "state-import":
            check(("state",), ("strict",))
            return self.import_state(r["state"], boolean(r.get("strict", True)))
        return self.compute(r)

    def compute(self, r):
        op = r["op"]
        binary = {"add": torch.add, "sub": torch.sub, "mul": torch.mul, "div": torch.div,
                  "pow": torch.pow, "maximum": torch.maximum, "minimum": torch.minimum,
                  "eq": torch.eq, "ne": torch.ne, "lt": torch.lt, "le": torch.le, "gt": torch.gt, "ge": torch.ge,
                  "matmul": torch.matmul}
        unary = {"neg": torch.neg, "abs": torch.abs, "exp": torch.exp, "log": torch.log,
                 "sqrt": torch.sqrt, "relu": torch.relu, "sigmoid": torch.sigmoid, "tanh": torch.tanh,
                 "softplus": F.softplus, "logsigmoid": F.logsigmoid,
                 "detach": lambda t: t.detach().clone(), "clone": torch.clone}
        options = {
            "linear": (), "softmax": ("dim",), "log-softmax": ("dim",), "normalize": ("dim", "eps"),
            "sum": ("dim", "keepdim"), "mean": ("dim", "keepdim"), "min": ("dim", "keepdim"),
            "max": ("dim", "keepdim"), "argmax": ("dim", "keepdim"), "clamp": ("min", "max"),
            "reshape": ("shape",), "transpose": ("dim0", "dim1"), "unsqueeze": ("dim",), "squeeze": ("dim",),
            "cast": ("dtype",), "cat": ("dim",), "stack": ("dim",), "index-select": ("dim",), "gather": ("dim",),
            "slice": ("dim", "start", "end", "step"), "where": (),
            "cross-entropy": ("reduction", "label_smoothing"), "kl-div": ("reduction", "log_target"),
            "cosine-similarity": ("dim", "eps")}
        require(op in binary or op in unary or op in options, f"unknown tensor operation {op}")
        fields(r, ("op", "out", "inputs"), options.get(op, ()))
        self.new_name(r["out"])
        require(type(r["inputs"]) is list and r["inputs"], "kernel inputs required")
        inputs = [self.tensor(ref) for ref in r["inputs"]]
        if op not in ("cat", "stack"):
            expected = 2 if op in binary or op in ("index-select", "gather", "cross-entropy", "kl-div", "cosine-similarity") else 3 if op == "where" else 1
            require(len(inputs) in (2, 3) if op == "linear" else len(inputs) == expected, "wrong kernel input count")
        for t in inputs:
            finite(t)
        dim = integer(r.get("dim", -1)) if "dim" in options.get(op, ()) and type(r.get("dim", -1)) is not list else r.get("dim")
        keepdim = boolean(r.get("keepdim", False))
        x = inputs[0]
        if op in binary:
            if op != "matmul":
                self.shape(list(torch.broadcast_shapes(*(t.shape for t in inputs))))
            else:
                a, b = inputs
                require(a.ndim > 0 and b.ndim > 0, "matmul requires rank >= 1", "shape-error")
                batch = torch.broadcast_shapes(a.shape[:-2] if a.ndim > 1 else (), b.shape[:-2] if b.ndim > 1 else ())
                self.shape([*batch, *([a.shape[-2]] if a.ndim > 1 else []), *([b.shape[-1]] if b.ndim > 1 else [])])
            result = binary[op](*inputs)
        elif op in unary:
            result = unary[op](x)
        elif op == "linear":
            weight = inputs[1]
            require(x.ndim >= 1 and weight.ndim == 2 and x.shape[-1] == weight.shape[1], "linear shape mismatch", "shape-error")
            require(all(t.dtype == x.dtype and t.is_floating_point() for t in inputs), "linear dtype mismatch")
            if len(inputs) == 3:
                require(inputs[2].shape == (weight.shape[0],), "linear bias mismatch", "shape-error")
            self.shape([*x.shape[:-1], weight.shape[0]])
            result = F.linear(*inputs)
        elif op in ("softmax", "log-softmax", "normalize"):
            require(type(dim) is int, "dimension must be integer")
            if op == "normalize":
                eps = number(r.get("eps", 1e-12))
                require(eps > 0, "epsilon must be positive")
                result = F.normalize(x, dim=dim, eps=eps)
            else:
                result = getattr(torch, op.replace("-", "_"))(x, dim=dim)
        elif op in ("sum", "mean", "min", "max", "argmax"):
            if "dim" in r:
                if type(dim) is list:
                    require(op in ("sum", "mean"), "multi-axis reduction unsupported")
                    dim = tuple(integer(axis) for axis in dim)
                result = getattr(torch, op)(x, dim=dim, keepdim=keepdim)
                if op in ("min", "max"):
                    result = result.values
            else:
                result = getattr(torch, op)(x)
                if keepdim:
                    result = result.reshape([1] * x.ndim)
        elif op == "clamp":
            require("min" in r or "max" in r, "clamp bound required")
            result = torch.clamp(x, **{key: number(r[key]) for key in ("min", "max") if key in r})
        elif op == "reshape":
            shape = r["shape"]
            require(type(shape) is list and len(shape) <= 32 and shape.count(-1) <= 1, "invalid reshape")
            for axis in shape:
                integer(axis, -1)
            result = x.reshape(shape)
        elif op == "transpose":
            result = x.transpose(integer(r["dim0"]), integer(r["dim1"]))
        elif op == "unsqueeze":
            result = x.unsqueeze(integer(r["dim"]))
        elif op == "squeeze":
            result = x.squeeze(dim) if "dim" in r else x.squeeze()
        elif op == "cast":
            require(r["dtype"] in DTYPES, "unsupported cast dtype")
            if r["dtype"] == "int64" and x.is_floating_point():
                require(bool(((x >= -(1 << 63)) & (x < (1 << 63))).all()), "int64 cast overflow")
            result = x.to(DTYPES[r["dtype"]])
        elif op in ("cat", "stack"):
            require(type(dim) is int, "dimension must be integer")
            require(sum(t.numel() for t in inputs) <= self.max_elements, "join element cap exceeded", "resource-limit")
            result = getattr(torch, op)(inputs, dim=r.get("dim", 0))
        elif op in ("index-select", "gather"):
            require(inputs[1].dtype == torch.int64, "index dtype must be int64")
            axis = integer(r.get("dim", 0 if op == "index-select" else -1))
            require(x.ndim > 0 and -x.ndim <= axis < x.ndim, "index dimension invalid")
            axis %= x.ndim
            self.shape(list(inputs[1].shape) if op == "gather" else [*x.shape[:axis], inputs[1].numel(), *x.shape[axis + 1:]])
            result = torch.index_select(x, axis, inputs[1]) if op == "index-select" else torch.gather(x, axis, inputs[1])
        elif op == "slice":
            require(x.ndim > 0 and type(dim) is int and -x.ndim <= r.get("dim", 0) < x.ndim, "slice dimension invalid")
            selection = [slice(None)] * x.ndim
            selection[r.get("dim", 0)] = slice(integer(r.get("start", 0)), integer(r["end"]) if "end" in r else None, integer(r.get("step", 1), 1))
            result = x[tuple(selection)]
        elif op == "where":
            require(x.dtype == torch.bool, "where requires bool condition")
            self.shape(list(torch.broadcast_shapes(*(t.shape for t in inputs))))
            result = torch.where(*inputs)
        elif op in ("cross-entropy", "kl-div"):
            reduction = r.get("reduction", "none")
            require(reduction in (("none", "mean", "sum") if op == "cross-entropy" else ("none", "mean", "sum", "batchmean")), "invalid reduction")
            if op == "cross-entropy":
                target = inputs[1]
                require(target.dtype == torch.int64 or (target.is_floating_point() and target.shape == x.shape), "cross entropy target mismatch")
                smoothing = number(r.get("label_smoothing", 0))
                require(0 <= smoothing <= 1, "invalid label smoothing")
                if target.is_floating_point():
                    require(bool((target >= 0).all()) and torch.allclose(target.sum(dim=1 if target.ndim > 1 else 0), torch.ones_like(target.sum(dim=1 if target.ndim > 1 else 0)), atol=1e-5), "invalid probability target")
                result = F.cross_entropy(x, target, reduction=reduction, label_smoothing=smoothing)
            else:
                require(x.shape == inputs[1].shape and x.dtype == inputs[1].dtype, "KL shape/dtype mismatch", "shape-error")
                log_target = boolean(r.get("log_target", False))
                if not log_target:
                    require(bool((inputs[1] >= 0).all()), "negative KL target")
                result = F.kl_div(x, inputs[1], reduction=reduction, log_target=log_target)
        elif op == "cosine-similarity":
            eps = number(r.get("eps", 1e-8))
            require(eps > 0, "epsilon must be positive")
            self.shape(list(torch.broadcast_shapes(*(t.shape for t in inputs))))
            result = F.cosine_similarity(*inputs, dim=dim, eps=eps)
        if op in ("reshape", "transpose", "unsqueeze", "squeeze", "slice", "cast"):
            # Named outputs own storage; an assign cannot mutate a differently named frozen leaf.
            result = result.clone()
        return self.put(r["out"], result)


def reject_duplicate_keys(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "duplicate JSON key")
        result[key] = value
    return result


def serve(backend, input_stream, output_stream, max_frame_bytes):
    while True:
        line = input_stream.readline(max_frame_bytes + 1)
        if not line:
            return 0
        if len(line) > max_frame_bytes or not line.endswith(b"\n"):
            return 2
        request_id = None
        try:
            frame = json.loads(line, object_pairs_hook=reject_duplicate_keys,
                               parse_constant=lambda value: (_ for _ in ()).throw(TensorError("nonfinite JSON constant")))
            fields(frame, ("id", "operation"))
            request_id = integer(frame["id"], 1)
            require(request_id <= (1 << 53), "request id overflow")
            result = backend.call(frame["operation"])
            reply = {"id": request_id, "ok": True, "result": result}
        except (TensorError, json.JSONDecodeError, UnicodeDecodeError, RecursionError) as error:
            reply = {"id": request_id, "ok": False,
                     "error": {"class": getattr(error, "kind", "invalid-input"), "message": str(error)[:2000]}}
        encoded = json.dumps(reply, separators=(",", ":"), allow_nan=False).encode("utf-8") + b"\n"
        if len(encoded) > max_frame_bytes:
            encoded = json.dumps({"id": request_id, "ok": False, "error": {
                "class": "resource-limit", "message": "response frame cap exceeded"}}, separators=(",", ":")).encode() + b"\n"
        output_stream.write(encoded)
        output_stream.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--max-frame-bytes", type=int, default=1048576)
    parser.add_argument("--max-elements", type=int, default=8388608)
    parser.add_argument("--max-tensors", type=int, default=4096)
    args = parser.parse_args()
    require(args.max_frame_bytes >= 256, "frame cap must be at least 256")
    torch.set_num_threads(1)
    torch.use_deterministic_algorithms(True)
    return serve(Backend(args.max_elements, args.max_tensors), sys.stdin.buffer, sys.stdout.buffer, args.max_frame_bytes)


if __name__ == "__main__":
    sys.exit(main())
