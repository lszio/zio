#!/usr/bin/env python3
"""W00 — frozen acceptance task for the grove self-learning work packages.

Generates the "geometry image + numeric sensor" classification task:

  * 16x16 grayscale image holding a horizontal or vertical bar
    (brightness / position / pixel noise are the perturbation axes),
  * a structured numeric block with two noisy positive/negative readings,
  * label = figure_class XOR reading_sign.

The XOR is the point: a *linear* fusion of the two modalities cannot
represent it, so a candidate that only tunes weights stays stuck and the
structural change (a nonlinear fusion term) is what moves the metric.

Splits are grouped by `scene_id`: every derived variant of a base scene
stays inside the split that owns the scene, so train/val/holdout can never
share a near-duplicate.

Holdout labels are written outside the data directory
(`eval/holdout_labels.json`) and the holdout image container stores no
label byte at all, so the training path physically cannot read the
acceptance answers.

    python examples/self-learning/generate.py                 # write data + manifest
    python examples/self-learning/generate.py --self-check    # verify invariants
"""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import random
import struct
import sys
from pathlib import Path

# ── task constants (mirrored in task.json; keep the two in sync) ──

SIDE = 16
BAR_LEN = 10
BAR_THICK = 3
MIN_MAGNITUDE = 1.0          # |reading| >= MIN_MAGNITUDE keeps the sign unambiguous
NOISE_FRACTION = 0.4         # |noise| <= NOISE_FRACTION * magnitude
VARIANTS_PER_SCENE = 3       # canonical, perturbed, perturbed+missing-modality
CLASSES = ("clear", "fault")
LABEL_NONE = 0xFF            # holdout rows carry no label byte

FLAG_PERTURBED = 0b0000_0001
FLAG_MISSING = 0b0000_0010

# scene_id, image_present, numeric_present, label, flags, reading1, reading2
SAMPLE = struct.Struct("<IBBBBff")
assert SAMPLE.size == 16
PIXELS = SIDE * SIDE
ROW_BYTES = SAMPLE.size + PIXELS

SPLITS = (("train", 512), ("val", 128), ("holdout", 256))


# ── scene synthesis ─────────────────────────────────────────────────

def make_readings(rng: random.Random, sign: int) -> tuple[float, float]:
    """Two noisy readings; the sign survives the noise by construction."""
    out = []
    for _ in range(2):
        magnitude = MIN_MAGNITUDE * (1.0 + rng.random() * 2.0)
        noise = (rng.random() * 2.0 - 1.0) * magnitude * NOISE_FRACTION
        out.append(sign * (magnitude + noise))
    return out[0], out[1]


def make_pixels(rng: random.Random, figure: int, brightness: float,
                row0: int, col0: int, noise: float) -> bytes:
    """Bar on a dark background; `figure` 0 = horizontal, 1 = vertical."""
    grid = [0.0] * PIXELS
    for t in range(BAR_THICK):
        for k in range(BAR_LEN):
            r = row0 + t
            c = col0 + k
            if figure == 0:      # horizontal bar spans columns
                r, c = row0, col0 + k
            else:                # vertical bar spans rows
                r, c = row0 + k, col0
            if 0 <= r < SIDE and 0 <= c < SIDE:
                grid[r * SIDE + c] = brightness
    return bytes(
        max(0, min(255, int(round(v * 255.0 + (rng.random() * 2.0 - 1.0) * noise * 255.0))))
        for v in grid
    )


def build_scene(rng: random.Random, scene_id: int) -> dict:
    """One base scene plus its three derived variants."""
    figure = rng.randint(0, 1)
    sign = rng.randint(0, 1)
    brightness = 0.6 + rng.random() * 0.4
    row0 = rng.randint(0, SIDE - BAR_THICK)
    col0 = rng.randint(0, SIDE - BAR_LEN)
    noise = 0.02 + rng.random() * 0.08
    label = figure ^ sign
    missing = 0 if scene_id % 2 == 0 else 1   # 0 = drop image, 1 = drop numerics
    samples = []

    for variant in range(VARIANTS_PER_SCENE):
        vrng = random.Random(f"variant:{scene_id}:{variant}")
        perturbed = variant > 0
        b = min(1.0, brightness + vrng.random() * 0.2 - 0.1) if perturbed else brightness
        n = min(0.15, noise + vrng.random() * 0.05) if perturbed else noise
        r = min(SIDE - BAR_THICK, max(0, row0 + vrng.randint(-2, 2))) if perturbed else row0
        c = min(SIDE - BAR_LEN, max(0, col0 + vrng.randint(-2, 2))) if perturbed else col0
        image_present = 1
        numeric_present = 1
        if variant == VARIANTS_PER_SCENE - 1:
            if missing == 0:
                image_present = 0
            else:
                numeric_present = 0
        pixels = make_pixels(vrng, figure, b, r, c, n) if image_present else bytes(PIXELS)
        r1, r2 = make_readings(vrng, sign) if numeric_present else (0.0, 0.0)
        flags = (FLAG_PERTURBED if perturbed else 0) | (FLAG_MISSING if not (image_present and numeric_present) else 0)
        samples.append({
            "scene_id": scene_id,
            "image_present": image_present,
            "numeric_present": numeric_present,
            "label": label,
            "flags": flags,
            "readings": (r1, r2),
            "pixels": pixels,
        })
    return {"scene_id": scene_id, "figure": figure, "sign": sign, "label": label,
            "missing": missing, "samples": samples}


# ── containers ──────────────────────────────────────────────────────

def encode_split(name: str, seed: int, scenes: int) -> tuple[bytes, list[dict], set[int]]:
    header = {
        "magic": "GVD1",
        "schema": 1,
        "generator": "generate.py/1.0.0",
        "split": name,
        "seed": seed,
        "scenes": scenes,
        "variants_per_scene": VARIANTS_PER_SCENE,
        "side": SIDE,
        "record_bytes": ROW_BYTES,
        "fields": ["scene_id", "image_present", "numeric_present", "label",
                   "flags", "reading1", "reading2", "pixels[256]"],
        "labels_present": name != "holdout",
    }
    blob = json.dumps(header, sort_keys=True, separators=(",", ":")).encode("utf-8")
    out = bytearray(b"GVD1" + struct.pack("<I", len(blob)) + blob)
    rows: list[dict] = []
    group_ids: set[int] = set()
    for scene_id in scenes_of(name, seed):
        scene = build_scene(random.Random(f"{seed}:{name}:{scene_id}"), scene_id)
        group_ids.add(scene_id)
        for sample in scene["samples"]:
            r1, r2 = sample["readings"]
            # the holdout container stores no answer byte; the oracle in
            # eval/holdout_labels.json is the only place it exists
            packed_label = LABEL_NONE if name == "holdout" else sample["label"]
            out += SAMPLE.pack(sample["scene_id"], sample["image_present"],
                               sample["numeric_present"], packed_label,
                               sample["flags"], r1, r2)
            out += sample["pixels"]
            rows.append({
                "scene_id": sample["scene_id"],
                "index": len(rows),
                "image_present": sample["image_present"],
                "numeric_present": sample["numeric_present"],
                "label": sample["label"],
            })
    return bytes(out), rows, group_ids


def scenes_of(name: str, seed: int) -> range:
    """Scene ids are namespaced per split so group sets cannot collide."""
    offset = {"train": 0, "val": 1_000_000, "holdout": 2_000_000}[name]
    count = dict(SPLITS)[name]
    return range(offset, offset + count)


def read_rows(blob: bytes) -> list[dict]:
    magic, hlen = struct.unpack_from("<4sI", blob, 0)
    assert magic == b"GVD1", "bad magic"
    head = 8 + hlen
    header = json.loads(blob[8:head].decode("utf-8"))
    stride = header["record_bytes"]
    rows = []
    pos = head
    index = 0
    while pos < len(blob):
        (scene_id, image_present, numeric_present, label, flags, r1, r2) = SAMPLE.unpack_from(blob, pos)
        rows.append({"scene_id": scene_id, "index": index, "image_present": image_present,
                     "numeric_present": numeric_present, "label": label, "flags": flags,
                     "readings": [r1, r2]})
        index += 1
        pos += stride
    return rows


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def decidable(row: dict) -> bool:
    """Contract: the label is determined only when both modalities exist."""
    return bool(row["image_present"] and row["numeric_present"])


# ── generation ──────────────────────────────────────────────────────

def generate(out_dir: Path, seed: int) -> dict:
    data_dir = out_dir / "data"
    eval_dir = out_dir / "eval"
    data_dir.mkdir(parents=True, exist_ok=True)
    eval_dir.mkdir(parents=True, exist_ok=True)

    manifest_splits = {}
    all_rows: dict[str, list[dict]] = {}
    for name, count in SPLITS:
        blob, rows, group_ids = encode_split(name, seed, count)
        (data_dir / f"{name}.bin").write_bytes(blob)
        all_rows[name] = rows
        manifest_splits[name] = {
            "file": f"data/{name}.bin",
            "sha256": sha256(blob),
            "bytes": len(blob),
            "scenes": len(group_ids),
            "samples": len(rows),
            "decidable": sum(1 for r in rows if decidable(r)),
            "labels_in_file": name != "holdout",
        }

    labels = {
        "magic": "GVL1",
        "schema": 1,
        "split": "holdout",
        "note": "acceptance oracle — never read by the training path",
        "expected": [
            {"index": r["index"], "scene_id": r["scene_id"],
             "answer": r["label"] if decidable(r) else None}
            for r in all_rows["holdout"]
        ],
    }
    labels_blob = json.dumps(labels, sort_keys=True, separators=(",", ":")).encode("utf-8")
    (eval_dir / "holdout_labels.json").write_bytes(labels_blob)

    manifest = {
        "schema": 1,
        "generator": "generate.py/1.0.0",
        "seed": seed,
        "task": "geometry-sensor-xor@1.0.0",
        "environment": {
            "python": platform.python_version(),
            "implementation": platform.python_implementation(),
            "numpy_used": False,
        },
        "splits": manifest_splits,
        "labels": {
            "file": "eval/holdout_labels.json",
            "sha256": sha256(labels_blob),
        },
    }
    manifest_blob = json.dumps(manifest, sort_keys=True, indent=2).encode("utf-8") + b"\n"
    (out_dir / "manifest.json").write_bytes(manifest_blob)
    return manifest


# ── self-check ──────────────────────────────────────────────────────

def self_check(out_dir: Path, seed: int) -> int:
    failures: list[str] = []

    def check(name: str, ok: bool, detail: str = "") -> None:
        print(f"  {'ok  ' if ok else 'FAIL'} {name}{' — ' + detail if detail else ''}")
        if not ok:
            failures.append(name)

    print("grove W00 acceptance contract self-check")

    # 1. same seed → same bytes (rebuild in memory, compare digests)
    first = generate(out_dir, seed)
    again = generate(out_dir, seed)
    check("deterministic rebuild: identical digests",
          first["splits"] == again["splits"] and first["labels"] == again["labels"])

    # 2. a different seed must produce different data (guards a frozen stub)
    other = generate(out_dir, seed + 1)
    check("distinct seed: distinct data",
          other["splits"]["train"]["sha256"] != first["splits"]["train"]["sha256"])
    generate(out_dir, seed)  # restore the frozen artifacts

    rows = {name: read_rows((out_dir / "data" / f"{name}.bin").read_bytes()) for name, _ in SPLITS}

    # 3. group leakage: scene ids are disjoint across splits, and every
    #    derived variant of a scene stays in its own split
    groups = {name: {r["scene_id"] for r in rs} for name, rs in rows.items()}
    leakage = sum(len(groups[a] & groups[b]) for a, b in
                  (("train", "val"), ("train", "holdout"), ("val", "holdout")))
    check("group leakage across splits", leakage == 0, f"leakage={leakage}")
    per_scene = {name: {} for name, _ in SPLITS}
    for name, rs in rows.items():
        for r in rs:
            per_scene[name].setdefault(r["scene_id"], []).append(r)
    check("variants grouped under one split",
          all(len(v) == VARIANTS_PER_SCENE for m in per_scene.values() for v in m.values()))
    check("split sizes frozen at 512/128/256 scenes",
          [len(groups[n]) for n, _ in SPLITS] == [c for _, c in SPLITS],
          f"{[len(groups[n]) for n, _ in SPLITS]}")

    # 4. missing-modality contract: the label is undetermined, so the
    #    expected answer is abstain — never a negative label
    oracle = json.loads((out_dir / "eval" / "holdout_labels.json").read_text())["expected"]
    by_index = {r["index"]: r for r in rows["holdout"]}
    missing_rows = [r for rs in rows.values() for r in rs if not decidable(r)]
    check("missing-modality rows exist", len(missing_rows) > 0, f"n={len(missing_rows)}")
    holdout_missing = [r for r in rows["holdout"] if not decidable(r)]
    check("holdout oracle abstains exactly on missing-modality rows",
          bool(holdout_missing) and
          all((e["answer"] is None) == (not decidable(by_index[e["index"]])) for e in oracle),
          f"missing={len(holdout_missing)} abstain={sum(1 for e in oracle if e['answer'] is None)}")
    check("no missing-modality row is scored as class 0",
          all(e["answer"] != 0 for e in oracle
              if not decidable(by_index[e["index"]])))
    check("both modalities present on the rest",
          all(decidable(r) for rs in rows.values() for r in rs
              if not (r["flags"] & FLAG_MISSING)))

    # 5. the holdout container carries no label byte
    holdout_blob = (out_dir / "data" / "holdout.bin").read_bytes()
    check("holdout file stores no labels", all(r["label"] == LABEL_NONE for r in rows["holdout"]))
    check("holdout file is outside the data-only export",
          first["splits"]["holdout"]["labels_in_file"] is False and holdout_blob[:4] == b"GVD1")

    # 6. the XOR rule itself: over the labelled splits the label is the
    #    XOR of the figure class and the reading sign, so it is neither
    #    linearly separable in the signed readings nor a copy of either
    #    input. (The holdout split has no label byte; its answers live
    #    only in the oracle.)
    labelled = [r for name, rs in rows.items() if name != "holdout"
                for r in rs if decidable(r)]
    check("labels are binary and both classes present",
          {r["label"] for r in labelled} == {0, 1}, f"n={len(labelled)}")
    check("label is not the reading sign alone (XOR structure holds)",
          len({(r["label"] == (1 if sum(r["readings"]) < 0 else 0)) for r in labelled}) == 2)

    print(f"\ngroup leakage: {leakage}")
    print(f"samples: " + ", ".join(f"{n}={len(rows[n])}" for n, _ in SPLITS))
    if failures:
        print(f"\nSELF-CHECK FAILED: {', '.join(failures)}")
        return 1
    print("\nSELF-CHECK OK")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    here = Path(__file__).resolve().parent
    parser.add_argument("--out", default=str(here), help="output directory (default: this file's dir)")
    parser.add_argument("--seed", type=int, default=20261003, help="generation seed")
    parser.add_argument("--self-check", action="store_true", help="verify the frozen invariants")
    args = parser.parse_args()

    out_dir = Path(args.out)
    if args.self_check:
        return self_check(out_dir, args.seed)
    manifest = generate(out_dir, args.seed)
    print(f"wrote {out_dir}/manifest.json")
    for name, info in manifest["splits"].items():
        print(f"  {name:8s} scenes={info['scenes']:4d} samples={info['samples']:5d} "
              f"decidable={info['decidable']:5d} sha256={info['sha256'][:16]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
